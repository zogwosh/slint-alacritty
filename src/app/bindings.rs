//! Slint 输入、标签页、尺寸和窗口控制回调。

use super::{
    input::{KeyAction, key_action, normalize_key},
    sessions::{TabManager, add_tab, close_tab, sync_tab_ui},
};
use crate::MainWindow;
use slint::{ComponentHandle, PhysicalPosition};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// 处理应用快捷键；未被应用消费的按键才会编码后发送给 PTY。
pub(super) fn connect_input(
    ui: &MainWindow,
    tabs: Rc<RefCell<TabManager>>,
    awaiting_full_frame: Rc<Cell<bool>>,
) {
    let weak_ui = ui.as_weak();
    ui.on_key_input(move |text, control, alt, shift, altgr| {
        let action = key_action(text.as_str(), control, alt, shift, altgr);
        match action {
            KeyAction::NewTab => {
                if let Some(ui) = weak_ui.upgrade() {
                    add_tab(&ui, &tabs, &awaiting_full_frame);
                }
            }
            KeyAction::CloseTab => {
                let active_id = tabs.borrow().active_id();
                if let (Some(ui), Some(id)) = (weak_ui.upgrade(), active_id) {
                    close_tab(&ui, &tabs, &awaiting_full_frame, id);
                }
            }
            KeyAction::Copy => {
                if let Some(controller) = tabs.borrow().active_controller() {
                    controller.copy_selection();
                }
            }
            KeyAction::SelectAll => {
                if let Some(controller) = tabs.borrow().active_controller() {
                    controller.select_all();
                }
            }
            KeyAction::Paste => {
                if let Some(controller) = tabs.borrow().active_controller() {
                    controller.paste_clipboard();
                }
            }
            KeyAction::Interrupt => {
                if let Some(controller) = tabs.borrow().active_controller() {
                    controller.send_key(
                        crate::terminal::KeyInput::Text("c".into()),
                        true,
                        false,
                        false,
                        false,
                    );
                }
            }
            KeyAction::Ignore => {}
            KeyAction::Forward => {
                if let (Some(controller), Some(input)) = (
                    tabs.borrow().active_controller(),
                    normalize_key(text.as_str()),
                ) {
                    controller.send_key(input, control, alt, shift, altgr);
                }
            }
        }
    });
}

/// 将 Slint 的鼠标坐标与修饰键转交给活动终端。
pub(super) fn connect_mouse(ui: &MainWindow, tabs: Rc<RefCell<TabManager>>) {
    let mouse_tabs = tabs.clone();
    ui.on_mouse_input(
        move |column, row, button, action, shift, alt, control, right_half| {
            if let Some(controller) = mouse_tabs.borrow().active_controller() {
                controller.mouse_input(
                    column.max(0) as usize,
                    row.max(0) as usize,
                    button,
                    action,
                    shift,
                    alt,
                    control,
                    right_half,
                );
            }
        },
    );
    let scroll_tabs = tabs.clone();
    ui.on_mouse_scroll(move |column, row, lines, shift, alt, control| {
        if let Some(controller) = scroll_tabs.borrow().active_controller() {
            controller.mouse_scroll(
                column.max(0) as usize,
                row.max(0) as usize,
                lines,
                shift,
                alt,
                control,
            );
        }
    });

    ui.on_scroll_to(move |display_offset| {
        if let Some(controller) = tabs.borrow().active_controller() {
            controller.scroll_to(display_offset.max(0) as usize);
        }
    });
}

/// 监听字符网格尺寸变化，并避开最小化时产生的无效瞬时尺寸。
pub(super) fn connect_resize(ui: &MainWindow, tabs: Rc<RefCell<TabManager>>) {
    let weak_ui = ui.as_weak();
    let resize_suspended = Rc::new(Cell::new(false));
    ui.on_viewport_resized(move |columns, rows| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };

        let terminal_width = ui.get_terminal_width();
        let terminal_height = ui.get_terminal_height();
        let cell_width = ui.get_cell_width();
        let cell_height = ui.get_cell_height();
        // 最小化或布局尚未稳定时不调整 PTY，避免 shell 收到 1×1 一类尺寸。
        let invalid_viewport = ui.window().is_minimized()
            || !terminal_width.is_finite()
            || !terminal_height.is_finite()
            || terminal_width <= cell_width
            || terminal_height <= cell_height
            || columns <= 2
            || rows <= 1;

        if invalid_viewport {
            resize_suspended.set(true);
            return;
        }

        if let Some(controller) = tabs.borrow().active_controller() {
            controller.resize(columns as usize, rows as usize);
            if resize_suspended.replace(false) {
                controller.request_full_redraw();
            }
        }
    });
}

/// 连接新建、切换和关闭标签页的界面事件。
pub(super) fn connect_tabs(
    ui: &MainWindow,
    tabs: Rc<RefCell<TabManager>>,
    awaiting_full_frame: Rc<Cell<bool>>,
) {
    let weak_ui = ui.as_weak();
    let new_tabs = tabs.clone();
    let new_awaiting = awaiting_full_frame.clone();
    ui.on_new_tab(move || {
        if let Some(ui) = weak_ui.upgrade() {
            add_tab(&ui, &new_tabs, &new_awaiting);
        }
    });

    let weak_ui = ui.as_weak();
    let select_tabs = tabs.clone();
    let select_awaiting = awaiting_full_frame.clone();
    ui.on_select_tab(move |id| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        let mut manager = select_tabs.borrow_mut();
        let Some(index) = manager.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        if index == manager.active {
            return;
        }
        manager.active = index;
        sync_tab_ui(&ui, &manager);
        let controller = manager.active_controller();
        drop(manager);
        // 每个会话有独立帧历史；切换后要求活动会话重发完整画面。
        select_awaiting.set(true);
        if let Some(controller) = controller {
            controller.resize(
                ui.get_viewport_columns().max(2) as usize,
                ui.get_viewport_rows().max(1) as usize,
            );
            controller.request_full_redraw();
        }
        ui.invoke_frame_ready();
    });

    let weak_ui = ui.as_weak();
    ui.on_close_tab(move |id| {
        if let Some(ui) = weak_ui.upgrade() {
            close_tab(&ui, &tabs, &awaiting_full_frame, id);
        }
    });
}

/// 自绘标题栏通过位移量拖动原生窗口。
pub(super) fn connect_window_controls(ui: &MainWindow) {
    let weak_ui = ui.as_weak();
    ui.on_drag_window(move |delta_x, delta_y| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        let window = ui.window();
        if window.is_maximized() {
            return;
        }
        let scale = window.scale_factor();
        let position = window.position();
        window.set_position(PhysicalPosition::new(
            position.x + (delta_x * scale).round() as i32,
            position.y + (delta_y * scale).round() as i32,
        ));
    });
}
