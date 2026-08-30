mod backend;
mod command;
mod controller;
mod frame;
mod input;
mod mouse;
mod palette;
mod worker;

pub(crate) use controller::TerminalController;
#[cfg(test)]
pub(crate) use frame::{CursorPatch, RowPatch};
pub(crate) use frame::{FramePatch, TerminalCellPatch};
pub(crate) use input::KeyInput;
pub(crate) use palette::RgbColor;
