//! 用户设置的数据结构，以及各项具体设置职责的模块入口。

mod fonts;
mod profiles;
mod shortcuts;
mod storage;
mod ui_sync;

pub(super) use fonts::mono_font_families;
pub(super) use profiles::build_profile;
pub(crate) use shortcuts::parse_shortcut;
pub(super) use shortcuts::update_shortcut;
pub(super) use storage::{SettingsWatcher, load_or_create};
pub(super) use ui_sync::{
    sync_font_metrics, sync_profile_draft, sync_settings_limits, sync_settings_ui,
};

use crate::terminal::TerminalOptions;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io, path::PathBuf};

const DEFAULT_FONT_FAMILY: &str = "Cascadia Mono";
const DEFAULT_FONT_SIZE: i32 = 15;
pub(crate) const MIN_FONT_SIZE: i32 = 8;
pub(crate) const MAX_FONT_SIZE: i32 = 36;
const DEFAULT_SCROLLBACK_LINES: u32 = 10_000;
/// 与 Alacritty 自身的配置上限一致；每行都常驻内存，再大会让长会话占用失控。
pub(crate) const MAX_SCROLLBACK_LINES: u32 = 100_000;
const DEFAULT_SCROLL_LINES: u32 = 3;
pub(crate) const MAX_SCROLL_LINES: u32 = 20;

/// 未被终端程序覆盖时使用的光标形状。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CursorShapeSetting {
    #[default]
    Block,
    Underline,
    Beam,
}

impl CursorShapeSetting {
    /// 与设置页下拉框的选项顺序一一对应。
    pub(crate) fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Underline,
            2 => Self::Beam,
            _ => Self::Block,
        }
    }

    pub(crate) fn index(self) -> i32 {
        match self {
            Self::Block => 0,
            Self::Underline => 1,
            Self::Beam => 2,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ShortcutSetting {
    pub(crate) action: String,
    pub(crate) shortcut: String,
    pub(crate) pass_through: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProfileKind {
    #[default]
    Local,
    Ssh,
}

impl ProfileKind {
    fn is_local(kind: &Self) -> bool {
        *kind == Self::Local
    }
}

fn default_ssh_port() -> u16 {
    22
}

fn is_default_ssh_port(port: &u16) -> bool {
    *port == default_ssh_port()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ShellProfile {
    pub(crate) id: i32,
    pub(crate) name: String,
    #[serde(skip_serializing_if = "ProfileKind::is_local")]
    pub(crate) kind: ProfileKind,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) program: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) arguments: Vec<String>,
    pub(crate) working_directory: Option<PathBuf>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub(crate) environment: HashMap<String, String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) ssh_host: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) ssh_user: String,
    #[serde(
        default = "default_ssh_port",
        skip_serializing_if = "is_default_ssh_port"
    )]
    pub(crate) ssh_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ssh_identity_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) ssh_password: String,
}

impl Default for ShellProfile {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            kind: ProfileKind::Local,
            program: String::new(),
            arguments: Vec::new(),
            working_directory: None,
            environment: HashMap::new(),
            ssh_host: String::new(),
            ssh_user: String::new(),
            ssh_port: default_ssh_port(),
            ssh_identity_file: None,
            ssh_password: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct AppSettings {
    pub(super) profiles: Vec<ShellProfile>,
    pub(super) default_profile_id: Option<i32>,
    pub(super) next_profile_id: i32,
    pub(super) font_family: String,
    pub(super) font_size: i32,
    /// 每个会话保留的滚动历史行数；0 表示不保留历史。
    pub(super) scrollback_lines: u32,
    /// 鼠标滚轮每格滚动的行数。
    pub(super) scroll_lines: u32,
    pub(super) copy_on_select: bool,
    pub(super) right_click_paste: bool,
    pub(super) cursor_shape: CursorShapeSetting,
    pub(super) cursor_blink: bool,
    pub(super) shortcuts: Vec<ShortcutSetting>,
    // 兼容上一版只保存路径的配置，加载后自动迁移为 Profile。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) shells: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) default_shell: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            default_profile_id: None,
            next_profile_id: 1,
            font_family: DEFAULT_FONT_FAMILY.to_owned(),
            font_size: DEFAULT_FONT_SIZE,
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            scroll_lines: DEFAULT_SCROLL_LINES,
            copy_on_select: false,
            right_click_paste: false,
            cursor_shape: CursorShapeSetting::Block,
            cursor_blink: false,
            shortcuts: shortcuts::default_shortcuts(),
            shells: Vec::new(),
            default_shell: None,
        }
    }
}

impl AppSettings {
    pub(super) fn built_in_defaults() -> Self {
        let mut settings = Self::default();
        settings.normalize();
        settings
    }

    pub(super) fn save(&self) -> io::Result<()> {
        storage::save(self)
    }

    pub(super) fn default_profile(&self) -> Option<&ShellProfile> {
        let id = self.default_profile_id?;
        self.profile(id)
    }

