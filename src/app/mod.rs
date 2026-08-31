//! 应用入口：组装 Slint 界面、终端会话和 GPU 渲染器。

mod bindings;
mod input;
mod platform;
mod rendering;
mod sessions;

use crate::{MainWindow, renderer::GpuTerminalRenderer};
use sessions::{TabManager, create_session, sync_tab_ui};
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
    let initial_columns = ui.get_viewport_columns().max(2) as usize;
    let initial_rows = ui.get_viewport_rows().max(1) as usize;
    let first_session = create_session(&ui, 1, initial_columns, initial_rows)?;
    let tabs = Rc::new(RefCell::new(TabManager {
        sessions: vec![first_session],
        active: 0,
        next_id: 2,
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
        awaiting_full_frame.clone(),
    )?;
    bindings::connect_input(&ui, tabs.clone(), awaiting_full_frame.clone());
    bindings::connect_resize(&ui, tabs.clone());
    bindings::connect_mouse(&ui, tabs.clone());
    bindings::connect_tabs(&ui, tabs.clone(), awaiting_full_frame.clone());
    bindings::connect_window_controls(&ui);
    rendering::connect_frame_updates(&ui, tabs, renderer.clone(), awaiting_full_frame);

    // 光标闪烁是渲染状态，不需要推动终端解析器生成新帧。
    let cursor_timer = Timer::default();
    let timer_renderer = renderer;
    let weak_ui = ui.as_weak();
    cursor_timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        let redraw = timer_renderer
            .borrow_mut()
            .as_mut()
            .is_some_and(GpuTerminalRenderer::tick_cursor);
        if redraw && let Some(ui) = weak_ui.upgrade() {
            ui.window().request_redraw();
        }
    });

    ui.run()?;
    Ok(())
}
