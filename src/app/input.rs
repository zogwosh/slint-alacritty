use crate::terminal::KeyInput;
use slint::{SharedString, platform::Key};

/// Converts Slint's key representation into the terminal-facing input model.
pub(super) fn normalize_key(text: &str) -> Option<KeyInput> {
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
