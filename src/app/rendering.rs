//! Slint WGPU 生命周期与终端帧提交之间的桥接。

use super::{
    platform::apply_native_window_rounding,
    sessions::{TabManager, apply_frame_metadata},
};
use crate::{MainWindow, renderer::GpuTerminalRenderer};
use slint::ComponentHandle;
use std::{
    cell::{Cell, RefCell},
    error::Error,
    rc::Rc,
};

/// 把 Slint 的逻辑尺寸换算为 GPU 纹理所需的物理像素尺寸。
fn terminal_surface_size(ui: &MainWindow, scale: f32) -> (u32, u32) {
    let width = (ui.get_terminal_width() * scale).round().max(1.0) as u32;
    let height = (ui.get_terminal_height() * scale).round().max(1.0) as u32;
    (width, height)
}

/// 在 Slint 的 WGPU 生命周期内创建、调整和销毁终端渲染器。
pub(super) fn install_renderer(
    ui: &MainWindow,
    renderer: Rc<RefCell<Option<GpuTerminalRenderer>>>,
    tabs: Rc<RefCell<TabManager>>,
    awaiting_full_frame: Rc<Cell<bool>>,
) -> Result<(), Box<dyn Error>> {
    let weak_ui = ui.as_weak();
    ui.window()
        .set_rendering_notifier(move |state, graphics_api| match (state, graphics_api) {
            (
                slint::RenderingState::RenderingSetup,
                slint::GraphicsAPI::WGPU29 { device, queue, .. },
            ) => {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                apply_native_window_rounding(ui.window());
                let scale = ui.window().scale_factor();
                let (width, height) = terminal_surface_size(&ui, scale);
                // 复用 Slint 的 device/queue，使生成的纹理可以直接作为 Image 显示。
                let gpu = GpuTerminalRenderer::new(
                    device,
                    queue,
                    width,
                    height,
                    ui.get_viewport_columns().max(2) as usize,
                    ui.get_viewport_rows().max(1) as usize,
                    ui.get_cell_width() * scale,
                    ui.get_cell_height() * scale,
                    15.0 * scale,
                );
                match gpu.image() {
                    Ok(image) => ui.set_terminal_frame(image),
                    Err(error) => eprintln!("failed to import terminal WGPU texture: {error}"),
                }
                *renderer.borrow_mut() = Some(gpu);
                awaiting_full_frame.set(true);
                if let Some(controller) = tabs.borrow().active_controller() {
                    controller.request_full_redraw();
                }
            }
            (slint::RenderingState::BeforeRendering, slint::GraphicsAPI::WGPU29 { .. }) => {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                let scale = ui.window().scale_factor();
                let (width, height) = terminal_surface_size(&ui, scale);
                if let Some(gpu) = renderer.borrow_mut().as_mut() {
                    let texture_changed = gpu.sync_surface(
                        width,
                        height,
                        ui.get_cell_width() * scale,
                        ui.get_cell_height() * scale,
                        15.0 * scale,
                    );
                    if texture_changed {
                        match gpu.image() {
                            Ok(image) => ui.set_terminal_frame(image),
                            Err(error) => {
                                eprintln!("failed to import resized terminal texture: {error}")
                            }
                        }
                    }
                    gpu.render_if_needed();
                }
            }
            (slint::RenderingState::RenderingTeardown, _) => {
                *renderer.borrow_mut() = None;
                awaiting_full_frame.set(true);
            }
            _ => {}
        })?;
    Ok(())
}

/// 消费后台发布的最新终端帧，并把有效补丁提交给 GPU 渲染器。
pub(super) fn connect_frame_updates(
    ui: &MainWindow,
    tabs: Rc<RefCell<TabManager>>,
    renderer: Rc<RefCell<Option<GpuTerminalRenderer>>>,
    awaiting_full_frame: Rc<Cell<bool>>,
) {
    let weak_ui = ui.as_weak();
    ui.on_frame_ready(move || {
        let (active_id, controller) = {
            let manager = tabs.borrow();
            let Some(session) = manager.active_session() else {
                return;
            };
            (session.id, session.controller.clone())
        };
        let Some(frame) = controller.take_latest_frame() else {
            return;
        };
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };

        apply_frame_metadata(&ui, &tabs, active_id, &frame);
        let mut renderer_ref = renderer.borrow_mut();
        let Some(gpu) = renderer_ref.as_mut() else {
            awaiting_full_frame.set(true);
            return;
        };

        // 缺少基准完整帧时，增量行无法安全地重建整个网格。
        if awaiting_full_frame.get() && !frame.full_redraw {
            controller.request_full_redraw();
            return;
        }

        let scale = ui.window().scale_factor();
        let (width, height) = terminal_surface_size(&ui, scale);
        let texture_changed = gpu.sync_surface(
            width,
            height,
            ui.get_cell_width() * scale,
            ui.get_cell_height() * scale,
            15.0 * scale,
        );
        if !gpu.apply_frame(&frame) {
            awaiting_full_frame.set(true);
            controller.request_full_redraw();
            return;
        }
        if frame.full_redraw {
            awaiting_full_frame.set(false);
        }
        if texture_changed {
            match gpu.image() {
                Ok(image) => ui.set_terminal_frame(image),
                Err(error) => eprintln!("failed to import resized terminal texture: {error}"),
            }
        }
        ui.window().request_redraw();
    });
}
