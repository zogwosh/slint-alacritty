//! 终端工作线程：串行处理命令，并以约 60Hz 的上限发布增量帧。

use super::{backend::TerminalBackend, command::WorkerMessage, frame::FramePatch};
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
    let mut last_frame = Instant::now() - FRAME_INTERVAL;
    backend.mark_dirty();

    loop {
        // 有待渲染内容时只等到下一帧时间；空闲时则无限等待新命令。
        let message = if frame_pending {
            let wait = FRAME_INTERVAL.saturating_sub(last_frame.elapsed());
            match receiver.recv_timeout(wait) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match receiver.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            }
        };

        match message {
            Some(WorkerMessage::Input {
                input,
                control,
                alt,
                shift,
                altgr,
            }) => backend.send_key(input, control, alt, shift, altgr),
            Some(WorkerMessage::Resize(size)) => {
                backend.resize(size);
                frame_pending = true;
            }
            Some(WorkerMessage::PasteClipboard) => {
                if let Some(text) = clipboard
                    .as_mut()
                    .and_then(|clipboard| clipboard.get_text().ok())
                {
                    backend.paste(&text);
                }
            }
            Some(WorkerMessage::ClipboardStore(text)) if text.len() <= 1024 * 1024 => {
                if let Some(clipboard) = &mut clipboard {
                    let _ = clipboard.set_text(text);
                }
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
            Some(WorkerMessage::Mouse(input)) => backend.mouse_input(input),
            Some(WorkerMessage::MouseScroll(input)) => backend.mouse_scroll(input),
            Some(WorkerMessage::ScrollTo(display_offset)) => backend.scroll_to(display_offset),
            Some(WorkerMessage::CopySelection) => {
                if let Some(text) = backend.selected_text().filter(|text| !text.is_empty())
                    && let Some(clipboard) = &mut clipboard
                {
                    let _ = clipboard.set_text(text);
                }
            }
            Some(WorkerMessage::SelectAll) => backend.select_all(),
            Some(WorkerMessage::ForceFullRedraw) => {
                backend.request_full_redraw();
                frame_pending = true;
            }
            Some(WorkerMessage::Render) => frame_pending = true,
            Some(WorkerMessage::Shutdown) => break,
            None => {}
        }

        if frame_pending && last_frame.elapsed() >= FRAME_INTERVAL {
            if let Some(frame) = backend.take_frame() {
                publish_frame(&latest_frame, frame);
                if !notification_pending.swap(true, Ordering::AcqRel) {
                    frame_notifier();
                }
            }
            last_frame = Instant::now();
            frame_pending = false;
        }
    }
}

/// 把新帧写入单槽邮箱；同一世代和尺寸的未消费增量帧按行合并。
fn publish_frame(latest_frame: &Mutex<Option<FramePatch>>, mut incoming: FramePatch) {
    let mut slot = latest_frame.lock().expect("latest frame mutex poisoned");
    let Some(mut pending) = slot.take() else {
        *slot = Some(incoming);
        return;
    };

    // 尺寸或世代不一致时，旧补丁已经没有意义，直接以新帧替换。
    if pending.generation != incoming.generation
        || pending.columns != incoming.columns
        || pending.rows != incoming.rows
    {
        *slot = Some(incoming);
        return;
    }

    for row in incoming.changed_rows.drain(..) {
        if let Some(existing) = pending
            .changed_rows
            .iter_mut()
            .find(|existing| existing.row == row.row)
        {
            *existing = row;
        } else {
            pending.changed_rows.push(row);
        }
    }
    pending.changed_rows.sort_unstable_by_key(|row| row.row);
    pending.full_redraw |= incoming.full_redraw;
    if incoming.full_redraw_reason.is_some() {
        pending.full_redraw_reason = incoming.full_redraw_reason;
    }
    pending.cursor = incoming.cursor;
    pending.scroll_offset = incoming.scroll_offset;
    pending.scroll_history_lines = incoming.scroll_history_lines;
    if incoming.title.is_some() {
        pending.title = incoming.title;
    }
    if incoming.exit_message.is_some() {
        pending.exit_message = incoming.exit_message;
    }
    *slot = Some(pending);
}

#[cfg(test)]
mod tests {
    use super::publish_frame;
    use crate::terminal::frame::{CursorPatch, FramePatch, RowPatch};
    use std::sync::Mutex;

    fn frame(generation: u64, changed_rows: &[usize]) -> FramePatch {
        FramePatch {
            generation,
            columns: 80,
            rows: 24,
            full_redraw: false,
            full_redraw_reason: None,
            changed_rows: changed_rows
                .iter()
                .map(|row| RowPatch {
                    row: *row,
                    cells: Vec::new(),
                })
                .collect(),
            cursor: CursorPatch::default(),
            scroll_offset: 0,
            scroll_history_lines: 0,
            title: None,
            exit_message: None,
        }
    }

    #[test]
    fn mailbox_merges_unconsumed_incremental_rows() {
        let slot = Mutex::new(None);
        publish_frame(&slot, frame(0, &[1, 3]));
        publish_frame(&slot, frame(0, &[2, 3]));

        let frame = slot.lock().unwrap().take().unwrap();
        let rows = frame
            .changed_rows
            .iter()
            .map(|row| row.row)
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![1, 2, 3]);
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
}
