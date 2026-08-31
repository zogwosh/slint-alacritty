//! Alacritty 终端状态与真实 PTY 的适配层。
//!
//! 本模块持有终端网格、PTY 事件循环和本地选择状态，并把变化整理为帧补丁。

use super::{
    command::{
        MouseAction, MouseButton, MouseInput, MouseScrollInput, TerminalSize, WorkerMessage,
    },
    frame::{FramePatch, capture_frame},
    input::{KeyInput, encode_key},
    mouse::{encode_mouse_button_code, encode_mouse_report, scroll_lines, visible_point},
    notifier::Notifier,
};
use alacritty_terminal::{
    event::{EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::{Dimensions, Scroll},
    index::{Column, Point, Side},
    selection::{Selection, SelectionRange, SelectionType},
    sync::FairMutex,
    term::{Config, Term},
    tty::{self, Options},
};
use std::{
    borrow::Cow,
    io,
    sync::{Arc, mpsc::Sender},
    thread::JoinHandle,
};

/// 单个终端会话的后台实现。
pub(super) struct TerminalBackend {
    /// Alacritty 网格；PTY 事件线程会更新它，工作线程会读取它。
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
    /// 每次字符网格尺寸改变时递增，使旧增量帧自动失效。
    generation: u64,
    cell_width: u16,
    cell_height: u16,
    /// true 表示当前拖动属于本地文本选择，而不是发给终端应用。
    selecting: bool,
    pressed_button: Option<MouseButton>,
    forced_full_redraw: Option<super::frame::FullRedrawReason>,
    /// 上一帧已绘制的选择区，用于额外标记取消高亮的旧行。
    rendered_selection: SelectionSnapshot,
}

/// 只保留判断选择区绘制变化所需的数据。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SelectionSnapshot {
    range: Option<SelectionRange>,
    display_offset: usize,
}

