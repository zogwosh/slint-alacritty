//! Alacritty 终端状态与真实 PTY 的适配层。
//!
//! 本模块持有终端网格、PTY 事件循环和本地选择状态，并把变化整理为帧补丁。

use super::{
    command::{
        MouseAction, MouseButton, MouseInput, MouseScrollInput, TerminalSize, WorkerMessage,
    },
    frame::{FramePatch, capture_frame},
    input::{KeyInput, encode_key},
    mouse::{
        accumulate_scroll_lines, encode_mouse_button_code, encode_mouse_report, visible_point,
    },
    notifier::Notifier,
    palette::{TerminalTheme, resolve_dynamic_color},
    search::SearchState,
    ssh::configure_askpass,
};
use crate::app::settings::{ProfileKind, ShellProfile};
use alacritty_terminal::{
    event::WindowSize,
    event_loop::{EventLoop, EventLoopSender, Msg},
    grid::{Dimensions, Scroll},
    index::{Column, Point, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, Term},
    tty::{self, Options, Shell},
};
use std::{
    borrow::Cow,
    io,
    sync::{Arc, mpsc::SyncSender},
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
    /// true 表示当前拖动属于本地文本选择，而不是发给终端应用。
    selecting: bool,
    pressed_button: Option<MouseButton>,
    forced_full_redraw: Option<super::frame::FullRedrawReason>,
    scroll_remainder: f32,
    search: SearchState,
    theme: TerminalTheme,
}

