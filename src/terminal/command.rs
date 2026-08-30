use super::input::KeyInput;
use alacritty_terminal::grid::Dimensions;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TerminalSize {
    pub(super) columns: usize,
    pub(super) rows: usize,
}

impl Dimensions for TerminalSize {
    fn columns(&self) -> usize {
        self.columns
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn total_lines(&self) -> usize {
        self.rows
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseButton {
    Left,
    Middle,
    Right,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseAction {
    Press,
    Release,
    Move,
    Cancel,
    DoubleClick,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MouseInput {
    pub(super) column: usize,
    pub(super) row: usize,
    pub(super) button: MouseButton,
    pub(super) action: MouseAction,
    pub(super) shift: bool,
    pub(super) alt: bool,
    pub(super) control: bool,
    pub(super) right_half: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MouseScrollInput {
    pub(super) column: usize,
    pub(super) row: usize,
    pub(super) lines: f32,
    pub(super) shift: bool,
    pub(super) alt: bool,
    pub(super) control: bool,
}

pub(super) enum WorkerMessage {
    Input {
        input: KeyInput,
        control: bool,
        alt: bool,
        shift: bool,
        altgr: bool,
    },
    Resize(TerminalSize),
    PasteClipboard,
    ClipboardStore(String),
    ClipboardLoad(Arc<dyn Fn(&str) -> String + Send + Sync + 'static>),
    Mouse(MouseInput),
    MouseScroll(MouseScrollInput),
    CopySelection,
    SelectAll,
    ForceFullRedraw,
    Render,
    Shutdown,
}
