//! 应用快捷键的默认值、解析、校验与更新。

use super::{AppSettings, ShortcutChord, ShortcutSetting};
use std::collections::BTreeSet;

const SHORTCUT_DEFINITIONS: [(&str, &str, &str); 8] = [
    ("copy", "复制", "Ctrl+C"),
    ("select-all", "全选", "Ctrl+A"),
    ("paste", "粘贴", "Ctrl+V"),
    ("find", "搜索终端", "Ctrl+F"),
    ("interrupt", "中断终端", "Alt+C"),
    ("new-tab", "新建标签页", "Ctrl+T"),
    ("close-tab", "关闭标签页", "Ctrl+W"),
    ("quit", "退出应用", "Alt+Q"),
];

impl ShortcutChord {
    fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.control {
            parts.push("Ctrl".to_owned());
        }
        if self.alt {
            parts.push("Alt".to_owned());
        }
        if self.shift {
            parts.push("Shift".to_owned());
        }
        parts.push(self.key.clone());
        parts.join("+")
    }
}

pub(crate) fn parse_shortcut(shortcut: &str) -> Result<ShortcutChord, String> {
    let mut chord = ShortcutChord {
        control: false,
        alt: false,
        shift: false,
        key: String::new(),
    };
    for part in shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if part.eq_ignore_ascii_case("ctrl") || part.eq_ignore_ascii_case("control") {
            chord.control = true;
        } else if part.eq_ignore_ascii_case("alt") {
            chord.alt = true;
        } else if part.eq_ignore_ascii_case("shift") {
            chord.shift = true;
        } else if chord.key.is_empty() {
            chord.key = normalize_shortcut_key(part)?;
        } else {
            return Err("错误：快捷键只能包含一个主按键".to_owned());
        }
    }
    if chord.key.is_empty() {
        return Err("错误：快捷键缺少主按键".to_owned());
    }
    if !chord.control && !chord.alt && !chord.shift {
        return Err("错误：应用快捷键至少需要 Ctrl、Alt 或 Shift 修饰键".to_owned());
    }
    Ok(chord)
}

pub(super) fn default_shortcuts() -> Vec<ShortcutSetting> {
    SHORTCUT_DEFINITIONS
        .iter()
        .map(|(action, _, shortcut)| ShortcutSetting {
            action: (*action).to_owned(),
            shortcut: (*shortcut).to_owned(),
            pass_through: false,
        })
        .collect()
}

pub(super) fn shortcut_label(action: &str) -> Option<&'static str> {
    SHORTCUT_DEFINITIONS
        .iter()
        .find(|(candidate, _, _)| *candidate == action)
        .map(|(_, label, _)| *label)
}

pub(super) fn normalize_shortcuts(shortcuts: &mut Vec<ShortcutSetting>) {
    let previous = std::mem::take(shortcuts);
    *shortcuts = SHORTCUT_DEFINITIONS
        .iter()
        .map(|(action, _, default_shortcut)| {
            previous
                .iter()
                .find(|setting| setting.action == *action)
                .cloned()
                .map(|mut setting| {
                    setting.shortcut = setting.shortcut.trim().to_owned();
                    setting.shortcut = match (setting.action.as_str(), setting.shortcut.as_str()) {
                        ("new-tab", "Ctrl+Shift+T") => "Ctrl+T".to_owned(),
                        ("close-tab", "Ctrl+Shift+W") => "Ctrl+W".to_owned(),
                        _ => setting.shortcut,
                    };
                    if parse_shortcut(&setting.shortcut).is_err() {
                        setting.shortcut = (*default_shortcut).to_owned();
                    }
                    setting
                })
                .unwrap_or_else(|| ShortcutSetting {
                    action: (*action).to_owned(),
                    shortcut: (*default_shortcut).to_owned(),
                    pass_through: false,
                })
        })
        .collect();
}

pub(super) fn validate_shortcuts(shortcuts: &[ShortcutSetting]) -> Result<(), String> {
    let mut actions = BTreeSet::new();
    let mut chords = BTreeSet::new();
    for setting in shortcuts {
        if shortcut_label(&setting.action).is_none() {
            return Err(format!("未知的快捷键功能：{}", setting.action));
        }
        if !actions.insert(setting.action.as_str()) {
            return Err(format!("快捷键功能 {} 重复", setting.action));
        }
        let chord = parse_shortcut(&setting.shortcut)?;
        if !chords.insert(chord.display()) {
            return Err(format!("快捷键 {} 重复", chord.display()));
        }
    }
    Ok(())
}

