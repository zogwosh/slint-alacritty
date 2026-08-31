//! Slint 按键事件到“应用动作”或“终端输入”的第一层分流。

use crate::terminal::KeyInput;
use slint::{SharedString, platform::Key};

use super::settings::{AppSettings, ShortcutChord, parse_shortcut};

/// 应用层对一次按键的处理决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    Copy,
    SelectAll,
    Paste,
    Interrupt,
    NewTab,
    CloseTab,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KeyDecision {
    pub(super) action: Option<KeyAction>,
    pub(super) forward: bool,
}

/// 命中的应用快捷键按配置决定是否继续透传；未命中的按键始终交给终端。
pub(super) fn configured_key_action(
    settings: &AppSettings,
    text: &str,
    control: bool,
    alt: bool,
    shift: bool,
    altgr: bool,
) -> KeyDecision {
    let default = KeyDecision {
        action: None,
        forward: true,
    };
    if altgr {
        return default;
    }
    let Some(key) = event_key(text) else {
        return default;
    };
    let pressed = ShortcutChord {
        control,
        alt,
        shift,
        key,
    };
    let Some(setting) = settings.shortcuts.iter().find(|setting| {
        parse_shortcut(&setting.shortcut).is_ok_and(|shortcut| shortcut == pressed)
    }) else {
        return default;
    };
    KeyDecision {
        action: shortcut_action(&setting.action),
        forward: setting.pass_through,
    }
}

fn shortcut_action(action: &str) -> Option<KeyAction> {
    match action {
        "copy" => Some(KeyAction::Copy),
        "select-all" => Some(KeyAction::SelectAll),
        "paste" => Some(KeyAction::Paste),
        "interrupt" => Some(KeyAction::Interrupt),
        "new-tab" => Some(KeyAction::NewTab),
        "close-tab" => Some(KeyAction::CloseTab),
        "quit" => Some(KeyAction::Quit),
        _ => None,
    }
}

fn event_key(text: &str) -> Option<String> {
    let named = [
        (Key::Return, "Enter"),
        (Key::Tab, "Tab"),
        (Key::Escape, "Escape"),
        (Key::Backspace, "Backspace"),
        (Key::Delete, "Delete"),
        (Key::Insert, "Insert"),
        (Key::Home, "Home"),
        (Key::End, "End"),
        (Key::PageUp, "PageUp"),
        (Key::PageDown, "PageDown"),
        (Key::UpArrow, "Up"),
        (Key::DownArrow, "Down"),
        (Key::LeftArrow, "Left"),
        (Key::RightArrow, "Right"),
        (Key::F1, "F1"),
        (Key::F2, "F2"),
        (Key::F3, "F3"),
        (Key::F4, "F4"),
        (Key::F5, "F5"),
        (Key::F6, "F6"),
        (Key::F7, "F7"),
        (Key::F8, "F8"),
        (Key::F9, "F9"),
        (Key::F10, "F10"),
        (Key::F11, "F11"),
        (Key::F12, "F12"),
    ];
    if let Some((_, name)) = named
        .iter()
        .find(|(key, _)| text == SharedString::from(*key).as_str())
    {
        return Some((*name).to_owned());
    }
    let mut characters = text.chars();
    let character = characters.next()?;
    if characters.next().is_some() || character.is_control() {
        return None;
    }
    Some(character.to_uppercase().collect())
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
    use super::{KeyAction, configured_key_action};
    use crate::app::settings::AppSettings;

    #[test]
    fn resolves_supported_shortcuts() {
        let settings = AppSettings::default();
        assert_eq!(
            configured_key_action(&settings, "c", true, false, false, false).action,
            Some(KeyAction::Copy)
        );
        assert_eq!(
            configured_key_action(&settings, "A", true, false, false, false).action,
            Some(KeyAction::SelectAll)
        );
        assert_eq!(
            configured_key_action(&settings, "v", true, false, false, false).action,
            Some(KeyAction::Paste)
        );
        assert_eq!(
            configured_key_action(&settings, "t", true, false, false, false).action,
            Some(KeyAction::NewTab)
        );
        assert_eq!(
            configured_key_action(&settings, "W", true, false, false, false).action,
            Some(KeyAction::CloseTab)
        );
        assert_eq!(
            configured_key_action(&settings, "C", false, true, false, false).action,
            Some(KeyAction::Interrupt)
        );
        assert_eq!(
            configured_key_action(&settings, "q", false, true, false, false).action,
            Some(KeyAction::Quit)
        );
    }

    #[test]
    fn forwards_unsupported_shortcuts() {
        let settings = AppSettings::default();
        for decision in [
            configured_key_action(&settings, "x", true, false, false, false),
            configured_key_action(&settings, "c", true, false, true, false),
            configured_key_action(&settings, "t", true, false, true, false),
            configured_key_action(&settings, "w", true, false, true, false),
            configured_key_action(&settings, "v", true, true, false, false),
            configured_key_action(&settings, "x", false, true, false, false),
        ] {
            assert_eq!(decision.action, None);
            assert!(decision.forward);
        }
    }

    #[test]
    fn forwards_text_and_altgr_input() {
        let settings = AppSettings::default();
        assert_eq!(
            configured_key_action(&settings, "x", false, false, false, false).action,
            None
        );
        assert!(configured_key_action(&settings, "X", false, false, true, false).forward);
        assert!(configured_key_action(&settings, "@", true, true, false, true).forward);
    }

    #[test]
    fn configured_shortcuts_are_not_forwarded_by_default() {
        let settings = AppSettings::default();
        assert!(!configured_key_action(&settings, "c", true, false, false, false).forward);
    }

    #[test]
    fn configured_shortcuts_can_execute_and_forward() {
        let mut settings = AppSettings::default();
        settings.shortcuts[0].pass_through = true;
        let decision = configured_key_action(&settings, "c", true, false, false, false);
        assert_eq!(decision.action, Some(KeyAction::Copy));
        assert!(decision.forward);
    }
}
