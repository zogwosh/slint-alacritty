//! 终端会话与标签页状态，以及它们到 Slint 模型的同步。

use crate::{
    MainWindow, TabData,
    app::settings::ShellProfile,
    terminal::{FramePatch, RgbColor, RgbaColor, TerminalController, TerminalTheme},
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::{Cell, RefCell},
    io,
    rc::Rc,
};

/// 一个标签页对应的一次独立终端会话。
pub(super) struct TerminalSession {
    /// 稳定的标签页标识；切换和关闭标签时不依赖数组下标。
    pub(super) id: i32,
    pub(super) title: SharedString,
    pub(super) terminal_active: bool,
    pub(super) exit_message: SharedString,
    pub(super) scroll_offset: i32,
    pub(super) scroll_history_lines: i32,
    pub(super) cursor_column: i32,
    pub(super) cursor_row: i32,
    pub(super) controller: Rc<TerminalController>,
}

/// 保存全部标签页以及当前激活位置。
pub(super) struct TabManager {
    pub(super) sessions: Vec<TerminalSession>,
    pub(super) active: usize,
    pub(super) next_id: i32,
    pub(super) settings_open: bool,
    pub(super) settings_active: bool,
}

impl TabManager {
    /// 当前选中的终端会话。设置页覆盖终端时，它仍然是需要持续同步尺寸的会话。
    pub(super) fn selected_session(&self) -> Option<&TerminalSession> {
        self.sessions.get(self.active)
    }

    pub(super) fn selected_controller(&self) -> Option<Rc<TerminalController>> {
        self.selected_session()
            .map(|session| session.controller.clone())
    }

    pub(super) fn active_session(&self) -> Option<&TerminalSession> {
        (!self.settings_active)
            .then(|| self.selected_session())
            .flatten()
    }

    pub(super) fn active_controller(&self) -> Option<Rc<TerminalController>> {
        self.active_session()
            .map(|session| session.controller.clone())
    }

    pub(super) fn active_id(&self) -> Option<i32> {
        self.active_session().map(|session| session.id)
    }
}

/// 终端单元的物理像素尺寸；PTY、GPU 渲染器与 Slint 布局共用这一整数度量。
pub(super) fn physical_cell_size(ui: &MainWindow) -> (f32, f32) {
    (
        ui.get_terminal_cell_width_px().max(1) as f32,
        ui.get_terminal_cell_height_px().max(1) as f32,
    )
}

/// Slint 当前布局得出的字符网格尺寸。
pub(super) fn viewport_grid(ui: &MainWindow) -> (usize, usize) {
    (
        ui.get_viewport_columns().max(2) as usize,
        ui.get_viewport_rows().max(1) as usize,
    )
}

/// 启动一个 PTY，并把后台的“有新帧”通知转发到 Slint 事件循环。
pub(super) fn create_session(
    ui: &MainWindow,
    id: i32,
    columns: usize,
    rows: usize,
    profile: Option<&ShellProfile>,
) -> io::Result<TerminalSession> {
    let weak_ui = ui.as_weak();
    let (cell_width, cell_height) = physical_cell_size(ui);
    // 标签页的初始名字同时也是程序清除标题后回退的名字，两处必须是同一个字符串。
    let default_title = format!("Terminal {id}");
    let controller = Rc::new(TerminalController::new(
        columns,
        rows,
        cell_width,
        cell_height,
        profile,
        terminal_theme(ui),
        default_title.clone(),
        move || {
            let weak_ui = weak_ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.invoke_frame_ready(id);
                }
            });
        },
    )?);
    Ok(TerminalSession {
        id,
        title: default_title.into(),
        terminal_active: true,
        exit_message: SharedString::default(),
        scroll_offset: 0,
        scroll_history_lines: 0,
        cursor_column: 0,
        cursor_row: 0,
        controller,
    })
}

pub(super) fn terminal_theme(ui: &MainWindow) -> TerminalTheme {
    TerminalTheme {
        background: rgb(ui.get_terminal_background_token()),
        foreground: rgb(ui.get_terminal_foreground_token()),
        selection_background: rgba(ui.get_terminal_selection_token()),
        search_match_background: rgba(ui.get_terminal_search_match_token()),
        search_current_background: rgba(ui.get_terminal_search_current_token()),
    }
}

fn rgb(color: slint::Color) -> RgbColor {
    RgbColor {
        red: color.red(),
        green: color.green(),
        blue: color.blue(),
    }
}

/// 装饰色保留设计令牌中的透明度，使选区可以叠加在 ANSI 背景色之上。
fn rgba(color: slint::Color) -> RgbaColor {
    RgbaColor {
        red: color.red(),
        green: color.green(),
        blue: color.blue(),
        alpha: color.alpha(),
    }
}

/// 将 Rust 中的标签页状态转换为 Slint 模型，并同步活动会话的元数据。
pub(super) fn sync_tab_ui(ui: &MainWindow, manager: &TabManager) {
    let model = manager
        .sessions
        .iter()
        .map(|session| TabData {
            id: session.id,
            title: session.title.clone(),
            terminal_active: session.terminal_active,
            is_settings: false,
        })
        .chain(manager.settings_open.then_some(TabData {
            id: -1,
            title: "设置".into(),
            terminal_active: false,
            is_settings: true,
        }))
        .collect::<Vec<_>>();
    ui.set_tabs(ModelRc::new(VecModel::from(model)));
    ui.set_settings_active(manager.settings_active);
    // 只有正在显示的会话才需要完整的网格捕获；其余会话降级为只上报元数据。
    let active_id = manager.active_id();
    for session in &manager.sessions {
        session.controller.set_active(Some(session.id) == active_id);
    }
    if manager.settings_active {
        ui.set_active_tab_id(-1);
        ui.set_active_tab_index(manager.sessions.len().min(i32::MAX as usize) as i32);
        ui.set_terminal_title("设置".into());
        return;
    }
    if let Some(active) = manager.active_session() {
        ui.set_active_tab_id(active.id);
        ui.set_active_tab_index(manager.active.min(i32::MAX as usize) as i32);
        ui.set_terminal_title(active.title.clone());
        ui.set_terminal_active(active.terminal_active);
        ui.set_exit_message(active.exit_message.clone());
        ui.set_scroll_offset(active.scroll_offset);
        ui.set_scroll_history_lines(active.scroll_history_lines);
        ui.set_terminal_cursor_column(active.cursor_column);
        ui.set_terminal_cursor_row(active.cursor_row);
    }
}

