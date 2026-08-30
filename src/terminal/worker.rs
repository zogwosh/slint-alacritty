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

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

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

fn publish_frame(latest_frame: &Mutex<Option<FramePatch>>, mut incoming: FramePatch) {
    let mut slot = latest_frame.lock().expect("latest frame mutex poisoned");
    let Some(mut pending) = slot.take() else {
        *slot = Some(incoming);
        return;
    };

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
}
