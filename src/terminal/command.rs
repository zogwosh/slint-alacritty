//! UI 线程发给终端工作线程的命令与参数。

use super::input::KeyInput;
use alacritty_terminal::grid::Dimensions;
use std::sync::Arc;

/// 以字符单元为单位的终端网格尺寸。
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

/// 与具体 UI 框架解耦的鼠标按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseButton {
    Left,
    Middle,
    Right,
    Other,
}

/// 鼠标事件阶段；数值映射在控制器边界完成。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseAction {
    Press,
    Release,
    Move,
    Cancel,
    DoubleClick,
}

/// 一次定位到终端字符单元的鼠标事件。
#[derive(Clone, Copy, Debug)]
pub(super) struct MouseInput {
    pub(super) column: usize,
    pub(super) row: usize,
    pub(super) button: MouseButton,
    pub(super) action: MouseAction,
    pub(super) shift: bool,
    pub(super) alt: bool,
    pub(super) control: bool,
    /// 指针是否落在单元格右半边，用于确定选择区端点方向。
    pub(super) right_half: bool,
}

/// 已换算为字符行数的滚轮输入。
#[derive(Clone, Copy, Debug)]
pub(super) struct MouseScrollInput {
    pub(super) column: usize,
    pub(super) row: usize,
    pub(super) lines: f32,
    pub(super) shift: bool,
    pub(super) alt: bool,
    pub(super) control: bool,
}

/// 终端工作线程串行处理的全部消息。
///
/// 通过单一队列修改终端状态，可以避免 UI、PTY 与渲染线程直接争用状态。
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
    ScrollTo(usize),
    CopySelection,
    SelectAll,
    ForceFullRedraw,
    Render,
    Shutdown,
}
