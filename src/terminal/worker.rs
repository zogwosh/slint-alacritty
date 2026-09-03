//! 终端工作线程：串行处理命令，并以约 60Hz 的上限发布增量帧。

use super::{
    backend::TerminalBackend,
    command::{MouseEffect, WorkerMessage},
    frame::FramePatch,
};
use arboard::Clipboard;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

/// 一帧最短间隔；连续 PTY 事件会在该窗口内合并。
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// 查询输入停止后再执行全量搜索，避免每个按键都扫描整段滚动历史。
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(130);
/// 邮箱中累积的行补丁超过“整屏行数 × 该倍数”时放弃合并，改为请求一次完整重绘。
const MAILBOX_ROW_PATCH_FACTOR: usize = 8;
/// 终端程序通过 OSC 52 写入剪贴板的最大字节数，防止不可信输出填满系统剪贴板。
const OSC52_CLIPBOARD_LIMIT: usize = 1024 * 1024;

/// 工作线程主循环。没有待渲染内容时会阻塞等待消息，避免空转。
pub(super) fn run_worker(
    mut backend: TerminalBackend,
    receiver: Receiver<WorkerMessage>,
    latest_frame: Arc<Mutex<Option<FramePatch>>>,
    notification_pending: Arc<AtomicBool>,
    frame_notifier: Arc<dyn Fn() + Send + Sync>,
) {
    let mut clipboard = Clipboard::new().ok();
    let mut frame_pending = true;
    let mut active = true;
    let mut last_frame = Instant::now() - FRAME_INTERVAL;
    let mut pending_search: Option<(String, Instant)> = None;
    backend.mark_dirty();

    loop {
        // 有待渲染内容或待执行搜索时只等到最近的截止时间；空闲时则无限等待新命令。
        let now = Instant::now();
        let frame_wait = frame_pending.then(|| FRAME_INTERVAL.saturating_sub(last_frame.elapsed()));
        let search_wait = pending_search
            .as_ref()
            .map(|(_, deadline)| deadline.saturating_duration_since(now));
        let wait = match (frame_wait, search_wait) {
            (Some(frame), Some(search)) => Some(frame.min(search)),
            (Some(wait), None) | (None, Some(wait)) => Some(wait),
            (None, None) => None,
        };
        let message = match wait {
            Some(wait) => match receiver.recv_timeout(wait) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match receiver.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            },
        };

        // 即使 PTY 的 Render 唤醒因队列已满而被合并，消费任一命令也会检查 dirty 状态。
        if message.is_some() {
            frame_pending = true;
        }
        match message {
            Some(WorkerMessage::Input {
                input,
                control,
                alt,
                shift,
                altgr,
            }) => backend.send_key(input, control, alt, shift, altgr),
            Some(WorkerMessage::Resize(command)) => {
                let size = {
                    let mut command = command.lock().expect("pending resize mutex poisoned");
                    command.queued = false;
                    command.value.take()
                };
                if let Some(size) = size {
                    backend.resize(size);
                    frame_pending = true;
                }
            }
            Some(WorkerMessage::PasteClipboard) => paste_clipboard(&backend, &mut clipboard),
            // OSC 52 来自终端程序（可能是远端），只有它需要限长；本地选区复制是用户主动行为，不设上限。
            Some(WorkerMessage::ClipboardStore(text)) if text.len() <= OSC52_CLIPBOARD_LIMIT => {
                copy_to_clipboard(&mut clipboard, text);
            }
            Some(WorkerMessage::ClipboardStore(_)) => {}
            Some(WorkerMessage::ClipboardLoad(formatter)) => {
                if let Some(text) = clipboard
                    .as_mut()
                    .and_then(|clipboard| clipboard.get_text().ok())
                {
                    backend.send_to_pty(formatter(&text));
                }
            }
            Some(WorkerMessage::ColorRequest(index, formatter)) => {
                backend.respond_color_request(index, formatter);
            }
            Some(WorkerMessage::Mouse(input)) => match backend.mouse_input(input) {
                MouseEffect::None => {}
                MouseEffect::CopyToClipboard(text) => copy_to_clipboard(&mut clipboard, text),
                MouseEffect::PasteFromClipboard => paste_clipboard(&backend, &mut clipboard),
            },
            Some(WorkerMessage::MouseScroll(input)) => backend.mouse_scroll(input),
            Some(WorkerMessage::ScrollTo(command)) => {
                let display_offset = {
                    let mut command = command.lock().expect("pending scroll mutex poisoned");
                    command.queued = false;
                    command.value.take()
                };
                if let Some(display_offset) = display_offset {
                    backend.scroll_to(display_offset);
                    frame_pending = true;
                }
            }
            Some(WorkerMessage::CopySelection) => {
                if let Some(text) = backend.selected_text().filter(|text| !text.is_empty()) {
                    copy_to_clipboard(&mut clipboard, text);
                }
            }
            Some(WorkerMessage::SelectAll) => backend.select_all(),
            Some(WorkerMessage::Search(query)) => {
                // 清空查询要立即生效，让关闭查找框时高亮马上消失。
                if query.is_empty() {
                    pending_search = None;
                    backend.update_search(query);
                    frame_pending = true;
                } else {
                    pending_search = Some((query, Instant::now() + SEARCH_DEBOUNCE));
                }
            }
            Some(WorkerMessage::SearchStep(previous)) => {
                if let Some((query, _)) = pending_search.take() {
                    backend.update_search(query);
                }
                backend.search_step(previous);
                frame_pending = true;
            }
            Some(WorkerMessage::ForceFullRedraw) => {
                backend.request_full_redraw();
                frame_pending = true;
            }
            Some(WorkerMessage::SetOptions(options)) => {
                backend.set_options(options);
                frame_pending = true;
            }
            Some(WorkerMessage::SetActive(value)) => {
                active = value;
                frame_pending = true;
            }
            Some(WorkerMessage::Render) => frame_pending = true,
            Some(WorkerMessage::Shutdown) => break,
            None => {}
        }

        if pending_search
            .as_ref()
            .is_some_and(|(_, deadline)| Instant::now() >= *deadline)
            && let Some((query, _)) = pending_search.take()
        {
            backend.update_search(query);
            frame_pending = true;
        }

        if frame_pending && last_frame.elapsed() >= FRAME_INTERVAL {
            let frame = if active {
                backend.take_frame()
            } else {
                backend.take_metadata_frame()
            };
            let mut overflowed = false;
            if let Some(frame) = frame {
                overflowed = publish_frame(&latest_frame, frame) == MailboxState::Overflowed;
                if !notification_pending.swap(true, Ordering::AcqRel) {
                    frame_notifier();
                }
            }
            last_frame = Instant::now();
            // UI 长时间未消费时不再无界累积增量行，改用下一帧完整画面收敛。
            if overflowed {
                backend.request_full_redraw();
            }
            frame_pending = overflowed;
        }
    }
}

