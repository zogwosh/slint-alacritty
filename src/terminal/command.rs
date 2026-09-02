//! UI 线程发给终端工作线程的命令与参数。

use super::input::KeyInput;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::vte::ansi::Rgb;
use std::sync::{Arc, Mutex};

/// 高频“只关心最新值”命令的单槽状态；queued 与 value 在同一锁下避免丢失唤醒。
pub(super) struct CoalescedCommand<T> {
    pub(super) value: Option<T>,
    pub(super) queued: bool,
}

impl<T> Default for CoalescedCommand<T> {
    fn default() -> Self {
        Self {
            value: None,
            queued: false,
        }
    }
}

/// 终端网格尺寸及每个字符单元的像素尺寸。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TerminalSize {
    pub(super) columns: usize,
    pub(super) rows: usize,
    pub(super) cell_width: u16,
    pub(super) cell_height: u16,
}

impl TerminalSize {
    pub(super) fn new(columns: usize, rows: usize, cell_width: f32, cell_height: f32) -> Self {
        Self {
            columns,
            rows,
            cell_width: cell_width.ceil().clamp(1.0, u16::MAX as f32) as u16,
            cell_height: cell_height.ceil().clamp(1.0, u16::MAX as f32) as u16,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::TerminalSize;

    #[test]
    fn terminal_size_tracks_and_clamps_cell_pixels() {
        let size = TerminalSize::new(80, 24, 8.2, 17.1);
        assert_eq!(size.cell_width, 9);
        assert_eq!(size.cell_height, 18);

        let minimum = TerminalSize::new(80, 24, 0.0, -1.0);
        assert_eq!(minimum.cell_width, 1);
        assert_eq!(minimum.cell_height, 1);
    }
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
    Resize(Arc<Mutex<CoalescedCommand<TerminalSize>>>),
    PasteClipboard,
    ClipboardStore(String),
    ClipboardLoad(Arc<dyn Fn(&str) -> String + Send + Sync + 'static>),
    ColorRequest(usize, Arc<dyn Fn(Rgb) -> String + Send + Sync + 'static>),
    Mouse(MouseInput),
    MouseScroll(MouseScrollInput),
    ScrollTo(Arc<Mutex<CoalescedCommand<usize>>>),
    CopySelection,
    SelectAll,
    Search(String),
    SearchStep(bool),
    ForceFullRedraw,
    Render,
    Shutdown,
}
