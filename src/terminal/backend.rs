use super::{
    command::{
        MouseAction, MouseButton, MouseInput, MouseScrollInput, TerminalSize, WorkerMessage,
    },
    frame::{FramePatch, capture_frame},
    input::{KeyInput, encode_key},
    mouse::{encode_mouse_button_code, encode_mouse_report, scroll_lines, visible_point},
};
use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::Scroll,
    index::Side,
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, Term},
    tty::{self, Options},
};
use std::{
    borrow::Cow,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::JoinHandle,
};

#[derive(Clone)]
pub(super) struct Notifier {
    state: Arc<NotificationState>,
}

struct NotificationState {
    dirty: AtomicBool,
    title: Mutex<Option<String>>,
    exit_message: Mutex<Option<String>>,
    pty_sender: Mutex<Option<EventLoopSender>>,
    worker_sender: Sender<WorkerMessage>,
    window_size: Mutex<WindowSize>,
}

impl Notifier {
    fn new(window_size: WindowSize, worker_sender: Sender<WorkerMessage>) -> Self {
        Self {
            state: Arc::new(NotificationState {
                dirty: AtomicBool::new(false),
                title: Mutex::new(None),
                exit_message: Mutex::new(None),
                pty_sender: Mutex::new(None),
                worker_sender,
                window_size: Mutex::new(window_size),
            }),
        }
    }

    fn set_pty_sender(&self, sender: EventLoopSender) {
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

    fn update_window_size(&self, size: WindowSize) {
        *self
            .state
            .window_size
            .lock()
            .expect("terminal size mutex poisoned") = size;
    }

    pub(super) fn mark_dirty(&self) {
        if !self.state.dirty.swap(true, Ordering::AcqRel) {
            let _ = self.state.worker_sender.send(WorkerMessage::Render);
        }
    }
}

impl EventListener for Notifier {
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
            Event::ResetTitle => {
                *self
                    .state
                    .title
                    .lock()
                    .expect("terminal title mutex poisoned") = Some("Slint Terminal".into());
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
                    .send(WorkerMessage::ClipboardStore(text));
            }
            Event::ClipboardLoad(_, formatter) => {
                let _ = self
                    .state
                    .worker_sender
                    .send(WorkerMessage::ClipboardLoad(formatter));
            }
            Event::ColorRequest(_, _) => {}
        }
    }
}

pub(super) struct TerminalBackend {
    terminal: Arc<FairMutex<Term<Notifier>>>,
    notifier: Notifier,
    pty_sender: EventLoopSender,
    event_thread: Option<
        JoinHandle<(
            EventLoop<tty::Pty, Notifier>,
            alacritty_terminal::event_loop::State,
        )>,
    >,
    size: TerminalSize,
    generation: u64,
    cell_width: u16,
    cell_height: u16,
    selecting: bool,
    pressed_button: Option<MouseButton>,
    force_full_redraw: bool,
}