impl TerminalBackend {
    /// 创建 Alacritty 网格、系统 PTY 和负责读取子进程输出的事件线程。
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
            forced_full_redraw: None,
            rendered_selection: SelectionSnapshot::default(),
        })
    }

    /// 在终端变脏时捕获最新帧，同时附带一次性的标题与退出信息。
    pub(super) fn take_frame(&mut self) -> Option<FramePatch> {
        if !self.notifier.begin_frame() {
            return None;
        }

        let forced_full_redraw = self.forced_full_redraw.take();
        let mut terminal = self.terminal.lock();
        let selection = selection_snapshot(&terminal);
        // 选择区不是 Alacritty 单元本身的 damage，需要补上新旧范围涉及的行。
        let selection_dirty_rows = if selection == self.rendered_selection {
            Vec::new()
        } else {
            let mut rows = visible_selection_rows(self.rendered_selection, self.size.rows);
            rows.extend(visible_selection_rows(selection, self.size.rows));
            rows.sort_unstable();
            rows.dedup();
            rows
        };
        self.rendered_selection = selection;
        let mut frame = capture_frame(
            &mut terminal,
            self.size.columns,
            self.size.rows,
            self.generation,
            forced_full_redraw,
            &selection_dirty_rows,
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
        self.forced_full_redraw = Some(super::frame::FullRedrawReason::RendererRequest);
        self.notifier.mark_dirty();
    }

    pub(super) fn send_to_pty(&self, text: String) {
        self.notifier.send_to_pty(text);
    }

    /// 编码抽象按键，并把生成的 ANSI 字节送进 PTY。
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

    /// 按终端的 bracketed-paste 模式安全地编码剪贴板文本。
    pub(super) fn paste(&self, text: &str) {
        let mode = *self.terminal.lock().mode();
        let bytes = encode_paste(
            text,
            mode.contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE),
        );
        let _ = self.pty_sender.send(Msg::Input(Cow::Owned(bytes)));
    }

    /// 同时调整 Alacritty 网格与真实 PTY，并开启新的帧世代。
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
        self.forced_full_redraw = Some(super::frame::FullRedrawReason::Resize);
        self.notifier.mark_dirty();
    }

    /// 在“终端应用鼠标协议”和“本地文本选择”之间路由鼠标事件。
    pub(super) fn mouse_input(&mut self, input: MouseInput) {
        let mut terminal = self.terminal.lock();
        let mode = *terminal.mode();
        // 一次拖动始终归最初接收按下事件的一方所有，避免中途按下/释放 Shift 时，
        // 在本地选择与应用鼠标报告之间跳变。
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
        drop(terminal);
        self.notifier.mark_dirty();
    }

    /// 根据终端模式把滚轮解释为鼠标报告、方向键或本地历史滚动。
    pub(super) fn mouse_scroll(&mut self, input: MouseScrollInput) {
        let lines = scroll_lines(input.lines);
        if lines == 0 {
            return;
        }

        let mut terminal = self.terminal.lock();
        let mode = *terminal.mode();
        if mode.intersects(alacritty_terminal::term::TermMode::MOUSE_MODE) && !input.shift {
            // 运行中的 TUI 请求了鼠标跟踪：把滚轮编码成按钮 64/65。
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
            // 备用屏幕常没有滚动历史，传统终端会把滚轮转换为上下方向键。
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

    /// 将滚动条位置换算为 Alacritty 视口偏移，并限制在现有历史范围内。
    pub(super) fn scroll_to(&mut self, display_offset: usize) {
        let mut terminal = self.terminal.lock();
        let history_lines = terminal
            .total_lines()
            .saturating_sub(terminal.screen_lines());
        let target = display_offset.min(history_lines);
        let current = terminal.grid().display_offset();
        let delta = target as i64 - current as i64;
        terminal.scroll_display(Scroll::Delta(
            delta.clamp(i32::MIN as i64, i32::MAX as i64) as i32
        ));
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        self.terminal.lock().selection_to_string()
    }

    /// 选中从历史缓冲区顶部到当前网格底部的全部文本。
    pub(super) fn select_all(&mut self) {
        let mut terminal = self.terminal.lock();
        let start = Point::new(terminal.topmost_line(), Column(0));
        let end = Point::new(terminal.bottommost_line(), terminal.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        terminal.selection = Some(selection);
        drop(terminal);
        self.notifier.mark_dirty();
    }
}

fn selection_snapshot<T: EventListener>(terminal: &Term<T>) -> SelectionSnapshot {
    let content = terminal.renderable_content();
    SelectionSnapshot {
        range: content.selection,
        display_offset: content.display_offset,
    }
}

/// 把选择区裁剪到当前可见视口，返回需要重新绘制的行号。
fn visible_selection_rows(snapshot: SelectionSnapshot, rows: usize) -> Vec<usize> {
    let Some(range) = snapshot.range else {
        return Vec::new();
    };
    let offset = snapshot.display_offset as i32;
    let start = range.start.line.0 + offset;
    let end = range.end.line.0 + offset;
    if rows == 0 || end < 0 || start >= rows as i32 {
        return Vec::new();
    }
    let start = start.max(0) as usize;
    let end = end.min(rows as i32 - 1) as usize;
    (start..=end).collect()
}

impl Drop for TerminalBackend {
    fn drop(&mut self) {
        // 关闭 PTY 事件循环并等待读取线程结束，避免会话资源泄漏。
        let _ = self.pty_sender.send(Msg::Shutdown);
        if let Some(thread) = self.event_thread.take() {
            let _ = thread.join();
        }
    }
}

/// 规范化换行；bracketed paste 会加边界序列并移除可注入结束标记的 ESC。
fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if bracketed {
        let safe_text = normalized.replace('\x1b', "");
        format!("\x1b[200~{safe_text}\x1b[201~").into_bytes()
    } else {
        normalized.replace('\n', "\r").into_bytes()
    }
}

/// 将字符网格尺寸和单元像素尺寸转换为 PTY ioctl 使用的 WindowSize。
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
    use super::{SelectionSnapshot, encode_paste, visible_selection_rows};
    use alacritty_terminal::{
        index::{Column, Line, Point},
        selection::SelectionRange,
    };

    #[test]
    fn paste_normalizes_lines_and_protects_bracketed_terminator() {
        assert_eq!(encode_paste("one\r\ntwo\n", false), b"one\rtwo\r");
        assert_eq!(
            encode_paste("one\x1b[201~two", true),
            b"\x1b[200~one[201~two\x1b[201~"
        );
    }

    #[test]
    fn selection_damage_is_clipped_to_visible_rows() {
        let snapshot = SelectionSnapshot {
            range: Some(SelectionRange::new(
                Point::new(Line(-2), Column(0)),
                Point::new(Line(2), Column(5)),
                false,
            )),
            display_offset: 0,
        };
        assert_eq!(visible_selection_rows(snapshot, 3), vec![0, 1, 2]);

        let hidden = SelectionSnapshot {
            range: Some(SelectionRange::new(
                Point::new(Line(-4), Column(0)),
                Point::new(Line(-2), Column(5)),
                false,
            )),
            display_offset: 0,
        };
        assert!(visible_selection_rows(hidden, 3).is_empty());
    }
}
