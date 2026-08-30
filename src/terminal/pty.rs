use super::{
    input::{KeyInput, encode_key},
    snapshot::FramePatch,
};
use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, Term},
    tty::{self, Options},
};
use arboard::Clipboard;
use std::{
    borrow::Cow,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

enum WorkerMessage {
    Input {
        input: KeyInput,
        control: bool,
        alt: bool,
        shift: bool,
        altgr: bool,
    },
    Resize(TerminalSize),
    PasteClipboard,
    ClipboardStore(String),
    ClipboardLoad(Arc<dyn Fn(&str) -> String + Send + Sync + 'static>),
    Mouse(MouseInput),
    MouseScroll(MouseScrollInput),
    CopySelection,
    Render,
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseButton {
    Left,
    Middle,
    Right,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseAction {
    Press,
    Release,
    Move,
    Cancel,
    DoubleClick,
}

#[derive(Clone, Copy, Debug)]
struct MouseInput {
    column: usize,
    row: usize,
    button: MouseButton,
    action: MouseAction,
    shift: bool,
    alt: bool,
    control: bool,
    right_half: bool,
}

#[derive(Clone, Copy, Debug)]
struct MouseScrollInput {
    column: usize,
    row: usize,
    lines: f32,
    shift: bool,
    alt: bool,
    control: bool,
}

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

    fn send_to_pty(&self, text: String) {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TerminalSize {
    pub(super) columns: usize,
    pub(super) rows: usize,
}

impl Dimensions for TerminalSize {
    fn columns(&self) -> usize {
        self.columns
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn total_lines(&self) -> usize {
        self.rows
    }
}

pub(super) struct TerminalBackend {
    pub(super) terminal: Arc<FairMutex<Term<Notifier>>>,
    pub(super) notifier: Notifier,
    pty_sender: EventLoopSender,
    event_thread: Option<
        JoinHandle<(
            EventLoop<tty::Pty, Notifier>,
            alacritty_terminal::event_loop::State,
        )>,
    >,
    pub(super) size: TerminalSize,
    pub(super) generation: u64,
    cell_width: u16,
    cell_height: u16,
    selecting: bool,
    pressed_button: Option<MouseButton>,
    pub(super) force_full_redraw: bool,
}

impl TerminalBackend {
    fn new(
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

    fn send_key(&self, input: KeyInput, control: bool, alt: bool, shift: bool, altgr: bool) {
        let mode = *self.terminal.lock().mode();
        let bytes = encode_key(input, control, alt, shift, altgr, mode);
        if !bytes.is_empty() {
            let _ = self.pty_sender.send(Msg::Input(Cow::Owned(bytes)));
        }
    }

    fn paste(&self, text: &str) {
        let mode = *self.terminal.lock().mode();
        let bytes = encode_paste(
            text,
            mode.contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE),
        );
        let _ = self.pty_sender.send(Msg::Input(Cow::Owned(bytes)));
    }

    fn resize(&mut self, size: TerminalSize) {
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

    fn mouse_input(&mut self, input: MouseInput) {
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

    fn mouse_scroll(&mut self, input: MouseScrollInput) {
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

    fn selected_text(&self) -> Option<String> {
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

pub(crate) struct TerminalController {
    worker_sender: Sender<WorkerMessage>,
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

fn run_worker(
    mut backend: TerminalBackend,
    receiver: Receiver<WorkerMessage>,
    latest_frame: Arc<Mutex<Option<FramePatch>>>,
    notification_pending: Arc<AtomicBool>,
    frame_notifier: Arc<dyn Fn() + Send + Sync>,
) {
    let mut clipboard = Clipboard::new().ok();
    let mut frame_pending = true;
    let mut last_frame = Instant::now() - FRAME_INTERVAL;
    backend.notifier.mark_dirty();

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
                    backend.notifier.send_to_pty(formatter(&text));
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
    pending.cursor = incoming.cursor;
    if incoming.title.is_some() {
        pending.title = incoming.title;
    }
    if incoming.exit_message.is_some() {
        pending.exit_message = incoming.exit_message;
    }
    *slot = Some(pending);
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

fn visible_point(
    terminal: &Term<Notifier>,
    size: TerminalSize,
    column: usize,
    row: usize,
) -> Point {
    let display_offset = terminal.grid().display_offset() as i32;
    Point::new(
        Line(row.min(size.rows.saturating_sub(1)) as i32 - display_offset),
        Column(column.min(size.columns.saturating_sub(1))),
    )
}

fn scroll_lines(lines: f32) -> i32 {
    if !lines.is_finite() || lines == 0.0 {
        return 0;
    }

    let rounded = lines.round() as i32;
    if rounded == 0 {
        lines.signum() as i32
    } else {
        rounded.clamp(-20, 20)
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_mouse_report(
    button: MouseButton,
    action: MouseAction,
    column: usize,
    row: usize,
    shift: bool,
    alt: bool,
    control: bool,
    mode: alacritty_terminal::term::TermMode,
) -> Vec<u8> {
    let Some(mut button_code) = (match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Other => None,
    }) else {
        return Vec::new();
    };

    let release = action == MouseAction::Release;
    if release && !mode.contains(alacritty_terminal::term::TermMode::SGR_MOUSE) {
        button_code = 3;
    } else if action == MouseAction::Move {
        button_code += 32;
    }

    encode_mouse_button_code(button_code, column, row, shift, alt, control, mode, release)
}

#[allow(clippy::too_many_arguments)]
fn encode_mouse_button_code(
    button_code: u8,
    column: usize,
    row: usize,
    shift: bool,
    alt: bool,
    control: bool,
    mode: alacritty_terminal::term::TermMode,
    release: bool,
) -> Vec<u8> {
    let modifiers = u8::from(shift) * 4 + u8::from(alt) * 8 + u8::from(control) * 16;
    let button_code = button_code.saturating_add(modifiers);
    let column = column.saturating_add(1);
    let row = row.saturating_add(1);

    if mode.contains(alacritty_terminal::term::TermMode::SGR_MOUSE) {
        let suffix = if release { 'm' } else { 'M' };
        return format!("\x1b[<{button_code};{column};{row}{suffix}").into_bytes();
    }

    let values = [
        u32::from(button_code) + 32,
        column.min(223) as u32 + 32,
        row.min(223) as u32 + 32,
    ];
    if mode.contains(alacritty_terminal::term::TermMode::UTF8_MOUSE) {
        let mut encoded = b"\x1b[M".to_vec();
        for value in values {
            encoded.extend(
                char::from_u32(value)
                    .unwrap_or('\u{fffd}')
                    .to_string()
                    .as_bytes(),
            );
        }
        encoded
    } else {
        let mut encoded = b"\x1b[M".to_vec();
        encoded.extend(values.map(|value| value as u8));
        encoded
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
    use super::{
        MouseAction, MouseButton, encode_mouse_report, encode_paste, publish_frame, scroll_lines,
    };
    use crate::terminal::snapshot::{CursorPatch, FramePatch, RowPatch};
    use alacritty_terminal::term::TermMode;
    use std::sync::Mutex;

    fn frame(generation: u64, changed_rows: &[usize]) -> FramePatch {
        FramePatch {
            generation,
            columns: 80,
            rows: 24,
            full_redraw: false,
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
    fn paste_normalizes_lines_and_protects_bracketed_terminator() {
        assert_eq!(encode_paste("one\r\ntwo\n", false), b"one\rtwo\r");
        assert_eq!(
            encode_paste("one\x1b[201~two", true),
            b"\x1b[200~one[201~two\x1b[201~"
        );
    }

    #[test]
    fn sgr_mouse_encodes_press_release_and_modifiers() {
        let mode = TermMode::SGR_MOUSE;
        assert_eq!(
            encode_mouse_report(
                MouseButton::Left,
                MouseAction::Press,
                4,
                2,
                false,
                false,
                true,
                mode,
            ),
            b"\x1b[<16;5;3M"
        );
        assert_eq!(
            encode_mouse_report(
                MouseButton::Left,
                MouseAction::Release,
                4,
                2,
                false,
                false,
                false,
                mode,
            ),
            b"\x1b[<0;5;3m"
        );
    }

    #[test]
    fn legacy_mouse_encodes_motion_and_release() {
        assert_eq!(
            encode_mouse_report(
                MouseButton::Right,
                MouseAction::Move,
                0,
                0,
                false,
                false,
                false,
                TermMode::empty(),
            ),
            b"\x1b[MB!!"
        );
        assert_eq!(
            encode_mouse_report(
                MouseButton::Left,
                MouseAction::Release,
                0,
                0,
                false,
                false,
                false,
                TermMode::empty(),
            ),
            b"\x1b[M#!!"
        );
    }

    #[test]
    fn wheel_delta_is_never_lost_and_is_bounded() {
        assert_eq!(scroll_lines(0.2), 1);
        assert_eq!(scroll_lines(-0.2), -1);
        assert_eq!(scroll_lines(50.0), 20);
        assert_eq!(scroll_lines(f32::NAN), 0);
    }
}
