//! Slint 按键事件到“应用动作”或“终端输入”的第一层分流。

use crate::terminal::KeyInput;
use slint::{SharedString, platform::Key};

/// 应用层对一次按键的处理决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    /// 继续交给终端协议编码器。
    Forward,
    Copy,
    SelectAll,
    Paste,
    Interrupt,
    NewTab,
    CloseTab,
    Ignore,
}

/// 在终端编码之前解析应用快捷键。
///
/// Ctrl+C/A/V 与 Alt+C 是明确的产品策略；其他 Ctrl/Alt 组合也会被应用拦截。
pub(super) fn key_action(
    text: &str,
    control: bool,
    alt: bool,
    shift: bool,
    altgr: bool,
) -> KeyAction {
    if altgr {
        return KeyAction::Forward;
    }

    let exact_control = control && !alt && !shift;
    let exact_control_shift = control && !alt && shift;
    let exact_alt = alt && !control && !shift;
    if exact_control_shift && text.eq_ignore_ascii_case("t") {
        KeyAction::NewTab
    } else if exact_control_shift && text.eq_ignore_ascii_case("w") {
        KeyAction::CloseTab
    } else if exact_control && text.eq_ignore_ascii_case("c") {
        KeyAction::Copy
    } else if exact_control && text.eq_ignore_ascii_case("a") {
        KeyAction::SelectAll
    } else if exact_control && text.eq_ignore_ascii_case("v") {
        KeyAction::Paste
    } else if exact_alt && text.eq_ignore_ascii_case("c") {
        KeyAction::Interrupt
    } else if control || alt {
        KeyAction::Ignore
    } else {
        KeyAction::Forward
    }
}

/// 将 Slint 的按键表示转换成与 UI 框架无关的终端输入模型。
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

#[cfg(test)]
mod tests {
    use super::{KeyAction, key_action};

    #[test]
    fn resolves_supported_shortcuts() {
        assert_eq!(key_action("c", true, false, false, false), KeyAction::Copy);
        assert_eq!(
            key_action("A", true, false, false, false),
            KeyAction::SelectAll
        );
        assert_eq!(key_action("v", true, false, false, false), KeyAction::Paste);
        assert_eq!(key_action("t", true, false, true, false), KeyAction::NewTab);
        assert_eq!(
            key_action("W", true, false, true, false),
            KeyAction::CloseTab
        );
        assert_eq!(
            key_action("C", false, true, false, false),
            KeyAction::Interrupt
        );
    }

    #[test]
    fn blocks_unsupported_shortcuts() {
        assert_eq!(
            key_action("x", true, false, false, false),
            KeyAction::Ignore
        );
        assert_eq!(key_action("c", true, false, true, false), KeyAction::Ignore);
        assert_eq!(key_action("v", true, true, false, false), KeyAction::Ignore);
        assert_eq!(
            key_action("x", false, true, false, false),
            KeyAction::Ignore
        );
    }

    #[test]
    fn forwards_text_and_altgr_input() {
        assert_eq!(
            key_action("x", false, false, false, false),
            KeyAction::Forward
        );
        assert_eq!(
            key_action("X", false, false, true, false),
            KeyAction::Forward
        );
        assert_eq!(key_action("@", true, true, false, true), KeyAction::Forward);
    }
}
