use super::{
    backend::TerminalBackend,
    command::{
        MouseAction, MouseButton, MouseInput, MouseScrollInput, TerminalSize, WorkerMessage,
    },
    frame::FramePatch,
    input::KeyInput,
    worker::run_worker,
};
use crate::app::settings::ShellProfile;
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

/// UI 层控制终端运行时的线程安全门面。
pub(crate) struct TerminalController {
    /// 所有输入和控制操作都通过该发送端交给工作线程。
    worker_sender: mpsc::Sender<WorkerMessage>,
    /// 单槽帧邮箱：UI 来不及消费时，工作线程会合并兼容的增量帧。
    latest_frame: Arc<Mutex<Option<FramePatch>>>,
    /// 防止同一批未消费帧重复唤醒 Slint 事件循环。
    notification_pending: Arc<AtomicBool>,
    worker_thread: Option<JoinHandle<()>>,
}

impl TerminalController {
    /// 创建终端后端并启动专属工作线程。
    pub(crate) fn new(
        columns: usize,
        rows: usize,
        cell_width: f32,
        cell_height: f32,
        profile: Option<&ShellProfile>,
        frame_notifier: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let (worker_sender, worker_receiver) = mpsc::channel();
        let backend = TerminalBackend::new(
            columns,
            rows,
            cell_width,
            cell_height,
            worker_sender.clone(),
            profile,
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

    /// 将 Slint 的整数按钮/动作编码转换为内部枚举后入队。
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

    pub(crate) fn resize(&self, columns: usize, rows: usize, cell_width: f32, cell_height: f32) {
        let _ = self
            .worker_sender
            .send(WorkerMessage::Resize(TerminalSize::new(
                columns,
                rows,
                cell_width,
                cell_height,
            )));
    }

    pub(crate) fn scroll_to(&self, display_offset: usize) {
        let _ = self
            .worker_sender
            .send(WorkerMessage::ScrollTo(display_offset));
    }

    /// 取走最近一帧，并允许工作线程再次发送 UI 唤醒通知。
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
        // 先通知循环退出再 join，确保 PTY 与子线程按顺序释放。
        let _ = self.worker_sender.send(WorkerMessage::Shutdown);
        if let Some(thread) = self.worker_thread.take() {
            let _ = thread.join();
        }
    }
}