pub(crate) fn update_shortcut(
    settings: &mut AppSettings,
    action: &str,
    shortcut: &str,
    pass_through: bool,
) -> Result<(), String> {
    if shortcut_label(action).is_none() {
        return Err("错误：未知的快捷键功能".to_owned());
    }
    let parsed = parse_shortcut(shortcut)?;
    let normalized = parsed.display();
    if settings.shortcuts.iter().any(|setting| {
        setting.action != action
            && parse_shortcut(&setting.shortcut).is_ok_and(|existing| existing == parsed)
    }) {
        return Err(format!("错误：快捷键 {normalized} 已被其他功能使用"));
    }
    let Some(setting) = settings
        .shortcuts
        .iter_mut()
        .find(|setting| setting.action == action)
    else {
        return Err("错误：找不到快捷键设置".to_owned());
    };
    setting.shortcut = normalized;
    setting.pass_through = pass_through;
    Ok(())
}

fn normalize_shortcut_key(key: &str) -> Result<String, String> {
    let named = [
        "Enter",
        "Tab",
        "Escape",
        "Backspace",
        "Delete",
        "Insert",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "Up",
        "Down",
        "Left",
        "Right",
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
    ];
    if let Some(name) = named.iter().find(|name| name.eq_ignore_ascii_case(key)) {
        return Ok((*name).to_owned());
    }
    let mut characters = key.chars();
    let Some(character) = characters.next() else {
        return Err("错误：快捷键缺少主按键".to_owned());
    };
    if characters.next().is_some() || character.is_control() || character.is_whitespace() {
        return Err(format!("错误：不支持的快捷键主按键：{key}"));
    }
    Ok(character.to_uppercase().collect())
}

#[cfg(test)]
mod tests {
    use super::{parse_shortcut, update_shortcut};
    use crate::app::settings::AppSettings;

    #[test]
    fn shortcut_parser_normalizes_modifiers_and_key() {
        let shortcut = parse_shortcut("shift + control + t").unwrap();
        assert!(shortcut.control);
        assert!(shortcut.shift);
        assert!(!shortcut.alt);
        assert_eq!(shortcut.key, "T");
        assert_eq!(shortcut.display(), "Ctrl+Shift+T");
    }

    #[test]
    fn migrates_previous_tab_shortcut_defaults() {
        let mut settings = AppSettings::default();
        settings
            .shortcuts
            .iter_mut()
            .find(|setting| setting.action == "new-tab")
            .unwrap()
            .shortcut = "Ctrl+Shift+T".to_owned();
        settings
            .shortcuts
            .iter_mut()
            .find(|setting| setting.action == "close-tab")
            .unwrap()
            .shortcut = "Ctrl+Shift+W".to_owned();
        settings.normalize();
        assert_eq!(
            settings
                .shortcuts
                .iter()
                .find(|setting| setting.action == "new-tab")
                .unwrap()
                .shortcut,
            "Ctrl+T"
        );
        assert_eq!(
            settings
                .shortcuts
                .iter()
                .find(|setting| setting.action == "close-tab")
                .unwrap()
                .shortcut,
            "Ctrl+W"
        );
    }

    #[test]
    fn adds_find_shortcut_to_existing_settings() {
        let mut settings = AppSettings::default();
        settings
            .shortcuts
            .retain(|setting| setting.action != "find");

        settings.normalize();

        let find = settings
            .shortcuts
            .iter()
            .find(|setting| setting.action == "find")
            .unwrap();
        assert_eq!(find.shortcut, "Ctrl+F");
        assert!(!find.pass_through);
    }

    #[test]
    fn shortcut_update_rejects_conflicts() {
        let mut settings = AppSettings::default();
        let error = update_shortcut(&mut settings, "paste", "Ctrl+C", false).unwrap_err();
        assert!(error.contains("已被其他功能使用"));
    }

    #[test]
    fn shortcut_update_persists_forwarding_choice() {
        let mut settings = AppSettings::default();
        update_shortcut(&mut settings, "copy", "Alt+X", true).unwrap();
        let copy = settings
            .shortcuts
            .iter()
            .find(|setting| setting.action == "copy")
            .unwrap();
        assert_eq!(copy.shortcut, "Alt+X");
        assert!(copy.pass_through);
    }
}
