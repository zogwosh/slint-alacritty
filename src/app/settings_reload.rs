//! 轮询用户可编辑的 TOML 设置文件，并把变更同步到运行中的应用。

use super::{
    sessions::TabManager,
    settings::{AppSettings, SettingsWatcher, sync_profile_draft, sync_settings_ui},
};
use crate::MainWindow;
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

pub(super) fn start(
    ui: &MainWindow,
    settings: Rc<RefCell<AppSettings>>,
    mono_fonts: Rc<Vec<String>>,
    tabs: Rc<RefCell<TabManager>>,
    awaiting_full_frame: Rc<Cell<bool>>,
    settings_writable: Rc<Cell<bool>>,
) -> Timer {
    let timer = Timer::default();
    let watcher = Rc::new(RefCell::new(SettingsWatcher::new()));
    let weak_ui = ui.as_weak();
    timer.start(TimerMode::Repeated, Duration::from_millis(750), move || {
        let Some(result) = watcher.borrow_mut().poll() else {
            return;
        };
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        let (next, message, error) = match result {
            Ok(settings) => (settings, "settings.toml 已重新加载。".to_owned(), false),
            Err(message) => (
                AppSettings::built_in_defaults(),
                format!("设置文件错误：{message}。当前使用内置默认设置，请修正 settings.toml。"),
                true,
            ),
        };
        let font_changed = {
            let current = settings.borrow();
            current.font_family != next.font_family || current.font_size != next.font_size
        };
        *settings.borrow_mut() = next;
        settings_writable.set(!error);
        let settings_ref = settings.borrow();
        sync_settings_ui(&ui, &settings_ref, &mono_fonts);
        let selected = ui.get_selected_profile_id();
        let profile = settings_ref
            .profile(selected)
            .or_else(|| settings_ref.default_profile());
        sync_profile_draft(&ui, profile);
        drop(settings_ref);
        ui.set_settings_message_error(error);
        ui.set_settings_message(message.into());
        if font_changed {
            awaiting_full_frame.set(true);
            if let Some((active_id, controller)) = tabs
                .borrow()
                .selected_session()
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
    });
    timer
}
