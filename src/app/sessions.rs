//! 终端会话与标签页状态，以及它们到 Slint 模型的同步。

use crate::{
    MainWindow, TabData,
    terminal::{FramePatch, TerminalController},
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
    pub(super) controller: Rc<TerminalController>,
}

/// 保存全部标签页以及当前激活位置。
pub(super) struct TabManager {
    pub(super) sessions: Vec<TerminalSession>,
    pub(super) active: usize,
    pub(super) next_id: i32,
}

impl TabManager {
    pub(super) fn active_session(&self) -> Option<&TerminalSession> {
        self.sessions.get(self.active)
    }

    pub(super) fn active_controller(&self) -> Option<Rc<TerminalController>> {
        self.active_session()
            .map(|session| session.controller.clone())
    }

    pub(super) fn active_id(&self) -> Option<i32> {
        self.active_session().map(|session| session.id)
    }
}

/// 启动一个 PTY，并把后台的“有新帧”通知转发到 Slint 事件循环。
pub(super) fn create_session(
    ui: &MainWindow,
    id: i32,
    columns: usize,
    rows: usize,
) -> io::Result<TerminalSession> {
    let weak_ui = ui.as_weak();
    let controller = Rc::new(TerminalController::new(
        columns,
        rows,
        ui.get_cell_width(),
        ui.get_cell_height(),
        move || {
            let weak_ui = weak_ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak_ui.upgrade() {
                    ui.invoke_frame_ready();
                }
            });
        },
    )?);
    Ok(TerminalSession {
        id,
        title: format!("Terminal {id}").into(),
        terminal_active: true,
        exit_message: SharedString::default(),
        scroll_offset: 0,
        scroll_history_lines: 0,
        controller,
    })
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
        })
        .collect::<Vec<_>>();
    ui.set_tabs(ModelRc::new(VecModel::from(model)));
    if let Some(active) = manager.active_session() {
        ui.set_active_tab_id(active.id);
        ui.set_terminal_title(active.title.clone());
        ui.set_terminal_active(active.terminal_active);
        ui.set_exit_message(active.exit_message.clone());
        ui.set_scroll_offset(active.scroll_offset);
        ui.set_scroll_history_lines(active.scroll_history_lines);
    }
}

/// 新建终端标签页，并立即切换到该会话。
pub(super) fn add_tab(
    ui: &MainWindow,
    tabs: &Rc<RefCell<TabManager>>,
    awaiting_full_frame: &Rc<Cell<bool>>,
) {
    let id = {
        let mut manager = tabs.borrow_mut();
        let id = manager.next_id;
        manager.next_id += 1;
        id
    };
    let session = match create_session(
        ui,
        id,
        ui.get_viewport_columns().max(2) as usize,
        ui.get_viewport_rows().max(1) as usize,
    ) {
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
    sync_tab_ui(ui, &manager);
    drop(manager);
    awaiting_full_frame.set(true);
    controller.request_full_redraw();
    ui.invoke_frame_ready();
}

/// 关闭指定会话；最后一个标签关闭时隐藏窗口以结束应用。
pub(super) fn close_tab(
    ui: &MainWindow,
    tabs: &Rc<RefCell<TabManager>>,
    awaiting_full_frame: &Rc<Cell<bool>>,
    id: i32,
) {
    let (removed, next_controller, is_empty) = {
        let mut manager = tabs.borrow_mut();
        let Some(index) = manager.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        let removed = manager.sessions.remove(index);
        if manager.sessions.is_empty() {
            (removed, None, true)
        } else {
            if index < manager.active || manager.active >= manager.sessions.len() {
                manager.active = manager.active.saturating_sub(1);
            }
            sync_tab_ui(ui, &manager);
            (removed, manager.active_controller(), false)
        }
    };
    drop(removed);
    if is_empty {
        let _ = ui.window().hide();
        return;
    }
    awaiting_full_frame.set(true);
    if let Some(controller) = next_controller {
        controller.request_full_redraw();
    }
    ui.invoke_frame_ready();
}

/// 将终端标题、退出状态和滚动信息写回对应标签页。
pub(super) fn apply_frame_metadata(
    ui: &MainWindow,
    tabs: &Rc<RefCell<TabManager>>,
    session_id: i32,
    frame: &FramePatch,
) {
    let mut manager = tabs.borrow_mut();
    let Some(session) = manager
        .sessions
        .iter_mut()
        .find(|session| session.id == session_id)
    else {
        return;
    };
    let mut changed = false;
    session.scroll_offset = frame.scroll_offset.min(i32::MAX as usize) as i32;
    session.scroll_history_lines = frame.scroll_history_lines.min(i32::MAX as usize) as i32;
    ui.set_scroll_offset(session.scroll_offset);
    ui.set_scroll_history_lines(session.scroll_history_lines);
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
