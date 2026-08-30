use crate::{
    MainWindow, TerminalCell, TerminalRow,
    terminal::{FramePatch, KeyInput, RgbColor, TerminalCellPatch, TerminalController},
};
use slint::{Color, ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, error::Error, rc::Rc};

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
    let viewport = Rc::new(RefCell::new(UiViewport::new(initial_rows)));
    ui.set_rows(viewport.borrow().model());

    connect_input(&ui, controller.clone());
    connect_resize(&ui, controller.clone());
    connect_mouse(&ui, controller.clone());
    connect_frame_updates(&ui, controller, viewport);

    ui.run()?;
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
    viewport: Rc<RefCell<UiViewport>>,
) {
    let weak_ui = ui.as_weak();
    ui.on_frame_ready(move || {
        let Some(frame) = controller.take_latest_frame() else {
            return;
        };
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };

        viewport.borrow_mut().apply(&ui, frame);
    });
}

struct UiViewport {
    rows: Rc<VecModel<TerminalRow>>,
    cell_models: Vec<Rc<VecModel<TerminalCell>>>,
    generation: u64,
    columns: usize,
}

impl UiViewport {
    fn new(row_count: usize) -> Self {
        let rows = Rc::new(VecModel::default());
        let mut viewport = Self {
            rows,
            cell_models: Vec::new(),
            generation: 0,
            columns: 0,
        };
        viewport.resize(row_count);
        viewport
    }

    fn model(&self) -> ModelRc<TerminalRow> {
        self.rows.clone().into()
    }

    fn apply(&mut self, ui: &MainWindow, frame: FramePatch) {
        if frame.generation < self.generation {
            return;
        }

        if frame.generation != self.generation
            || frame.rows != self.cell_models.len()
            || frame.columns != self.columns
        {
            self.resize(frame.rows);
            self.generation = frame.generation;
            self.columns = frame.columns;
        }

        if frame.full_redraw && frame.changed_rows.len() != frame.rows {
            return;
        }

        for row in frame.changed_rows {
            if let Some(model) = self.cell_models.get(row.row) {
                model.set_vec(row.cells.into_iter().map(to_ui_cell).collect::<Vec<_>>());
            }
        }

        ui.set_cursor_column(frame.cursor.column as i32);
        ui.set_cursor_row(frame.cursor.row as i32);
        ui.set_cursor_visible(frame.cursor.visible);
        ui.set_cursor_shape(frame.cursor.shape);
        ui.set_cursor_blinking(frame.cursor.blinking);
        if let Some(title) = frame.title {
            ui.set_terminal_title(title.into());
        }
        if let Some(message) = frame.exit_message {
            ui.set_terminal_active(false);
            ui.set_exit_message(message.into());
        }
    }

    fn resize(&mut self, row_count: usize) {
        self.cell_models = (0..row_count)
            .map(|_| Rc::new(VecModel::default()))
            .collect();
        self.rows.set_vec(
            self.cell_models
                .iter()
                .map(|cells| TerminalRow {
                    cells: ModelRc::from(cells.clone()),
                })
                .collect::<Vec<_>>(),
        );
    }
}

fn to_ui_cell(cell: TerminalCellPatch) -> TerminalCell {
    TerminalCell {
        text: cell.text.into(),
        foreground: to_slint_color(cell.foreground),
        background: to_slint_color(cell.background),
        column: cell.column as i32,
        width_in_columns: cell.width_in_columns as i32,
        bold: cell.bold,
        italic: cell.italic,
        underline_style: cell.underline_style,
        underline_decoration: cell.underline_decoration.into(),
        strikeout: cell.strikeout,
        hidden: cell.hidden,
    }
}

fn normalize_key(text: &str) -> Option<KeyInput> {
    use slint::platform::Key;

    let is = |key: Key| text == SharedString::from(key).as_str();
    let input = if is(Key::Shift)
        || is(Key::ShiftR)
        || is(Key::Control)
        || is(Key::ControlR)
        || is(Key::Alt)
        || is(Key::AltGr)
        || is(Key::Meta)
        || is(Key::MetaR)
        || is(Key::CapsLock)
    {
        return None;
    } else if is(Key::Return) {
        KeyInput::Return
    } else if is(Key::Backspace) {
        KeyInput::Backspace
    } else if is(Key::Tab) {
        KeyInput::Tab
    } else if is(Key::Escape) {
        KeyInput::Escape
    } else if is(Key::UpArrow) {
        KeyInput::Up
    } else if is(Key::DownArrow) {
        KeyInput::Down
    } else if is(Key::RightArrow) {
        KeyInput::Right
    } else if is(Key::LeftArrow) {
        KeyInput::Left
    } else if is(Key::Home) {
        KeyInput::Home
    } else if is(Key::End) {
        KeyInput::End
    } else if is(Key::Delete) {
        KeyInput::Delete
    } else if is(Key::PageUp) {
        KeyInput::PageUp
    } else if is(Key::PageDown) {
        KeyInput::PageDown
    } else if is(Key::Insert) {
        KeyInput::Insert
    } else if is(Key::Backtab) {
        KeyInput::Backtab
    } else if is(Key::F1) {
        KeyInput::Function(1)
    } else if is(Key::F2) {
        KeyInput::Function(2)
    } else if is(Key::F3) {
        KeyInput::Function(3)
    } else if is(Key::F4) {
        KeyInput::Function(4)
    } else if is(Key::F5) {
        KeyInput::Function(5)
    } else if is(Key::F6) {
        KeyInput::Function(6)
    } else if is(Key::F7) {
        KeyInput::Function(7)
    } else if is(Key::F8) {
        KeyInput::Function(8)
    } else if is(Key::F9) {
        KeyInput::Function(9)
    } else if is(Key::F10) {
        KeyInput::Function(10)
    } else if is(Key::F11) {
        KeyInput::Function(11)
    } else if is(Key::F12) {
        KeyInput::Function(12)
    } else if text.is_empty() {
        return None;
    } else {
        KeyInput::Text(text.into())
    };
    Some(input)
}

fn to_slint_color(color: RgbColor) -> Color {
    Color::from_rgb_u8(color.red, color.green, color.blue)
}