impl TerminalBackend {
    /// 创建 Alacritty 网格、系统 PTY 和负责读取子进程输出的事件线程。
    pub(super) fn new(
        columns: usize,
        rows: usize,
        cell_width: f32,
        cell_height: f32,
        worker_sender: SyncSender<WorkerMessage>,
        profile: Option<&ShellProfile>,
        theme: TerminalTheme,
    ) -> io::Result<Self> {
        let size = TerminalSize::new(columns, rows, cell_width, cell_height);
        let window_size = window_size(size);
        let notifier = Notifier::new(window_size, worker_sender);
        let terminal = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            notifier.clone(),
        )));

        let mut options = Options::default();
        if let Some(profile) = profile {
            match profile.kind {
                ProfileKind::Local => {
                    options.shell = Some(Shell::new(
                        profile.program.clone(),
                        profile.arguments.clone(),
                    ));
                    options.working_directory = profile.working_directory.clone();
                    options.env = profile.environment.clone();
                }
                ProfileKind::Ssh => {
                    options.shell = Some(Shell::new("ssh".to_owned(), ssh_arguments(profile)));
                    configure_askpass(&mut options.env, profile)?;
                }
            }
        }
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
            selecting: false,
            pressed_button: None,
            forced_full_redraw: None,
            scroll_remainder: 0.0,
            search: SearchState::default(),
            theme,
        })
    }

    /// 在终端变脏时捕获最新帧，同时附带一次性的标题与退出信息。
    pub(super) fn take_frame(&mut self) -> Option<FramePatch> {
        if !self.notifier.begin_frame() {
            return None;
        }

        let mut terminal = self.terminal.lock();
        // 计数刷新只影响查找面板，高亮由视口搜索每帧重新得出，不需要重绘终端。
        self.search.refresh_if_due(&mut terminal);
        let forced_full_redraw = self.forced_full_redraw.take();
        let search = self.search.snapshot();
        let search_matches = self.search.visible_matches(&terminal);
        let mut frame = capture_frame(
            &mut terminal,
            self.size.columns,
            self.size.rows,
            self.generation,
            forced_full_redraw,
            self.theme,
            search,
            &search_matches,
        );
        drop(terminal);
        frame.title = self.notifier.take_title();
        frame.exit_message = self.notifier.take_exit_message();
        Some(frame)
    }

    /// 后台会话不捕获网格：只消费脏标记并在有标题/退出信息时产出一帧轻量元数据。
    /// 期间累积的 damage 会在会话重新激活并请求完整重绘时一并覆盖。
    pub(super) fn take_metadata_frame(&mut self) -> Option<FramePatch> {
        if !self.notifier.begin_frame() {
            return None;
        }
        let title = self.notifier.take_title();
        let exit_message = self.notifier.take_exit_message();
        if title.is_none() && exit_message.is_none() {
            return None;
        }
        Some(FramePatch {
            generation: self.generation,
            columns: self.size.columns,
            rows: self.size.rows,
            full_redraw: false,
            full_redraw_reason: None,
            metadata_only: true,
            changed_rows: Vec::new(),
            cursor: super::frame::CursorPatch::default(),
            scroll_offset: 0,
            scroll_history_lines: 0,
            search: self.search.snapshot(),
            decorations: Vec::new(),
            title,
            exit_message,
        })
    }

    pub(super) fn mark_dirty(&self) {
        self.notifier.mark_dirty();
    }

    pub(super) fn request_full_redraw(&mut self) {
        self.forced_full_redraw = Some(super::frame::FullRedrawReason::RendererRequest);
        self.notifier.mark_dirty();
    }

    /// 搜索状态只影响装饰层与查找面板，因此仅标记脏而不请求完整重绘。
    pub(super) fn update_search(&mut self, query: String) {
        let mut terminal = self.terminal.lock();
        self.search.set_query(&mut terminal, query);
        drop(terminal);
        self.notifier.mark_dirty();
    }

    pub(super) fn search_step(&mut self, previous: bool) {
        let mut terminal = self.terminal.lock();
        self.search.step(&mut terminal, previous);
        drop(terminal);
        self.notifier.mark_dirty();
    }

    pub(super) fn send_to_pty(&self, text: String) {
        self.notifier.send_to_pty(text);
    }

    /// 回答 OSC 4/10/11/12 等动态颜色查询，优先返回终端程序当前设置的覆盖色。
    pub(super) fn respond_color_request(
        &self,
        index: usize,
        formatter: Arc<dyn Fn(alacritty_terminal::vte::ansi::Rgb) -> String + Send + Sync>,
    ) {
        let color = {
            let terminal = self.terminal.lock();
            let content = terminal.renderable_content();
            resolve_dynamic_color(index, content.colors, self.theme)
        };
        if let Some(color) = color {
            self.notifier.send_to_pty(formatter(color));
        }
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

        let grid_changed = self.size.columns != size.columns || self.size.rows != size.rows;
        if grid_changed {
            let mut terminal = self.terminal.lock();
            terminal.resize(size);
            if !self.search.query().is_empty() {
                let query = self.search.query().to_owned();
                self.search.set_query(&mut terminal, query);
            }
        }
        let new_window_size = window_size(size);
        self.notifier.update_window_size(new_window_size);
        let _ = self.pty_sender.send(Msg::Resize(new_window_size));
        self.size = size;
        if grid_changed {
            self.generation = self.generation.wrapping_add(1);
            self.forced_full_redraw = Some(super::frame::FullRedrawReason::Resize);
        }
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
        let lines = accumulate_scroll_lines(&mut self.scroll_remainder, input.lines);
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

fn ssh_arguments(profile: &ShellProfile) -> Vec<String> {
    let mut arguments = Vec::new();
    if !profile.ssh_password.is_empty() {
        // 防止用户的 SendEnv 配置把 askpass 专用密码环境变量发送到远端。
        arguments.extend([
            "-o".to_owned(),
            "SendEnv=-SLINT_TERMINAL_SSH_PASSWORD".to_owned(),
        ]);
    }
    if profile.ssh_port != 22 {
        arguments.extend(["-p".to_owned(), profile.ssh_port.to_string()]);
    }
    if let Some(identity_file) = &profile.ssh_identity_file {
        arguments.extend([
            "-i".to_owned(),
            identity_file.to_string_lossy().into_owned(),
        ]);
    }
    arguments.extend(profile.arguments.iter().cloned());
    let destination = if profile.ssh_user.is_empty() {
        profile.ssh_host.clone()
    } else {
        format!("{}@{}", profile.ssh_user, profile.ssh_host)
    };
    arguments.push(destination);
    arguments
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
fn window_size(size: TerminalSize) -> WindowSize {
    WindowSize {
        num_lines: size.rows.min(u16::MAX as usize) as u16,
        num_cols: size.columns.min(u16::MAX as usize) as u16,
        cell_width: size.cell_width,
        cell_height: size.cell_height,
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_paste, ssh_arguments};
    use crate::app::settings::{ProfileKind, ShellProfile};

    #[test]
    fn paste_normalizes_lines_and_protects_bracketed_terminator() {
        assert_eq!(encode_paste("one\r\ntwo\n", false), b"one\rtwo\r");
        assert_eq!(
            encode_paste("one\x1b[201~two", true),
            b"\x1b[200~one[201~two\x1b[201~"
        );
    }

    #[test]
    fn ssh_profile_builds_arguments_without_shell_quoting() {
        let profile = ShellProfile {
            kind: ProfileKind::Ssh,
            ssh_host: "example.com".to_owned(),
            ssh_user: "deploy".to_owned(),
            ssh_port: 2222,
            ssh_identity_file: Some("C:\\Keys\\work key".into()),
            ssh_password: "plain secret".to_owned(),
            arguments: vec!["-A".to_owned()],
            ..ShellProfile::default()
        };
        assert_eq!(
            ssh_arguments(&profile),
            vec![
                "-o",
                "SendEnv=-SLINT_TERMINAL_SSH_PASSWORD",
                "-p",
                "2222",
                "-i",
                "C:\\Keys\\work key",
                "-A",
                "deploy@example.com"
            ]
        );
    }
}