fn paste_clipboard(backend: &TerminalBackend, clipboard: &mut Option<Clipboard>) {
    if let Some(text) = clipboard
        .as_mut()
        .and_then(|clipboard| clipboard.get_text().ok())
    {
        backend.paste(&text);
    }
}

/// 系统剪贴板不可用（如无桌面会话）时静默忽略，与其他剪贴板路径一致。
fn copy_to_clipboard(clipboard: &mut Option<Clipboard>, text: String) {
    if let Some(clipboard) = clipboard {
        let _ = clipboard.set_text(text);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MailboxState {
    Merged,
    Overflowed,
}

/// 把新帧写入单槽邮箱；列补丁保持捕获顺序，避免同一行的独立区间互相覆盖。
/// 累积行数超过上限时丢弃全部行补丁并返回 Overflowed，由调用方请求完整重绘。
fn publish_frame(latest_frame: &Mutex<Option<FramePatch>>, mut incoming: FramePatch) -> MailboxState {
    let mut slot = latest_frame.lock().expect("latest frame mutex poisoned");
    let Some(mut pending) = slot.take() else {
        *slot = Some(incoming);
        return MailboxState::Merged;
    };

    // 后台元数据帧只携带标题/退出信息，不能覆盖已有的视口状态。
    if incoming.metadata_only {
        merge_metadata(&mut pending, &mut incoming);
        *slot = Some(pending);
        return MailboxState::Merged;
    }
    if pending.metadata_only {
        merge_metadata(&mut incoming, &mut pending);
        *slot = Some(incoming);
        return MailboxState::Merged;
    }

    // 尺寸或世代不一致时，旧补丁已经没有意义，直接以新帧替换；标题/退出信息不能丢。
    if pending.generation != incoming.generation
        || pending.columns != incoming.columns
        || pending.rows != incoming.rows
        || incoming.full_redraw
    {
        if incoming.title.is_none() {
            incoming.title = pending.title.take();
        }
        if incoming.exit_message.is_none() {
            incoming.exit_message = pending.exit_message.take();
        }
        *slot = Some(incoming);
        return MailboxState::Merged;
    }

    let limit = pending.rows.max(1) * MAILBOX_ROW_PATCH_FACTOR;
    let overflowed = pending.changed_rows.len() + incoming.changed_rows.len() > limit;
    if overflowed {
        pending.changed_rows.clear();
        pending.full_redraw = false;
        pending.full_redraw_reason = None;
    } else {
        pending.changed_rows.append(&mut incoming.changed_rows);
    }
    pending.cursor = incoming.cursor;
    pending.scroll_offset = incoming.scroll_offset;
    pending.scroll_history_lines = incoming.scroll_history_lines;
    pending.search = std::mem::take(&mut incoming.search);
    pending.decorations = std::mem::take(&mut incoming.decorations);
    merge_metadata(&mut pending, &mut incoming);
    *slot = Some(pending);
    if overflowed {
        MailboxState::Overflowed
    } else {
        MailboxState::Merged
    }
}

fn merge_metadata(target: &mut FramePatch, source: &mut FramePatch) {
    if source.title.is_some() {
        target.title = source.title.take();
    }
    if source.exit_message.is_some() {
        target.exit_message = source.exit_message.take();
    }
}

#[cfg(test)]
mod tests {
    use super::{MAILBOX_ROW_PATCH_FACTOR, MailboxState, publish_frame};
    use crate::terminal::frame::{CursorPatch, FramePatch, RowPatch};
    use std::sync::Mutex;

    fn frame(generation: u64, changed_rows: &[usize]) -> FramePatch {
        FramePatch {
            generation,
            columns: 80,
            rows: 24,
            full_redraw: false,
            full_redraw_reason: None,
            metadata_only: false,
            changed_rows: changed_rows
                .iter()
                .map(|row| RowPatch {
                    row: *row,
                    start_column: 0,
                    end_column: 80,
                    cells: Vec::new(),
                })
                .collect(),
            cursor: CursorPatch::default(),
            scroll_offset: 0,
            scroll_history_lines: 0,
            search: crate::terminal::SearchSnapshot::default(),
            decorations: Vec::new(),
            title: None,
            exit_message: None,
        }
    }

    #[test]
    fn mailbox_preserves_incremental_patch_order() {
        let slot = Mutex::new(None);
        publish_frame(&slot, frame(0, &[1, 3]));
        publish_frame(&slot, frame(0, &[2, 3]));

        let frame = slot.lock().unwrap().take().unwrap();
        let rows = frame
            .changed_rows
            .iter()
            .map(|row| row.row)
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![1, 3, 2, 3]);
    }

    #[test]
    fn mailbox_discards_frames_from_an_old_generation() {
        let slot = Mutex::new(None);
        publish_frame(&slot, frame(0, &[1]));
        publish_frame(&slot, frame(1, &[2]));

        let frame = slot.lock().unwrap().take().unwrap();
        assert_eq!(frame.generation, 1);
        assert_eq!(frame.changed_rows[0].row, 2);
    }

    #[test]
    fn mailbox_retains_exit_status_while_merging() {
        let slot = Mutex::new(None);
        publish_frame(&slot, frame(0, &[1]));
        let mut exited = frame(0, &[2]);
        exited.exit_message = Some("Process exited with code 7".into());
        publish_frame(&slot, exited);

        assert_eq!(
            slot.lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .exit_message
                .as_deref(),
            Some("Process exited with code 7")
        );
    }

    #[test]
    fn mailbox_retains_latest_full_redraw_reason() {
        use crate::terminal::FullRedrawReason;

        let slot = Mutex::new(None);
        let mut full = frame(0, &[0]);
        full.full_redraw = true;
        full.full_redraw_reason = Some(FullRedrawReason::Resize);
        publish_frame(&slot, full);
        publish_frame(&slot, frame(0, &[1]));

        assert_eq!(
            slot.lock().unwrap().as_ref().unwrap().full_redraw_reason,
            Some(FullRedrawReason::Resize)
        );
    }

    #[test]
    fn mailbox_retains_latest_scrollbar_state() {
        let slot = Mutex::new(None);
        let mut first = frame(0, &[1]);
        first.scroll_offset = 2;
        first.scroll_history_lines = 40;
        publish_frame(&slot, first);

        let mut latest = frame(0, &[2]);
        latest.scroll_offset = 17;
        latest.scroll_history_lines = 64;
        publish_frame(&slot, latest);

        let frame = slot.lock().unwrap().take().unwrap();
        assert_eq!(frame.scroll_offset, 17);
        assert_eq!(frame.scroll_history_lines, 64);
    }

    #[test]
    fn mailbox_stops_accumulating_rows_and_asks_for_a_full_redraw() {
        let slot = Mutex::new(None);
        let rows = (0..24 * MAILBOX_ROW_PATCH_FACTOR).map(|row| row % 24).collect::<Vec<_>>();
        assert_eq!(publish_frame(&slot, frame(0, &rows)), MailboxState::Merged);
        let mut overflow = frame(0, &[3]);
        overflow.title = Some("busy".into());
        assert_eq!(publish_frame(&slot, overflow), MailboxState::Overflowed);

        let frame = slot.lock().unwrap().take().unwrap();
        assert!(frame.changed_rows.is_empty());
        assert!(!frame.full_redraw);
        assert_eq!(frame.title.as_deref(), Some("busy"));
    }

    #[test]
    fn metadata_only_frames_never_replace_viewport_state() {
        let slot = Mutex::new(None);
        let mut pending = frame(0, &[1]);
        pending.scroll_offset = 5;
        publish_frame(&slot, pending);

        let mut metadata = frame(0, &[]);
        metadata.metadata_only = true;
        metadata.exit_message = Some("Process exited".into());
        publish_frame(&slot, metadata);

        let frame = slot.lock().unwrap().take().unwrap();
        assert!(!frame.metadata_only);
        assert_eq!(frame.scroll_offset, 5);
        assert_eq!(frame.changed_rows.len(), 1);
        assert_eq!(frame.exit_message.as_deref(), Some("Process exited"));
    }

    #[test]
    fn full_frame_replacement_keeps_pending_metadata() {
        let slot = Mutex::new(None);
        let mut pending = frame(0, &[1]);
        pending.title = Some("title".into());
        publish_frame(&slot, pending);
        let mut full = frame(0, &[0]);
        full.full_redraw = true;
        publish_frame(&slot, full);

        let frame = slot.lock().unwrap().take().unwrap();
        assert!(frame.full_redraw);
        assert_eq!(frame.title.as_deref(), Some("title"));
    }

    #[test]
    fn mailbox_retains_latest_search_state() {
        let slot = Mutex::new(None);
        let mut first = frame(0, &[1]);
        first.search.query = "old".to_owned();
        first.search.current = 1;
        first.search.total = 2;
        publish_frame(&slot, first);

        let mut latest = frame(0, &[2]);
        latest.search.query = "cargo".to_owned();
        latest.search.current = 3;
        latest.search.total = 12;
        publish_frame(&slot, latest);

        let frame = slot.lock().unwrap().take().unwrap();
        assert_eq!(frame.search.query, "cargo");
        assert_eq!(frame.search.current, 3);
        assert_eq!(frame.search.total, 12);
    }
}
