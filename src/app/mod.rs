mod input;

use self::input::normalize_key;
use crate::{
    MainWindow,
    renderer::GpuTerminalRenderer,
    terminal::{FramePatch, TerminalController},
};
use slint::{ComponentHandle, Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    error::Error,
    rc::Rc,
    time::Duration,
};

pub(crate) fn run() -> Result<(), Box<dyn Error>> {
    let ui = MainWindow::new()?;
    let initial_columns = ui.get_viewport_columns().max(2) as usize;
    let initial_rows = ui.get_viewport_rows().max(1) as usize;
    let weak_ui = ui.as_weak();
    let controller = Rc::new(TerminalController::new(
        initial_columns,
        initial_rows,
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

    let renderer = Rc::new(RefCell::new(None::<GpuTerminalRenderer>));
    let awaiting_full_frame = Rc::new(Cell::new(true));

    install_renderer(
        &ui,
        renderer.clone(),
        controller.clone(),
        awaiting_full_frame.clone(),
    )?;
    connect_input(&ui, controller.clone());
    connect_resize(&ui, controller.clone());
    connect_mouse(&ui, controller.clone());
    connect_frame_updates(&ui, controller, renderer.clone(), awaiting_full_frame);

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

fn install_renderer(
    ui: &MainWindow,
    renderer: Rc<RefCell<Option<GpuTerminalRenderer>>>,
    controller: Rc<TerminalController>,
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
                let size = ui.window().size();
                let scale = ui.window().scale_factor();
                let gpu = GpuTerminalRenderer::new(
                    device,
                    queue,
                    size.width.max(1),
                    size.height.max(1),
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
                controller.request_full_redraw();
            }
            (slint::RenderingState::BeforeRendering, slint::GraphicsAPI::WGPU29 { .. }) => {
                let Some(ui) = weak_ui.upgrade() else {
                    return;
                };
                let size = ui.window().size();
                let scale = ui.window().scale_factor();
                if let Some(gpu) = renderer.borrow_mut().as_mut() {
                    let texture_changed = gpu.sync_surface(
                        size.width.max(1),
                        size.height.max(1),
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

fn connect_input(ui: &MainWindow, controller: Rc<TerminalController>) {
    ui.on_key_input(move |text, control, alt, shift, altgr| {
        if control && shift && text.eq_ignore_ascii_case("v") {
            controller.paste_clipboard();
            return;
        }
        if control && shift && text.eq_ignore_ascii_case("c") {
            controller.copy_selection();
            return;
        }
        if let Some(input) = normalize_key(text.as_str()) {
            controller.send_key(input, control, alt, shift, altgr);
        }
    });
}

fn connect_mouse(ui: &MainWindow, controller: Rc<TerminalController>) {
    let mouse_controller = controller.clone();
    ui.on_mouse_input(
        move |column, row, button, action, shift, alt, control, right_half| {
            mouse_controller.mouse_input(
                column.max(0) as usize,
                row.max(0) as usize,
                button,
                action,
                shift,
                alt,
                control,
                right_half,
            );
        },
    );
    ui.on_mouse_scroll(move |column, row, lines, shift, alt, control| {
        controller.mouse_scroll(
            column.max(0) as usize,
            row.max(0) as usize,
            lines,
            shift,
            alt,
            control,
        );
    });
}

fn connect_resize(ui: &MainWindow, controller: Rc<TerminalController>) {
    ui.on_viewport_resized(move |columns, rows| {
        controller.resize(columns.max(2) as usize, rows.max(1) as usize);
    });
}

fn connect_frame_updates(
    ui: &MainWindow,
    controller: Rc<TerminalController>,
    renderer: Rc<RefCell<Option<GpuTerminalRenderer>>>,
    awaiting_full_frame: Rc<Cell<bool>>,
) {
    let weak_ui = ui.as_weak();
    ui.on_frame_ready(move || {
        let Some(frame) = controller.take_latest_frame() else {
            return;
        };
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };

        apply_frame_metadata(&ui, &frame);
        let mut renderer_ref = renderer.borrow_mut();
        let Some(gpu) = renderer_ref.as_mut() else {
            awaiting_full_frame.set(true);
            return;
        };

        if awaiting_full_frame.get() && !frame.full_redraw {
            controller.request_full_redraw();
            return;
        }

        let size = ui.window().size();
        let scale = ui.window().scale_factor();
        let texture_changed = gpu.sync_surface(
            size.width.max(1),
            size.height.max(1),
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

fn apply_frame_metadata(ui: &MainWindow, frame: &FramePatch) {
    if let Some(title) = &frame.title {
        ui.set_terminal_title(title.as_str().into());
    }
    if let Some(message) = &frame.exit_message {
        ui.set_terminal_active(false);
        ui.set_exit_message(message.as_str().into());
    }
}
