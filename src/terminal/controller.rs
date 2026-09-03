use super::{
    backend::TerminalBackend,
    command::{
        CoalescedCommand, MouseAction, MouseButton, MouseInput, MouseScrollInput, TerminalSize,
        WorkerMessage,
    },
    frame::FramePatch,
    input::KeyInput,
    palette::TerminalTheme,
    worker::run_worker,
};
use crate::app::settings::ShellProfile;
use std::{
    cell::Cell,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
};

/// UI 层控制终端运行时的线程安全门面。
pub(crate) struct TerminalController {
    /// 所有输入和控制操作都通过该发送端交给工作线程。
    worker_sender: SyncSender<WorkerMessage>,
    /// 单槽帧邮箱：UI 来不及消费时，工作线程会合并兼容的增量帧。
    latest_frame: Arc<Mutex<Option<FramePatch>>>,
    /// 防止同一批未消费帧重复唤醒 Slint 事件循环。
    notification_pending: Arc<AtomicBool>,
    pending_resize: Arc<Mutex<CoalescedCommand<TerminalSize>>>,
    pending_scroll: Arc<Mutex<CoalescedCommand<usize>>>,
    /// 上次通知给工作线程的显示状态，避免每次标签同步都重复入队。
    active: Cell<bool>,
    worker_thread: Option<JoinHandle<()>>,
}

impl TerminalController {
    /// 创建终端后端并启动专属工作线程。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        columns: usize,
        rows: usize,
        cell_width: f32,
        cell_height: f32,
        profile: Option<&ShellProfile>,
        theme: TerminalTheme,
        default_title: String,
        frame_notifier: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        // 有界队列为 UI/PTY 突发流量提供背压；可合并命令不会按事件数增长。
        let (worker_sender, worker_receiver) = mpsc::sync_channel(512);
        let backend = TerminalBackend::new(
            columns,
            rows,
            cell_width,
            cell_height,
            worker_sender.clone(),
            profile,
            theme,
            default_title,
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
        let pending_resize = Arc::new(Mutex::new(CoalescedCommand::default()));
        let pending_scroll = Arc::new(Mutex::new(CoalescedCommand::default()));

        Ok(Self {
            worker_sender,
            latest_frame,
            notification_pending,
            pending_resize,
            pending_scroll,
            active: Cell::new(true),
            worker_thread: Some(worker_thread),
        })
    }

    /// 告知工作线程该会话是否正在显示；后台会话停止网格捕获，只上报标题与退出状态。
    pub(crate) fn set_active(&self, active: bool) {
        if self.active.replace(active) != active {
            let _ = self.worker_sender.send(WorkerMessage::SetActive(active));
        }
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

    pub(crate) fn update_search(&self, query: String) {
        let _ = self.worker_sender.send(WorkerMessage::Search(query));
    }

    pub(crate) fn search_step(&self, previous: bool) {
        let _ = self.worker_sender.send(WorkerMessage::SearchStep(previous));
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
        let message = WorkerMessage::Mouse(MouseInput {
            column,
            row,
            button,
            action,
            shift,
            alt,
            control,
            right_half,
        });
        if action == MouseAction::Move {
            // 指针移动可由后续位置取代，绝不能让它阻塞 UI 事件循环。
            match self.worker_sender.try_send(message) {
                Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
            }
        } else {
            let _ = self.worker_sender.send(message);
        }
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
        let mut pending = self
            .pending_resize
            .lock()
            .expect("pending resize mutex poisoned");
        pending.value = Some(TerminalSize::new(columns, rows, cell_width, cell_height));
        if !pending.queued {
            pending.queued = true;
            let command = self.pending_resize.clone();
            drop(pending);
            let _ = self.worker_sender.send(WorkerMessage::Resize(command));
        }
    }

    pub(crate) fn scroll_to(&self, display_offset: usize) {
        let mut pending = self
            .pending_scroll
            .lock()
            .expect("pending scroll mutex poisoned");
        pending.value = Some(display_offset);
        if !pending.queued {
            pending.queued = true;
            let command = self.pending_scroll.clone();
            drop(pending);
            let _ = self.worker_sender.send(WorkerMessage::ScrollTo(command));
        }
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