impl TerminalBackend {
    pub(super) fn new(
        columns: usize,
        rows: usize,
        cell_width: f32,
        cell_height: f32,
        worker_sender: Sender<WorkerMessage>,
    ) -> io::Result<Self> {
        let size = TerminalSize { columns, rows };
        let cell_width = cell_width.ceil() as u16;
        let cell_height = cell_height.ceil() as u16;
        let window_size = window_size(size, cell_width, cell_height);
        let notifier = Notifier::new(window_size, worker_sender);
        let terminal = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            notifier.clone(),
        )));

        tty::setup_env();
        let options = Options::default();
        let pty = tty::new(&options, window_size, 0)?;
        let event_loop = EventLoop::new(
            terminal.clone(),
            notifier.clone(),
            pty,
            options.drain_on_exit,
            false,
        )?;
        let pty_sender = event_loop.channel();
        notifier.set_pty_sender(pty_sender.clone());
        let event_thread = event_loop.spawn();

        Ok(Self {
            terminal,
            notifier,
            pty_sender,
            event_thread: Some(event_thread),
            size,
            generation: 0,
            cell_width,
            cell_height,
            selecting: false,
            pressed_button: None,
            force_full_redraw: false,
        })
    }

    pub(super) fn take_frame(&mut self) -> Option<FramePatch> {
        if !self.notifier.begin_frame() {
            return None;
        }

        let force_full_redraw = std::mem::take(&mut self.force_full_redraw);
        let mut terminal = self.terminal.lock();
        let mut frame = capture_frame(
            &mut terminal,
            self.size.columns,
            self.size.rows,
            self.generation,
            force_full_redraw,
        );
        drop(terminal);
        frame.title = self.notifier.take_title();
        frame.exit_message = self.notifier.take_exit_message();
        Some(frame)
    }

    pub(super) fn mark_dirty(&self) {
        self.notifier.mark_dirty();
    }

    pub(super) fn request_full_redraw(&mut self) {
        self.force_full_redraw = true;
        self.notifier.mark_dirty();
    }

    pub(super) fn send_to_pty(&self, text: String) {
        self.notifier.send_to_pty(text);
    }

    pub(super) fn send_key(
        &self,
        input: KeyInput,
        control: bool,
        alt: bool,
        shift: bool,
        altgr: bool,
    ) {
        let mode = *self.terminal.lock().mode();
        let bytes = encode_key(input, control, alt, shift, altgr, mode);
        if !bytes.is_empty() {
            let _ = self.pty_sender.send(Msg::Input(Cow::Owned(bytes)));
        }
    }

    pub(super) fn paste(&self, text: &str) {
        let mode = *self.terminal.lock().mode();
        let bytes = encode_paste(
            text,
            mode.contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE),
        );
        let _ = self.pty_sender.send(Msg::Input(Cow::Owned(bytes)));
    }

    pub(super) fn resize(&mut self, size: TerminalSize) {
        if self.size == size {
            return;
        }

        self.terminal.lock().resize(size);
        let new_window_size = window_size(size, self.cell_width, self.cell_height);
        self.notifier.update_window_size(new_window_size);
        let _ = self.pty_sender.send(Msg::Resize(new_window_size));
        self.size = size;
        self.generation = self.generation.wrapping_add(1);
        self.notifier.mark_dirty();
    }

    pub(super) fn mouse_input(&mut self, input: MouseInput) {
        let mut terminal = self.terminal.lock();
        let mode = *terminal.mode();
        // Keep a drag owned by whichever side received its initial press. This avoids
        // switching between local selection and application reporting mid-drag when
        // Shift is pressed or released.
        let report_to_application = mode.intersects(alacritty_terminal::term::TermMode::MOUSE_MODE)
            && !self.selecting
            && (self.pressed_button.is_some() || !input.shift);

        if report_to_application {
            let should_report = match input.action {
                MouseAction::Press | MouseAction::Release => true,
                MouseAction::Move => {
                    mode.contains(alacritty_terminal::term::TermMode::MOUSE_MOTION)
                        || (mode.contains(alacritty_terminal::term::TermMode::MOUSE_DRAG)
                            && self.pressed_button.is_some())
                }
                MouseAction::Cancel | MouseAction::DoubleClick => false,
            };
            let report_button = if input.action == MouseAction::Release {
                self.pressed_button.unwrap_or(input.button)
            } else {
                input.button
            };
            match input.action {
                MouseAction::Press => self.pressed_button = Some(input.button),
                MouseAction::Release | MouseAction::Cancel => self.pressed_button = None,
                MouseAction::Move | MouseAction::DoubleClick => {}
            }
            let report = should_report.then(|| {
                encode_mouse_report(
                    report_button,
                    input.action,
                    input.column,
                    input.row,
                    input.shift,
                    input.alt,
                    input.control,
                    mode,
                )
            });
            drop(terminal);
            if let Some(bytes) = report.filter(|bytes| !bytes.is_empty()) {
                let _ = self.pty_sender.send(Msg::Input(Cow::Owned(bytes)));
            }
            return;
        }

        let point = visible_point(&terminal, self.size, input.column, input.row);
        let side = if input.right_half {
            Side::Right
        } else {
            Side::Left
        };
        match (input.button, input.action) {
            (MouseButton::Left, MouseAction::Press) => {
                terminal.selection = Some(Selection::new(SelectionType::Simple, point, side));
                self.selecting = true;
            }
            (MouseButton::Left, MouseAction::DoubleClick) => {
                terminal.selection =
                    Some(Selection::new(SelectionType::Semantic, point, Side::Left));
                self.selecting = false;
            }
            (_, MouseAction::Move) if self.selecting => {
                if let Some(selection) = &mut terminal.selection {
                    selection.update(point, side);
                }
            }
            (MouseButton::Left, MouseAction::Release) => {
                if let Some(selection) = &mut terminal.selection {
                    selection.update(point, side);
                }
                self.selecting = false;
            }
            (_, MouseAction::Cancel) => self.selecting = false,
            _ => return,
        }
        self.force_full_redraw = true;
        drop(terminal);
        self.notifier.mark_dirty();
    }

    pub(super) fn mouse_scroll(&mut self, input: MouseScrollInput) {
        let lines = scroll_lines(input.lines);
        if lines == 0 {
            return;
        }

        let mut terminal = self.terminal.lock();
        let mode = *terminal.mode();
        if mode.intersects(alacritty_terminal::term::TermMode::MOUSE_MODE) && !input.shift {
            drop(terminal);
            let button = if lines > 0 { 64 } else { 65 };
            let report = encode_mouse_button_code(
                button,
                input.column,
                input.row,
                input.shift,
                input.alt,
                input.control,
                mode,
                false,
            );
            for _ in 0..lines.unsigned_abs().min(20) {
                let _ = self.pty_sender.send(Msg::Input(Cow::Owned(report.clone())));
            }
        } else if mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
            && mode.contains(alacritty_terminal::term::TermMode::ALTERNATE_SCROLL)
        {
            let app_cursor = mode.contains(alacritty_terminal::term::TermMode::APP_CURSOR);
            drop(terminal);
            let sequence = match (lines > 0, app_cursor) {
                (true, true) => b"\x1bOA".as_slice(),
                (true, false) => b"\x1b[A".as_slice(),
                (false, true) => b"\x1bOB".as_slice(),
                (false, false) => b"\x1b[B".as_slice(),
            };
            for _ in 0..lines.unsigned_abs().min(20) {
                let _ = self
                    .pty_sender
                    .send(Msg::Input(Cow::Owned(sequence.to_vec())));
            }
        } else {
            terminal.scroll_display(Scroll::Delta(lines));
        }
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        self.terminal.lock().selection_to_string()
    }
}

impl Drop for TerminalBackend {
    fn drop(&mut self) {
        let _ = self.pty_sender.send(Msg::Shutdown);
        if let Some(thread) = self.event_thread.take() {
            let _ = thread.join();
        }
    }
}

fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if bracketed {
        let safe_text = normalized.replace('\x1b', "");
        format!("\x1b[200~{safe_text}\x1b[201~").into_bytes()
    } else {
        normalized.replace('\n', "\r").into_bytes()
    }
}

fn window_size(size: TerminalSize, cell_width: u16, cell_height: u16) -> WindowSize {
    WindowSize {
        num_lines: size.rows.min(u16::MAX as usize) as u16,
        num_cols: size.columns.min(u16::MAX as usize) as u16,
        cell_width,
        cell_height,
    }
}

#[cfg(test)]
mod tests {
    use super::encode_paste;

    #[test]
    fn paste_normalizes_lines_and_protects_bracketed_terminator() {
        assert_eq!(encode_paste("one\r\ntwo\n", false), b"one\rtwo\r");
        assert_eq!(
            encode_paste("one\x1b[201~two", true),
            b"\x1b[200~one[201~two\x1b[201~"
        );
    }
}
