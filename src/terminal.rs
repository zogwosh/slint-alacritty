//! 终端子系统的模块边界，只向应用层暴露控制器、帧补丁和输入类型。

mod backend;
mod command;
mod controller;
mod frame;
mod input;
mod mouse;
mod notifier;
mod palette;
mod worker;

pub(crate) use controller::TerminalController;
#[cfg(test)]
pub(crate) use frame::{CursorPatch, RowPatch};
pub(crate) use frame::{FramePatch, FullRedrawReason, TerminalCellPatch};
pub(crate) use input::KeyInput;
pub(crate) use palette::RgbColor;
