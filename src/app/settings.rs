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
pub(super) use ui_sync::{sync_profile_draft, sync_settings_ui};

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io, path::PathBuf};

const DEFAULT_FONT_FAMILY: &str = "Cascadia Mono";
const DEFAULT_FONT_SIZE: i32 = 15;

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

    fn validate(&self) -> Result<(), String> {
        if self.font_family.trim().is_empty() {
            return Err("字段 font_family 不能为空".to_owned());
        }
        if !(8..=36).contains(&self.font_size) {
            return Err("字段 font_size 必须在 8 到 36 之间".to_owned());
        }
        profiles::validate_profiles(self)?;
        shortcuts::validate_shortcuts(&self.shortcuts)
    }

    fn normalize(&mut self) {
        self.font_family = self.font_family.trim().to_owned();
        self.font_size = self.font_size.clamp(8, 36);
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
    use super::AppSettings;

    #[test]
    fn rejects_invalid_font_size_from_toml() {
        let settings: AppSettings = toml::from_str("font_size = 4").unwrap();
        assert!(settings.validate().unwrap_err().contains("font_size"));
    }

    #[test]
    fn rejects_unknown_toml_fields() {
        let error = toml::from_str::<AppSettings>("unknown_setting = true").unwrap_err();
        assert!(error.to_string().contains("unknown_setting"));
    }
}
