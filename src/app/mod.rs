mod input;

use self::input::{KeyAction, key_action, normalize_key};
use crate::{
    MainWindow, TabData,
    renderer::GpuTerminalRenderer,
    terminal::{FramePatch, TerminalController},
};
use slint::{ComponentHandle, ModelRc, PhysicalPosition, SharedString, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    error::Error,
    io,
    rc::Rc,
    time::Duration,
};

struct TerminalSession {
    id: i32,
    title: SharedString,
    terminal_active: bool,
    exit_message: SharedString,
    controller: Rc<TerminalController>,
}

struct TabManager {
    sessions: Vec<TerminalSession>,
    active: usize,
    next_id: i32,
}

impl TabManager {
    fn active_session(&self) -> Option<&TerminalSession> {
        self.sessions.get(self.active)
    }

    fn active_controller(&self) -> Option<Rc<TerminalController>> {
        self.active_session()
            .map(|session| session.controller.clone())
    }

    fn active_id(&self) -> Option<i32> {
        self.active_session().map(|session| session.id)
    }
}

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

    let renderer = Rc::new(RefCell::new(None::<GpuTerminalRenderer>));
    let awaiting_full_frame = Rc::new(Cell::new(true));

    install_renderer(
        &ui,
        renderer.clone(),
        tabs.clone(),
        awaiting_full_frame.clone(),
    )?;
    connect_input(&ui, tabs.clone(), awaiting_full_frame.clone());
    connect_resize(&ui, tabs.clone());
    connect_mouse(&ui, tabs.clone());
    connect_tabs(&ui, tabs.clone(), awaiting_full_frame.clone());
    connect_window_controls(&ui);
    connect_frame_updates(&ui, tabs, renderer.clone(), awaiting_full_frame);

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

fn create_session(
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
        controller,
    })
}

fn sync_tab_ui(ui: &MainWindow, manager: &TabManager) {
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
    }
}

fn terminal_surface_size(ui: &MainWindow, scale: f32) -> (u32, u32) {
    let width = (ui.get_terminal_width() * scale).round().max(1.0) as u32;
    let height = (ui.get_terminal_height() * scale).round().max(1.0) as u32;
    (width, height)
}

fn install_renderer(
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

fn connect_input(
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

fn connect_mouse(ui: &MainWindow, tabs: Rc<RefCell<TabManager>>) {
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
    ui.on_mouse_scroll(move |column, row, lines, shift, alt, control| {
        if let Some(controller) = tabs.borrow().active_controller() {
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
}

fn connect_resize(ui: &MainWindow, tabs: Rc<RefCell<TabManager>>) {
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

fn connect_tabs(
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

fn add_tab(ui: &MainWindow, tabs: &Rc<RefCell<TabManager>>, awaiting_full_frame: &Rc<Cell<bool>>) {
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

fn close_tab(
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

fn connect_window_controls(ui: &MainWindow) {
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

#[cfg(target_os = "windows")]
fn apply_native_window_rounding(window: &slint::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::ffi::c_void;

    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut c_void,
            attribute: u32,
            value: *const c_void,
            value_size: u32,
        ) -> i32;
    }

    let handle = window.window_handle();
    let Ok(handle) = handle.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let preference = DWMWCP_ROUND;
    let result = unsafe {
        DwmSetWindowAttribute(
            handle.hwnd.get() as *mut c_void,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&preference as *const i32).cast(),
            std::mem::size_of_val(&preference) as u32,
        )
    };
    if result < 0 {
        eprintln!("failed to enable native window rounding: HRESULT 0x{result:08x}");
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_native_window_rounding(_window: &slint::Window) {}

fn connect_frame_updates(
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

fn apply_frame_metadata(
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