/// 新建终端标签页，并立即切换到该会话。
pub(super) fn add_tab(
    ui: &MainWindow,
    tabs: &Rc<RefCell<TabManager>>,
    awaiting_full_frame: &Rc<Cell<bool>>,
    profile: Option<&ShellProfile>,
) {
    let id = {
        let mut manager = tabs.borrow_mut();
        let id = manager.next_id;
        manager.next_id += 1;
        id
    };
    let (columns, rows) = viewport_grid(ui);
    let session = match create_session(ui, id, columns, rows, profile) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("failed to create terminal tab: {error}");
            return;
        }
    };
    let controller = session.controller.clone();
    let mut manager = tabs.borrow_mut();
    manager.sessions.push(session);
    manager.active = manager.sessions.len() - 1;
    manager.settings_active = false;
    sync_tab_ui(ui, &manager);
    drop(manager);
    activate_session(ui, awaiting_full_frame, id, &controller);
}

/// 让某个会话成为画面来源：对齐当前网格与搜索查询，并要求它重发完整画面。
pub(super) fn activate_session(
    ui: &MainWindow,
    awaiting_full_frame: &Cell<bool>,
    id: i32,
    controller: &TerminalController,
) {
    awaiting_full_frame.set(true);
    let (columns, rows) = viewport_grid(ui);
    let (cell_width, cell_height) = physical_cell_size(ui);
    controller.resize(columns, rows, cell_width, cell_height);
    // 查询文本由界面拥有，切换会话后要让新会话的高亮与之一致。
    let query = if ui.get_search_open() {
        ui.get_search_query().to_string()
    } else {
        String::new()
    };
    controller.update_search(query);
    controller.request_full_redraw();
    ui.invoke_frame_ready(id);
}

/// 关闭指定会话；最后一个标签关闭时隐藏窗口以结束应用。
pub(super) fn close_tab(
    ui: &MainWindow,
    tabs: &Rc<RefCell<TabManager>>,
    awaiting_full_frame: &Rc<Cell<bool>>,
    id: i32,
) {
    let (removed, next_session, is_empty) = {
        let mut manager = tabs.borrow_mut();
        let Some(index) = manager.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        let removed = manager.sessions.remove(index);
        if manager.sessions.is_empty() && !manager.settings_open {
            (removed, None, true)
        } else {
            if !manager.sessions.is_empty()
                && (index < manager.active || manager.active >= manager.sessions.len())
            {
                manager.active = manager.active.saturating_sub(1);
            }
            if manager.sessions.is_empty() {
                manager.settings_active = true;
            }
            sync_tab_ui(ui, &manager);
            (
                removed,
                manager
                    .active_session()
                    .map(|session| (session.id, session.controller.clone())),
                false,
            )
        }
    };
    drop(removed);
    if is_empty {
        let _ = ui.window().hide();
        return;
    }
    awaiting_full_frame.set(true);
    if let Some((id, controller)) = next_session {
        activate_session(ui, awaiting_full_frame, id, &controller);
    }
}

/// 将终端标题、退出状态和滚动信息写回对应标签页。
pub(super) fn apply_frame_metadata(
    ui: &MainWindow,
    tabs: &Rc<RefCell<TabManager>>,
    session_id: i32,
    frame: &FramePatch,
) {
    let mut manager = tabs.borrow_mut();
    let is_active = !manager.settings_active
        && manager
            .sessions
            .get(manager.active)
            .is_some_and(|active| active.id == session_id);
    let Some(session) = manager
        .sessions
        .iter_mut()
        .find(|session| session.id == session_id)
    else {
        return;
    };
    let mut changed = false;
    if !frame.metadata_only {
        session.scroll_offset = frame.scroll_offset.min(i32::MAX as usize) as i32;
        session.scroll_history_lines = frame.scroll_history_lines.min(i32::MAX as usize) as i32;
        session.cursor_column = frame.cursor.column.min(i32::MAX as usize) as i32;
        session.cursor_row = frame.cursor.row.min(i32::MAX as usize) as i32;
    }
    if is_active && !frame.metadata_only {
        ui.set_scroll_offset(session.scroll_offset);
        ui.set_scroll_history_lines(session.scroll_history_lines);
        ui.set_terminal_cursor_column(session.cursor_column);
        ui.set_terminal_cursor_row(session.cursor_row);
        // 查询文本由搜索框拥有；后台快照可能落后于用户输入（防抖），只回传计数。
        ui.set_search_result_current(frame.search.current.min(i32::MAX as usize) as i32);
        ui.set_search_result_total(frame.search.total.min(i32::MAX as usize) as i32);
    }
    if let Some(title) = &frame.title {
        session.title = title.as_str().into();
        changed = true;
    }
    if let Some(message) = &frame.exit_message {
        session.terminal_active = false;
        session.exit_message = message.as_str().into();
        changed = true;
    }
    if changed {
        sync_tab_ui(ui, &manager);
    }
}