    pub(super) fn profile(&self, id: i32) -> Option<&ShellProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub(super) fn profile_mut(&mut self, id: i32) -> Option<&mut ShellProfile> {
        self.profiles.iter_mut().find(|profile| profile.id == id)
    }

    pub(super) fn allocate_profile_id(&mut self) -> i32 {
        let id = self.next_profile_id.max(1);
        self.next_profile_id = id.saturating_add(1);
        id
    }

    /// 交给终端后端的行为选项；字体等只影响渲染的设置不在其中。
    pub(super) fn terminal_options(&self) -> TerminalOptions {
        TerminalOptions {
            scrollback_lines: self.scrollback_lines,
            scroll_lines: self.scroll_lines,
            copy_on_select: self.copy_on_select,
            right_click_paste: self.right_click_paste,
            cursor_shape: self.cursor_shape,
            cursor_blink: self.cursor_blink,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.font_family.trim().is_empty() {
            return Err("字段 font_family 不能为空".to_owned());
        }
        if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&self.font_size) {
            return Err(format!(
                "字段 font_size 必须在 {MIN_FONT_SIZE} 到 {MAX_FONT_SIZE} 之间"
            ));
        }
        if self.scrollback_lines > MAX_SCROLLBACK_LINES {
            return Err(format!(
                "字段 scrollback_lines 不能超过 {MAX_SCROLLBACK_LINES}"
            ));
        }
        if !(1..=MAX_SCROLL_LINES).contains(&self.scroll_lines) {
            return Err(format!(
                "字段 scroll_lines 必须在 1 到 {MAX_SCROLL_LINES} 之间"
            ));
        }
        profiles::validate_profiles(self)?;
        shortcuts::validate_shortcuts(&self.shortcuts)
    }

    fn normalize(&mut self) {
        self.font_family = self.font_family.trim().to_owned();
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.scrollback_lines = self.scrollback_lines.min(MAX_SCROLLBACK_LINES);
        self.scroll_lines = self.scroll_lines.clamp(1, MAX_SCROLL_LINES);
        shortcuts::normalize_shortcuts(&mut self.shortcuts);
        profiles::normalize_profiles(self);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShortcutChord {
    pub(crate) control: bool,
    pub(crate) alt: bool,
    pub(crate) shift: bool,
    pub(crate) key: String,
}

#[cfg(test)]
mod tests {
    use super::{AppSettings, CursorShapeSetting};

    #[test]
    fn rejects_invalid_font_size_from_toml() {
        let settings: AppSettings = toml::from_str("font_size = 4").unwrap();
        assert!(settings.validate().unwrap_err().contains("font_size"));
    }

    #[test]
    fn scrollback_lines_defaults_and_rejects_out_of_range_values() {
        let settings: AppSettings = toml::from_str("").unwrap();
        assert_eq!(settings.scrollback_lines, 10_000);
        let settings: AppSettings = toml::from_str("scrollback_lines = 0").unwrap();
        assert!(settings.validate().is_ok());
        let settings: AppSettings = toml::from_str("scrollback_lines = 100001").unwrap();
        assert!(settings.validate().unwrap_err().contains("scrollback_lines"));
        assert!(toml::from_str::<AppSettings>("scrollback_lines = -1").is_err());
    }

    #[test]
    fn terminal_behavior_settings_default_and_validate() {
        let settings: AppSettings = toml::from_str("").unwrap();
        assert_eq!(settings.scroll_lines, 3);
        assert!(!settings.copy_on_select);
        assert!(!settings.right_click_paste);
        assert_eq!(settings.cursor_shape, CursorShapeSetting::Block);
        assert!(!settings.cursor_blink);

        let settings: AppSettings = toml::from_str(
            "scroll_lines = 5\ncopy_on_select = true\ncursor_shape = \"beam\"\ncursor_blink = true",
        )
        .unwrap();
        assert!(settings.validate().is_ok());
        let options = settings.terminal_options();
        assert_eq!(options.scroll_lines, 5);
        assert!(options.copy_on_select);
        assert_eq!(options.cursor_shape, CursorShapeSetting::Beam);
        assert!(options.cursor_blink);

        let settings: AppSettings = toml::from_str("scroll_lines = 0").unwrap();
        assert!(settings.validate().unwrap_err().contains("scroll_lines"));
        assert!(toml::from_str::<AppSettings>("cursor_shape = \"circle\"").is_err());
    }

    #[test]
    fn cursor_shape_round_trips_through_combo_index() {
        for shape in [
            CursorShapeSetting::Block,
            CursorShapeSetting::Underline,
            CursorShapeSetting::Beam,
        ] {
            assert_eq!(CursorShapeSetting::from_index(shape.index()), shape);
        }
        assert_eq!(CursorShapeSetting::from_index(-1), CursorShapeSetting::Block);
    }

    #[test]
    fn rejects_unknown_toml_fields() {
        let error = toml::from_str::<AppSettings>("unknown_setting = true").unwrap_err();
        assert!(error.to_string().contains("unknown_setting"));
    }
}
