//! 应用入口：组装 Slint 界面、终端会话和 GPU 渲染器。

mod bindings;
mod input;
mod platform;
mod rendering;
mod sessions;
pub(crate) mod settings;
mod settings_reload;

use crate::{MainWindow, renderer::GpuTerminalRenderer};
use sessions::{TabManager, create_session, sync_tab_ui, viewport_grid};
use settings::{
    load_or_create, mono_font_families, sync_profile_draft, sync_settings_limits, sync_settings_ui,
};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    error::Error,
    rc::Rc,
    time::Duration,
};

/// 创建窗口和初始会话，连接各职责模块后进入 Slint 事件循环。
pub(crate) fn run() -> Result<(), Box<dyn Error>> {
    let ui = MainWindow::new()?;
    let initial_settings = load_or_create();
    let settings_writable = Rc::new(Cell::new(initial_settings.writable));
    let initial_error = initial_settings.error;
    let settings = Rc::new(RefCell::new(initial_settings.settings));
    let mono_fonts = Rc::new(mono_font_families());
    sync_settings_limits(&ui);
    sync_settings_ui(&ui, &settings.borrow(), &mono_fonts);
    sync_profile_draft(&ui, settings.borrow().default_profile());
    if let Some(error) = initial_error {
        ui.set_settings_message_error(true);
        ui.set_settings_message(error.into());
    }
    let (initial_columns, initial_rows) = viewport_grid(&ui);
    let first_session = {
        let settings = settings.borrow();
        create_session(
            &ui,
            1,
            initial_columns,
            initial_rows,
            settings.default_profile(),
            settings.terminal_options(),
        )?
    };
    let tabs = Rc::new(RefCell::new(TabManager {
        sessions: vec![first_session],
        active: 0,
        next_id: 2,
        settings_open: false,
        settings_active: false,
    }));
    sync_tab_ui(&ui, &tabs.borrow());

    // 渲染器只能在 Slint 建立 WGPU 上下文后创建，所以初始值为空。
    let renderer = Rc::new(RefCell::new(None::<GpuTerminalRenderer>));
    // 切换标签或重建纹理后，必须先收到完整帧，不能直接套用增量补丁。
    let awaiting_full_frame = Rc::new(Cell::new(true));

    rendering::install_renderer(
        &ui,
        renderer.clone(),
        tabs.clone(),
        settings.clone(),
        awaiting_full_frame.clone(),
    )?;
    bindings::connect_input(
        &ui,
        tabs.clone(),
        settings.clone(),
        awaiting_full_frame.clone(),
    );
    bindings::connect_resize(&ui, tabs.clone());
    bindings::connect_mouse(&ui, tabs.clone());
    bindings::connect_search(&ui, tabs.clone());
    bindings::connect_tabs(
        &ui,
        tabs.clone(),
        settings.clone(),
        mono_fonts.clone(),
        awaiting_full_frame.clone(),
        settings_writable.clone(),
    );
    bindings::connect_window_controls(&ui);
    rendering::connect_frame_updates(
        &ui,
        tabs.clone(),
        renderer.clone(),
        awaiting_full_frame.clone(),
    );
    rendering::connect_ime(&ui, renderer.clone());

    let _settings_timer = settings_reload::start(
        &ui,
        settings,
        mono_fonts,
        tabs,
        awaiting_full_frame,
        settings_writable,
    );

    // 光标闪烁是渲染状态，不需要推动终端解析器生成新帧。
    let cursor_timer = Timer::default();
    let timer_renderer = renderer;
    let weak_ui = ui.as_weak();
    cursor_timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        // 终端被设置页覆盖或窗口最小化时，闪烁不可见，不值得驱动 GPU 与窗口重绘。
        if ui.get_settings_active() || ui.window().is_minimized() {
            return;
        }
        let redraw = timer_renderer
            .borrow_mut()
            .as_mut()
            .is_some_and(GpuTerminalRenderer::tick_cursor);
        if redraw {
            ui.window().request_redraw();
        }
    });

    ui.run()?;
    Ok(())
}
