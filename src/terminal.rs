//! 终端子系统的模块边界，只向应用层暴露控制器、帧补丁和输入类型。

mod backend;
mod command;
mod controller;
mod frame;
mod input;
mod mouse;
mod notifier;
mod palette;
mod search;
mod ssh;
mod worker;

pub(crate) use command::TerminalOptions;
pub(crate) use controller::TerminalController;
#[cfg(test)]
pub(crate) use frame::{CursorPatch, RowPatch};
pub(crate) use frame::{
    DecorationKind, DecorationRange, FramePatch, FullRedrawReason, TerminalCellPatch,
};
pub(crate) use input::KeyInput;
pub(crate) use palette::{RgbColor, RgbaColor, TerminalTheme};
#[cfg(test)]
pub(crate) use search::SearchSnapshot;
pub(crate) use ssh::respond_to_askpass_if_requested;

/// Alacritty 的环境初始化会修改进程环境，必须在线程创建前且仅执行一次。
pub(crate) fn setup_environment() {
    alacritty_terminal::tty::setup_env();
}
