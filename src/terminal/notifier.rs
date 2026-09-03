//! Alacritty 事件到终端工作线程和 PTY 的跨线程通知适配。

use super::command::WorkerMessage;
use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoopSender, Msg},
};
use std::{
    borrow::Cow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
};

/// Alacritty 事件监听器的可克隆句柄。
#[derive(Clone)]
pub(super) struct Notifier {
    state: Arc<NotificationState>,
}

/// 跨 PTY 事件线程与终端工作线程共享的通知状态。
struct NotificationState {
    /// dirty 从 false 变为 true 时只投递一次 Render，避免消息队列被唤醒事件淹没。
    dirty: AtomicBool,
    /// 会话在程序未设置标题时显示的名字；`ResetTitle` 回到这里而不是某个全局字符串。
    default_title: String,
    title: Mutex<Option<String>>,
    exit_message: Mutex<Option<String>>,
    /// 构造事件循环后才能取得发送端，因此初始化阶段允许为空。
    pty_sender: Mutex<Option<EventLoopSender>>,
    worker_sender: SyncSender<WorkerMessage>,
    window_size: Mutex<WindowSize>,
}

impl Notifier {
    pub(super) fn new(
        window_size: WindowSize,
        worker_sender: SyncSender<WorkerMessage>,
        default_title: String,
    ) -> Self {
        Self {
            state: Arc::new(NotificationState {
                dirty: AtomicBool::new(false),
                default_title,
                title: Mutex::new(None),
                exit_message: Mutex::new(None),
                pty_sender: Mutex::new(None),
                worker_sender,
                window_size: Mutex::new(window_size),
            }),
        }
    }

    pub(super) fn set_pty_sender(&self, sender: EventLoopSender) {
        *self
            .state
            .pty_sender
            .lock()
            .expect("terminal sender mutex poisoned") = Some(sender);
    }

    pub(super) fn send_to_pty(&self, text: String) {
        let sender = self
            .state
            .pty_sender
            .lock()
            .expect("terminal sender mutex poisoned")
            .clone();
        if let Some(sender) = sender {
            let _ = sender.send(Msg::Input(Cow::Owned(text.into_bytes())));
        }
    }

    /// 原子地开始捕获一帧；返回 false 表示没有新变化。
    pub(super) fn begin_frame(&self) -> bool {
        self.state.dirty.swap(false, Ordering::AcqRel)
    }

    pub(super) fn take_title(&self) -> Option<String> {
        self.state
            .title
            .lock()
            .expect("terminal title mutex poisoned")
            .take()
    }

    pub(super) fn take_exit_message(&self) -> Option<String> {
        self.state
            .exit_message
            .lock()
            .expect("terminal exit mutex poisoned")
            .take()
    }

    fn set_exit_message(&self, message: String, overwrite: bool) {
        let mut exit_message = self
            .state
            .exit_message
            .lock()
            .expect("terminal exit mutex poisoned");
        if overwrite || exit_message.is_none() {
            *exit_message = Some(message);
        }
        drop(exit_message);
        self.mark_dirty();
    }

    pub(super) fn update_window_size(&self, size: WindowSize) {
        *self
            .state
            .window_size
            .lock()
            .expect("terminal size mutex poisoned") = size;
    }

    /// 标记终端已变化，并在首次变脏时唤醒工作线程。
    pub(super) fn mark_dirty(&self) {
        if !self.state.dirty.swap(true, Ordering::AcqRel) {
            // EventListener 可能持有终端锁，绝不能在有界队列满时阻塞。
            let _ = self.state.worker_sender.try_send(WorkerMessage::Render);
        }
    }
}

impl EventListener for Notifier {
    /// 接收 Alacritty 解析器/PTY 产生的标题、剪贴板、退出和重绘事件。
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => {
                *self
                    .state
                    .title
                    .lock()
                    .expect("terminal title mutex poisoned") = Some(title);
                self.mark_dirty();
            }
            // 程序清除标题，或 `Term::set_options` 在没有标题时重放状态，都回到会话默认名。
            Event::ResetTitle => {
                *self
                    .state
                    .title
                    .lock()
                    .expect("terminal title mutex poisoned") =
                    Some(self.state.default_title.clone());
                self.mark_dirty();
            }
            Event::PtyWrite(text) => self.send_to_pty(text),
            Event::TextAreaSizeRequest(formatter) => {
                let size = *self
                    .state
                    .window_size
                    .lock()
                    .expect("terminal size mutex poisoned");
                self.send_to_pty(formatter(size));
            }
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange | Event::Bell => {
                self.mark_dirty()
            }
            Event::Exit => self.set_exit_message("Process exited".into(), false),
            Event::ChildExit(status) => {
                let message = status.code().map_or_else(
                    || "Process terminated".into(),
                    |code| format!("Process exited with code {code}"),
                );
                self.set_exit_message(message, true);
            }
            Event::ClipboardStore(_, text) => {
                let _ = self
                    .state
                    .worker_sender
                    .try_send(WorkerMessage::ClipboardStore(text));
            }
            Event::ClipboardLoad(_, formatter) => {
                let _ = self
                    .state
                    .worker_sender
                    .try_send(WorkerMessage::ClipboardLoad(formatter));
            }
            Event::ColorRequest(index, formatter) => {
                let _ = self
                    .state
                    .worker_sender
                    .try_send(WorkerMessage::ColorRequest(index, formatter));
            }
        }
    }
}
