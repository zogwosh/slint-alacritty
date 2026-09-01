//! Slint 输入、标签页、尺寸和窗口控制回调。

use super::{
    input::{KeyAction, configured_key_action, normalize_key},
    sessions::{TabManager, add_tab, close_tab, sync_tab_ui},
    settings::{AppSettings, build_profile, sync_profile_draft, sync_settings_ui, update_shortcut},
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
    settings: Rc<RefCell<AppSettings>>,
    awaiting_full_frame: Rc<Cell<bool>>,
) {
    let weak_ui = ui.as_weak();
    ui.on_key_input(move |text, control, alt, shift, altgr| {
        let decision = configured_key_action(
            &settings.borrow(),
            text.as_str(),
            control,
            alt,
            shift,
            altgr,
        );
        match decision.action {
            Some(KeyAction::NewTab) => {
                if let Some(ui) = weak_ui.upgrade() {
                    let settings = settings.borrow();
                    add_tab(&ui, &tabs, &awaiting_full_frame, settings.default_profile());
                }
            }
            Some(KeyAction::CloseTab) => {
                let active_id = tabs.borrow().active_id();
                if let (Some(ui), Some(id)) = (weak_ui.upgrade(), active_id) {
                    close_tab(&ui, &tabs, &awaiting_full_frame, id);
                }
            }
            Some(KeyAction::Copy) => {
                if let Some(controller) = tabs.borrow().active_controller() {
                    controller.copy_selection();
                }
            }
            Some(KeyAction::SelectAll) => {
                if let Some(controller) = tabs.borrow().active_controller() {
                    controller.select_all();
                }
            }
            Some(KeyAction::Paste) => {
                if let Some(controller) = tabs.borrow().active_controller() {
                    controller.paste_clipboard();
                }
            }
            Some(KeyAction::Interrupt) => {
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
            Some(KeyAction::Quit) => {
                if let Some(ui) = weak_ui.upgrade() {
                    let _ = ui.window().hide();
                }
            }
            None => {}
        }
        if decision.forward
            && let (Some(controller), Some(input)) = (
                tabs.borrow().active_controller(),
                normalize_key(text.as_str()),
            )
        {
            controller.send_key(input, control, alt, shift, altgr);
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
            controller.resize(columns as usize, rows as usize, cell_width, cell_height);
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
    settings: Rc<RefCell<AppSettings>>,
    mono_fonts: Rc<Vec<String>>,
    awaiting_full_frame: Rc<Cell<bool>>,
    settings_writable: Rc<Cell<bool>>,
) {
    let weak_ui = ui.as_weak();
    let new_tabs = tabs.clone();
    let new_settings = settings.clone();
    let new_awaiting = awaiting_full_frame.clone();
    ui.on_new_tab(move || {
        if let Some(ui) = weak_ui.upgrade() {
            let settings = new_settings.borrow();
            add_tab(&ui, &new_tabs, &new_awaiting, settings.default_profile());
        }
    });

    let weak_ui = ui.as_weak();
    let settings_tabs = tabs.clone();
    ui.on_open_settings(move || {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        let mut manager = settings_tabs.borrow_mut();
        manager.settings_open = true;
        manager.settings_active = true;
        sync_tab_ui(&ui, &manager);
    });

    let weak_ui = ui.as_weak();
    let select_tabs = tabs.clone();
    let select_awaiting = awaiting_full_frame.clone();
    ui.on_select_tab(move |id| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        let mut manager = select_tabs.borrow_mut();
        if id == -1 && manager.settings_open {
            manager.settings_active = true;
            sync_tab_ui(&ui, &manager);
            return;
        }
        let Some(index) = manager.sessions.iter().position(|session| session.id == id) else {
            return;
        };
        if index == manager.active && !manager.settings_active {
            return;
        }
        manager.active = index;
        manager.settings_active = false;
        sync_tab_ui(&ui, &manager);
        let controller = manager.active_controller();
        drop(manager);
        // 每个会话有独立帧历史；切换后要求活动会话重发完整画面。
        select_awaiting.set(true);
        if let Some(controller) = controller {
            controller.resize(
                ui.get_viewport_columns().max(2) as usize,
                ui.get_viewport_rows().max(1) as usize,
                ui.get_cell_width(),
                ui.get_cell_height(),
            );
            controller.request_full_redraw();
        }
        ui.invoke_frame_ready(id);
    });

    let weak_ui = ui.as_weak();
    let close_tabs = tabs.clone();
    let close_awaiting = awaiting_full_frame.clone();
    ui.on_close_tab(move |id| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        if id == -1 {
            let mut manager = close_tabs.borrow_mut();
            manager.settings_open = false;
            manager.settings_active = false;
            if manager.sessions.is_empty() {
                drop(manager);
                let _ = ui.window().hide();
                return;
            }
            sync_tab_ui(&ui, &manager);
            let active_session = manager
                .active_session()
                .map(|session| (session.id, session.controller.clone()));
            drop(manager);
            close_awaiting.set(true);
            if let Some((active_id, controller)) = active_session {
                controller.request_full_redraw();
                ui.invoke_frame_ready(active_id);
            }
        } else {
            close_tab(&ui, &close_tabs, &close_awaiting, id);
        }
    });

    let weak_ui = ui.as_weak();
    let select_settings = settings.clone();
    ui.on_select_profile(move |id| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        let settings = select_settings.borrow();
        sync_profile_draft(&ui, settings.profile(id));
        ui.set_settings_message("".into());
    });

    let weak_ui = ui.as_weak();
    ui.on_new_profile(move || {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        sync_profile_draft(&ui, None);
        ui.set_settings_message_error(false);
        ui.set_settings_message("填写信息后保存为新的 Shell Profile。".into());
    });

    let weak_ui = ui.as_weak();
    let save_settings = settings.clone();
    let save_fonts = mono_fonts.clone();
    let save_writable = settings_writable.clone();
    ui.on_save_profile(
        move |id, name, program, arguments, working_directory, environment| {
            let Some(ui) = weak_ui.upgrade() else {
                return;
            };
            if !ensure_settings_writable(&ui, &save_writable) {
                return;
            }
            let mut settings = save_settings.borrow_mut();
            let mut profile = match build_profile(
                id,
                name.as_str(),
                program.as_str(),
                arguments.as_str(),
                working_directory.as_str(),
                environment.as_str(),
            ) {
                Ok(profile) => profile,
                Err(message) => {
                    ui.set_settings_message_error(true);
                    ui.set_settings_message(message.into());
                    return;
                }
            };
            let resolved_id = if id < 0 {
                settings.allocate_profile_id()
            } else {
                id
            };
            profile.id = resolved_id;
            if let Some(existing) = settings.profile_mut(resolved_id) {
                *existing = profile;
            } else {
                settings.profiles.push(profile);
            }
            if settings.default_profile_id.is_none() {
                settings.default_profile_id = Some(resolved_id);
            }
            match settings.save() {
                Ok(()) => {
                    ui.set_settings_message_error(false);
                    ui.set_settings_message("Shell Profile 已保存。".into());
                    sync_settings_ui(&ui, &settings, &save_fonts);
                    sync_profile_draft(&ui, settings.profile(resolved_id));
                }
                Err(error) => {
                    ui.set_settings_message_error(true);
                    ui.set_settings_message(format!("错误：保存设置失败：{error}").into());
                }
            }
        },
    );

    let weak_ui = ui.as_weak();
    let default_settings = settings.clone();
    let default_fonts = mono_fonts.clone();
    let default_writable = settings_writable.clone();
    ui.on_set_default_profile(move |id| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        if !ensure_settings_writable(&ui, &default_writable) {
            return;
        }
        let mut settings = default_settings.borrow_mut();
        if settings.profile(id).is_none() {
            return;
        }
        settings.default_profile_id = Some(id);
        match settings.save() {
            Ok(()) => {
                ui.set_settings_message_error(false);
                ui.set_settings_message("默认 Profile 已更新；现有终端不会被重启。".into());
                sync_settings_ui(&ui, &settings, &default_fonts);
            }
            Err(error) => {
                ui.set_settings_message_error(true);
                ui.set_settings_message(format!("错误：保存设置失败：{error}").into());
            }
        }
    });

    let weak_ui = ui.as_weak();
    let remove_settings = settings.clone();
    let remove_fonts = mono_fonts.clone();
    let remove_writable = settings_writable.clone();
    ui.on_remove_profile(move |id| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        if !ensure_settings_writable(&ui, &remove_writable) {
            return;
        }
        let mut settings = remove_settings.borrow_mut();
        settings.profiles.retain(|profile| profile.id != id);
        if settings.default_profile_id == Some(id) {
            settings.default_profile_id = settings.profiles.first().map(|profile| profile.id);
        }
        match settings.save() {
            Ok(()) => {
                ui.set_settings_message_error(false);
                ui.set_settings_message("Shell Profile 已删除。".into());
                sync_settings_ui(&ui, &settings, &remove_fonts);
                let next_profile = settings
                    .default_profile()
                    .or_else(|| settings.profiles.first());
                sync_profile_draft(&ui, next_profile);
            }
            Err(error) => {
                ui.set_settings_message_error(true);
                ui.set_settings_message(format!("错误：保存设置失败：{error}").into());
            }
        }
    });

    let font_settings = settings.clone();
    let font_fonts = mono_fonts.clone();
    let font_writable = settings_writable.clone();
    let weak_ui = ui.as_weak();
    ui.on_save_font(move |index, size| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        if !ensure_settings_writable(&ui, &font_writable) {
            return;
        }
        let Some(family) = font_fonts.get(index.max(0) as usize) else {
            ui.set_settings_message_error(true);
            ui.set_settings_message("错误：请选择有效的等宽字体".into());
            return;
        };
        let mut settings = font_settings.borrow_mut();
        settings.font_family = family.clone();
        settings.font_size = size.clamp(8, 36);
        match settings.save() {
            Ok(()) => {
                ui.set_settings_message_error(false);
                sync_settings_ui(&ui, &settings, &font_fonts);
                ui.set_settings_message("字体设置已应用。".into());
                awaiting_full_frame.set(true);
                if let Some((active_id, controller)) = tabs
                    .borrow()
                    .active_session()
                    .map(|session| (session.id, session.controller.clone()))
                {
                    controller.resize(
                        ui.get_viewport_columns().max(2) as usize,
                        ui.get_viewport_rows().max(1) as usize,
                        ui.get_cell_width(),
                        ui.get_cell_height(),
                    );
                    controller.request_full_redraw();
                    ui.invoke_frame_ready(active_id);
                }
                ui.window().request_redraw();
            }
            Err(error) => {
                ui.set_settings_message_error(true);
                ui.set_settings_message(format!("错误：保存设置失败：{error}").into());
            }
        }
    });

    let weak_ui = ui.as_weak();
    let shortcut_writable = settings_writable;
    ui.on_save_shortcut(move |action, shortcut, pass_through| {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        if !ensure_settings_writable(&ui, &shortcut_writable) {
            return;
        }
        let mut settings = settings.borrow_mut();
        if let Err(message) = update_shortcut(
            &mut settings,
            action.as_str(),
            shortcut.as_str(),
            pass_through,
        ) {
            ui.set_settings_message_error(true);
            ui.set_settings_message(message.into());
            return;
        }
        match settings.save() {
            Ok(()) => {
                ui.set_settings_message_error(false);
                ui.set_settings_message("快捷键设置已应用。".into());
                sync_settings_ui(&ui, &settings, &mono_fonts);
            }
            Err(error) => {
                ui.set_settings_message_error(true);
                ui.set_settings_message(format!("错误：保存设置失败：{error}").into());
            }
        }
    });
}

fn ensure_settings_writable(ui: &MainWindow, writable: &Cell<bool>) -> bool {
    if writable.get() {
        return true;
    }
    ui.set_settings_message_error(true);
    ui.set_settings_message(
        "错误：settings.toml 当前无效，请先修改配置文件；应用不会覆盖该文件。".into(),
    );
    false
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
