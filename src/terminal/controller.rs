use super::{
    backend::TerminalBackend,
    command::{
        MouseAction, MouseButton, MouseInput, MouseScrollInput, TerminalSize, WorkerMessage,
    },
    frame::FramePatch,
    input::KeyInput,
    worker::run_worker,
};
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

/// Thread-safe facade used by the UI layer to control the terminal runtime.
pub(crate) struct TerminalController {
    worker_sender: mpsc::Sender<WorkerMessage>,
    latest_frame: Arc<Mutex<Option<FramePatch>>>,
    notification_pending: Arc<AtomicBool>,
    worker_thread: Option<JoinHandle<()>>,
}

impl TerminalController {
    pub(crate) fn new(
        columns: usize,
        rows: usize,
        cell_width: f32,
        cell_height: f32,
        frame_notifier: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let (worker_sender, worker_receiver) = mpsc::channel();
        let backend = TerminalBackend::new(
            columns,
            rows,
            cell_width,
            cell_height,
            worker_sender.clone(),
        )?;
        let latest_frame = Arc::new(Mutex::new(None));
        let worker_frame = latest_frame.clone();
        let notification_pending = Arc::new(AtomicBool::new(false));
        let worker_notification_pending = notification_pending.clone();
        let frame_notifier = Arc::new(frame_notifier);
        let worker_thread = thread::spawn(move || {
            run_worker(
                backend,
                worker_receiver,
                worker_frame,
                worker_notification_pending,
                frame_notifier,
            )
        });

        Ok(Self {
            worker_sender,
            latest_frame,
            notification_pending,
            worker_thread: Some(worker_thread),
        })
    }

    pub(crate) fn send_key(
        &self,
        input: KeyInput,
        control: bool,
        alt: bool,
        shift: bool,
        altgr: bool,
    ) {
        let _ = self.worker_sender.send(WorkerMessage::Input {
            input,
            control,
            alt,
            shift,
            altgr,
        });
    }

    pub(crate) fn paste_clipboard(&self) {
        let _ = self.worker_sender.send(WorkerMessage::PasteClipboard);
    }

    pub(crate) fn copy_selection(&self) {
        let _ = self.worker_sender.send(WorkerMessage::CopySelection);
    }

    pub(crate) fn select_all(&self) {
        let _ = self.worker_sender.send(WorkerMessage::SelectAll);
    }

    pub(crate) fn request_full_redraw(&self) {
        let _ = self.worker_sender.send(WorkerMessage::ForceFullRedraw);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mouse_input(
        &self,
        column: usize,
        row: usize,
        button: i32,
        action: i32,
        shift: bool,
        alt: bool,
        control: bool,
        right_half: bool,
    ) {
        let button = match button {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::Other,
        };
        let action = match action {
            0 => MouseAction::Press,
            1 => MouseAction::Release,
            2 => MouseAction::Move,
            4 => MouseAction::DoubleClick,
            _ => MouseAction::Cancel,
        };
        let _ = self.worker_sender.send(WorkerMessage::Mouse(MouseInput {
            column,
            row,
            button,
            action,
            shift,
            alt,
            control,
            right_half,
        }));
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mouse_scroll(
        &self,
        column: usize,
        row: usize,
        lines: f32,
        shift: bool,
        alt: bool,
        control: bool,
    ) {
        let _ = self
            .worker_sender
            .send(WorkerMessage::MouseScroll(MouseScrollInput {
                column,
                row,
                lines,
                shift,
                alt,
                control,
            }));
    }

    pub(crate) fn resize(&self, columns: usize, rows: usize) {
        let _ = self
            .worker_sender
            .send(WorkerMessage::Resize(TerminalSize { columns, rows }));
    }

    pub(crate) fn take_latest_frame(&self) -> Option<FramePatch> {
        self.notification_pending.store(false, Ordering::Release);
        self.latest_frame
            .lock()
            .expect("latest frame mutex poisoned")
            .take()
    }
}

impl Drop for TerminalController {
    fn drop(&mut self) {
        let _ = self.worker_sender.send(WorkerMessage::Shutdown);
        if let Some(thread) = self.worker_thread.take() {
            let _ = thread.join();
        }
    }
}
