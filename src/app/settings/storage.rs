//! TOML 设置文件的定位、首次生成、完整覆盖写入和变更检测。

use super::AppSettings;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

pub(crate) struct InitialSettings {
    pub(crate) settings: AppSettings,
    pub(crate) error: Option<String>,
    pub(crate) writable: bool,
}

#[derive(PartialEq, Eq)]
enum FileSnapshot {
    Contents(String),
    Error(String),
}

pub(crate) struct SettingsWatcher {
    path: Option<PathBuf>,
    snapshot: Option<FileSnapshot>,
}

impl SettingsWatcher {
    pub(crate) fn new() -> Self {
        let path = settings_path();
        let snapshot = path.as_deref().map(read_snapshot);
        Self { path, snapshot }
    }

    pub(crate) fn poll(&mut self) -> Option<Result<AppSettings, String>> {
        let path = self.path.as_deref()?;
        let snapshot = read_snapshot(path);
        if self.snapshot.as_ref() == Some(&snapshot) {
            return None;
        }
        let result = match &snapshot {
            FileSnapshot::Contents(contents) => parse(contents),
            FileSnapshot::Error(error) => Err(error.clone()),
        };
        self.snapshot = Some(snapshot);
        Some(result)
    }
}

pub(crate) fn load_or_create() -> InitialSettings {
    let Some(path) = settings_path() else {
        return InitialSettings {
            settings: AppSettings::built_in_defaults(),
            error: Some("无法确定用户配置目录，当前使用内置默认设置。".to_owned()),
            writable: false,
        };
    };
    load_or_create_at(&path)
}

pub(super) fn save(settings: &AppSettings) -> io::Result<()> {
    let path = settings_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定用户配置目录"))?;
    save_at(&path, settings)
}

fn load_or_create_at(path: &Path) -> InitialSettings {
    match fs::read_to_string(path) {
        Ok(contents) => match parse(&contents) {
            Ok(settings) => InitialSettings {
                settings,
                error: None,
                writable: true,
            },
            Err(error) => InitialSettings {
                settings: AppSettings::built_in_defaults(),
                error: Some(format!(
                    "设置文件错误：{error}。当前使用内置默认设置，请修正 settings.toml。"
                )),
                writable: false,
            },
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let settings = AppSettings::built_in_defaults();
            match save_at(path, &settings) {
                Ok(()) => InitialSettings {
                    settings,
                    error: None,
                    writable: true,
                },
                Err(error) => InitialSettings {
                    settings,
                    error: Some(format!("无法生成设置文件：{error}。当前使用内置默认设置。")),
                    writable: false,
                },
            }
        }
        Err(error) => InitialSettings {
            settings: AppSettings::built_in_defaults(),
            error: Some(format!("无法读取设置文件：{error}。当前使用内置默认设置。")),
            writable: false,
        },
    }
}

fn parse(contents: &str) -> Result<AppSettings, String> {
    let mut settings: AppSettings = toml::from_str(contents).map_err(|error| error.to_string())?;
    settings.validate()?;
    settings.normalize();
    Ok(settings)
}

fn save_at(path: &Path, settings: &AppSettings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(settings).map_err(io::Error::other)?;
    fs::write(path, contents)
}

fn read_snapshot(path: &Path) -> FileSnapshot {
    match fs::read_to_string(path) {
        Ok(contents) => FileSnapshot::Contents(contents),
        Err(error) => FileSnapshot::Error(format!("无法读取 settings.toml：{error}")),
    }
}

fn settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|directory| directory.join("slint-terminal").join("settings.toml"))
}

#[cfg(test)]
mod tests {
    use super::{load_or_create_at, parse};
    use crate::app::settings::{AppSettings, ShellProfile};
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn first_load_generates_a_toml_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "slint-terminal-settings-{}-{unique}",
            std::process::id()
        ));
        let path = directory.join("settings.toml");
        let loaded = load_or_create_at(&path);
        assert!(loaded.error.is_none());
        assert!(loaded.writable);
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("font_family"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_toml_is_rejected() {
        assert!(parse("font_size = nope").is_err());
    }

    #[test]
    fn invalid_external_profile_is_rejected() {
        let mut settings = AppSettings::default();
        settings.profiles.push(ShellProfile {
            id: 1,
            name: "Broken".to_owned(),
            program: "relative-shell".to_owned(),
            ..ShellProfile::default()
        });
        let contents = toml::to_string(&settings).unwrap();
        assert!(parse(&contents).unwrap_err().contains("完整路径"));
    }
}
