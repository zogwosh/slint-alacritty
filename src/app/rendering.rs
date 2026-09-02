//! Slint WGPU 生命周期与终端帧提交之间的桥接。

use super::{
    platform::apply_native_window_rounding,
    sessions::{TabManager, apply_frame_metadata, physical_cell_size, terminal_theme},
    settings::{AppSettings, sync_font_metrics},
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

/// 窗口缩放变化（跨显示器拖动、系统缩放调整）时重新测量物理单元尺寸。
fn refresh_scale_if_changed(ui: &MainWindow, settings: &RefCell<AppSettings>) -> f32 {
    let scale = ui.window().scale_factor();
    if (ui.get_scale_factor() - scale).abs() > f32::EPSILON {
        sync_font_metrics(ui, &settings.borrow());
    }
    scale
}

/// 把渲染器当前的物理表面、单元度量和字体与界面状态对齐；返回值表示纹理已重建。
fn sync_renderer_surface(ui: &MainWindow, gpu: &mut GpuTerminalRenderer, scale: f32) -> bool {
    let (width, height) = terminal_surface_size(ui, scale);
    let (cell_width, cell_height) = physical_cell_size(ui);
    gpu.sync_surface(
        width,
        height,
        cell_width,
        cell_height,
        ui.get_terminal_font_size() as f32 * scale,
        ui.get_terminal_font_family().as_str(),
    )
}

fn publish_texture(ui: &MainWindow, gpu: &GpuTerminalRenderer, context: &str) {
    match gpu.image() {
        Ok(image) => ui.set_terminal_frame(image),
        Err(error) => eprintln!("failed to import {context} terminal texture: {error}"),
    }
}

/// 在 Slint 的 WGPU 生命周期内创建、调整和销毁终端渲染器。
pub(super) fn install_renderer(
    ui: &MainWindow,
    renderer: Rc<RefCell<Option<GpuTerminalRenderer>>>,
    tabs: Rc<RefCell<TabManager>>,
    settings: Rc<RefCell<AppSettings>>,
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
                // 窗口创建后缩放才是确定值；先据此定下物理单元尺寸再建渲染器。
                sync_font_metrics(&ui, &settings.borrow());
                let scale = ui.window().scale_factor();
                let (width, height) = terminal_surface_size(&ui, scale);
                let (cell_width, cell_height) = physical_cell_size(&ui);
                // 复用 Slint 的 device/queue，使生成的纹理可以直接作为 Image 显示。
                let gpu = GpuTerminalRenderer::new(
                    device,
                    queue,
                    width,
                    height,
                    ui.get_viewport_columns().max(2) as usize,
                    ui.get_viewport_rows().max(1) as usize,
                    cell_width,
                    cell_height,
                    ui.get_terminal_font_size() as f32 * scale,
                    ui.get_terminal_font_family().as_str(),
                    terminal_theme(&ui),
                );
                publish_texture(&ui, &gpu, "initial");
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
                let scale = refresh_scale_if_changed(&ui, &settings);
                if let Some(gpu) = renderer.borrow_mut().as_mut() {
                    if sync_renderer_surface(&ui, gpu, scale) {
                        publish_texture(&ui, gpu, "resized");
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
    ui.on_frame_ready(move |session_id| {
        let (controller, is_active) = {
            let manager = tabs.borrow();
            let Some(session) = manager
                .sessions
                .iter()
                .find(|session| session.id == session_id)
            else {
                return;
            };
            (
                session.controller.clone(),
                manager
                    .active_session()
                    .is_some_and(|active| active.id == session_id),
            )
        };
        let Some(frame) = controller.take_latest_frame() else {
            return;
        };
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };

        apply_frame_metadata(&ui, &tabs, session_id, &frame);
        if !is_active || frame.metadata_only {
            return;
        }
        {
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
            let texture_changed = sync_renderer_surface(&ui, gpu, scale);
            if !gpu.apply_frame(&frame) {
                awaiting_full_frame.set(true);
                controller.request_full_redraw();
                return;
            }
            if frame.full_redraw {
                awaiting_full_frame.set(false);
            }
            if texture_changed {
                publish_texture(&ui, gpu, "resized");
            }
        }
        ui.window().request_redraw();
    });
}

/// TextInput 仅提供系统 IME 事件，预编辑文本由终端 GPU 管线自行绘制。
pub(super) fn connect_ime(ui: &MainWindow, renderer: Rc<RefCell<Option<GpuTerminalRenderer>>>) {
    let weak_ui = ui.as_weak();
    ui.on_ime_preedit(move |text| {
        let changed = renderer
            .borrow_mut()
            .as_mut()
            .is_some_and(|renderer| renderer.set_ime_preedit(text.as_str()));
        if changed && let Some(ui) = weak_ui.upgrade() {
            ui.window().request_redraw();
        }
    });
}
