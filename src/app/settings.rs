//! 用户设置、Shell Profile、系统等宽字体枚举，以及到 Slint 的同步。

use crate::{MainWindow, ShellProfileData, ShortcutData};
use fontdb::Database;
use serde::{Deserialize, Serialize};
use slint::{ModelRc, SharedString, VecModel};
use std::{
    collections::{BTreeSet, HashMap},
    env, fs, io,
    path::{Path, PathBuf},
};

const DEFAULT_FONT_FAMILY: &str = "Cascadia Mono";
const DEFAULT_FONT_SIZE: i32 = 15;

const SHORTCUT_DEFINITIONS: [(&str, &str, &str); 7] = [
    ("copy", "复制", "Ctrl+C"),
    ("select-all", "全选", "Ctrl+A"),
    ("paste", "粘贴", "Ctrl+V"),
    ("interrupt", "中断终端", "Alt+C"),
    ("new-tab", "新建标签页", "Ctrl+T"),
    ("close-tab", "关闭标签页", "Ctrl+W"),
    ("quit", "退出应用", "Alt+Q"),
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ShortcutSetting {
    pub(crate) action: String,
    pub(crate) shortcut: String,
    pub(crate) pass_through: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ShellProfile {
    pub(crate) id: i32,
    pub(crate) name: String,
    pub(crate) program: String,
    pub(crate) arguments: Vec<String>,
    pub(crate) working_directory: Option<PathBuf>,
    pub(crate) environment: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct AppSettings {
    pub(super) profiles: Vec<ShellProfile>,
    pub(super) default_profile_id: Option<i32>,
    pub(super) next_profile_id: i32,
    pub(super) font_family: String,
    pub(super) font_size: i32,
    pub(super) shortcuts: Vec<ShortcutSetting>,
    // 兼容上一版只保存路径的配置，加载后自动迁移为 Profile。
    pub(super) shells: Vec<String>,
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
            shortcuts: default_shortcuts(),
            shells: Vec::new(),
            default_shell: None,
        }
    }
}

impl AppSettings {
    pub(super) fn load() -> Self {
        let Some(path) = settings_path() else {
            let mut settings = Self::default();
            settings.normalize();
            return settings;
        };
        let Ok(contents) = fs::read_to_string(path) else {
            let mut settings = Self::default();
            settings.normalize();
            return settings;
        };
        let mut settings: Self = serde_json::from_str(&contents).unwrap_or_default();
        settings.normalize();
        settings
    }

    pub(super) fn save(&self) -> io::Result<()> {
        let path = settings_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定用户配置目录"))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(path, contents)
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

    fn normalize(&mut self) {
        self.font_family = self.font_family.trim().to_owned();
        if self.font_family.is_empty() {
            self.font_family = DEFAULT_FONT_FAMILY.to_owned();
        }
        self.font_size = self.font_size.clamp(8, 36);
        let previous_shortcuts = std::mem::take(&mut self.shortcuts);
        self.shortcuts = SHORTCUT_DEFINITIONS
            .iter()
            .map(|(action, _, default_shortcut)| {
                previous_shortcuts
                    .iter()
                    .find(|setting| setting.action == *action)
                    .cloned()
                    .map(|mut setting| {
                        setting.shortcut = setting.shortcut.trim().to_owned();
                        setting.shortcut =
                            match (setting.action.as_str(), setting.shortcut.as_str()) {
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
        if self.profiles.is_empty() && !self.shells.is_empty() {
            let legacy_default = self.default_shell.clone();
            let legacy_shells = std::mem::take(&mut self.shells);
            for path in legacy_shells {
                let id = self.allocate_profile_id();
                let name = profile_name_from_program(&path);
                if legacy_default
                    .as_ref()
                    .is_some_and(|default| default.eq_ignore_ascii_case(&path))
                {
                    self.default_profile_id = Some(id);
                }
                self.profiles.push(ShellProfile {
                    id,
                    name,
                    program: path,
                    ..ShellProfile::default()
                });
            }
        }
        if self.profiles.is_empty() {
            for (name, program, arguments) in detected_profiles() {
                let id = self.allocate_profile_id();
                self.profiles.push(ShellProfile {
                    id,
                    name,
                    program,
                    arguments,
                    ..ShellProfile::default()
                });
            }
            self.default_profile_id = self.profiles.first().map(|profile| profile.id);
        }
        self.shells.clear();
        self.default_shell = None;
        let mut next_id = 1;
        self.profiles.retain(|profile| {
            !profile.name.trim().is_empty() && !profile.program.trim().is_empty()
        });
        for profile in &mut self.profiles {
            profile.name = profile.name.trim().to_owned();
            profile.program = profile.program.trim().trim_matches('"').to_owned();
            profile
                .arguments
                .retain(|argument| !argument.trim().is_empty());
            profile.working_directory = profile
                .working_directory
                .take()
                .filter(|directory| !directory.as_os_str().is_empty());
            next_id = next_id.max(profile.id.saturating_add(1));
        }
        self.next_profile_id = self.next_profile_id.max(next_id);
        if self
            .default_profile_id
            .is_some_and(|id| self.profile(id).is_none())
        {
            self.default_profile_id = self.profiles.first().map(|profile| profile.id);
        }
    }
}

pub(super) fn build_profile(
    id: i32,
    name: &str,
    program: &str,
    arguments: &str,
    working_directory: &str,
    environment: &str,
) -> Result<ShellProfile, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("错误：Profile 名称不能为空".to_owned());
    }
    let program = validate_program(program)?;
    let working_directory = validate_working_directory(working_directory)?;
    let environment = parse_environment(environment)?;
    Ok(ShellProfile {
        id,
        name: name.to_owned(),
        program,
        arguments: arguments
            .lines()
            .map(str::trim)
            .filter(|argument| !argument.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        working_directory,
        environment,
    })
}

pub(super) fn mono_font_families() -> Vec<String> {
    let mut database = Database::new();
    database.load_system_fonts();
    let mut families = database
        .faces()
        .filter(|face| face.monospaced)
        .filter_map(|face| face.families.first().map(|(family, _)| family.clone()))
        .collect::<BTreeSet<_>>();
    families.insert(DEFAULT_FONT_FAMILY.to_owned());
    families.into_iter().collect()
}

pub(super) fn sync_settings_ui(ui: &MainWindow, settings: &AppSettings, mono_fonts: &[String]) {
    let model = settings
        .profiles
        .iter()
        .map(|profile| ShellProfileData {
            id: profile.id,
            name: profile.name.clone().into(),
            program: profile.program.clone().into(),
            is_default: settings.default_profile_id == Some(profile.id),
        })
        .collect::<Vec<_>>();
    ui.set_shell_profiles(ModelRc::new(VecModel::from(model)));
    let shortcuts = settings
        .shortcuts
        .iter()
        .filter_map(|setting| {
            let label = shortcut_label(&setting.action)?;
            Some(ShortcutData {
                action: setting.action.clone().into(),
                label: label.into(),
                shortcut: setting.shortcut.clone().into(),
                pass_through: setting.pass_through,
            })
        })
        .collect::<Vec<_>>();
    ui.set_shortcuts(ModelRc::new(VecModel::from(shortcuts)));
    ui.set_mono_fonts(ModelRc::new(VecModel::from(
        mono_fonts
            .iter()
            .map(|family| SharedString::from(family.as_str()))
            .collect::<Vec<_>>(),
    )));
    ui.set_terminal_font_family(settings.font_family.clone().into());
    ui.set_terminal_font_size(settings.font_size);
    ui.set_settings_font_size(settings.font_size);
    let font_index = mono_fonts
        .iter()
        .position(|family| family.eq_ignore_ascii_case(&settings.font_family))
        .unwrap_or(0);
    ui.set_settings_font_index(font_index.min(i32::MAX as usize) as i32);
}

pub(super) fn update_shortcut(
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShortcutChord {
    pub(crate) control: bool,
    pub(crate) alt: bool,
    pub(crate) shift: bool,
    pub(crate) key: String,
}

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

fn default_shortcuts() -> Vec<ShortcutSetting> {
    SHORTCUT_DEFINITIONS
        .iter()
        .map(|(action, _, shortcut)| ShortcutSetting {
            action: (*action).to_owned(),
            shortcut: (*shortcut).to_owned(),
            pass_through: false,
        })
        .collect()
}

fn shortcut_label(action: &str) -> Option<&'static str> {
    SHORTCUT_DEFINITIONS
        .iter()
        .find(|(candidate, _, _)| *candidate == action)
        .map(|(_, label, _)| *label)
}

pub(super) fn sync_profile_draft(ui: &MainWindow, profile: Option<&ShellProfile>) {
    let Some(profile) = profile else {
        ui.set_selected_profile_id(-1);
        ui.set_profile_name_draft("".into());
        ui.set_profile_program_draft("".into());
        ui.set_profile_arguments_draft("".into());
        ui.set_profile_working_directory_draft("".into());
        ui.set_profile_environment_draft("".into());
        return;
    };
    ui.set_selected_profile_id(profile.id);
    ui.set_profile_name_draft(profile.name.clone().into());
    ui.set_profile_program_draft(profile.program.clone().into());
    ui.set_profile_arguments_draft(profile.arguments.join("\n").into());
    ui.set_profile_working_directory_draft(
        profile
            .working_directory
            .as_ref()
            .map(|directory| directory.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    let mut environment = profile.environment.iter().collect::<Vec<_>>();
    environment.sort_by_key(|(key, _)| *key);
    ui.set_profile_environment_draft(
        environment
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into(),
    );
}

fn validate_program(program: &str) -> Result<String, String> {
    let program = program.trim().trim_matches('"');
    if program.is_empty() {
        return Err("错误：Shell 程序路径不能为空".to_owned());
    }
    let candidate = Path::new(program);
    if !candidate.is_absolute() {
        return Err("错误：请输入 Shell 可执行文件的完整路径".to_owned());
    }
    if !candidate.is_file() {
        return Err(format!("错误：找不到 Shell 可执行文件：{program}"));
    }
    Ok(candidate.to_string_lossy().into_owned())
}

fn validate_working_directory(directory: &str) -> Result<Option<PathBuf>, String> {
    let directory = directory.trim().trim_matches('"');
    if directory.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(directory);
    if !path.is_absolute() {
        return Err("错误：工作目录必须是完整路径".to_owned());
    }
    if !path.is_dir() {
        return Err(format!("错误：工作目录不存在：{directory}"));
    }
    Ok(Some(path))
}

fn parse_environment(input: &str) -> Result<HashMap<String, String>, String> {
    let mut environment = HashMap::new();
    for (line_index, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("错误：环境变量第 {} 行缺少 =", line_index + 1));
        };
        let key = key.trim();
        if key.is_empty() || key.contains('\0') || key.contains('=') {
            return Err(format!("错误：环境变量第 {} 行名称无效", line_index + 1));
        }
        environment.insert(key.to_owned(), value.trim().to_owned());
    }
    Ok(environment)
}

fn profile_name_from_program(program: &str) -> String {
    Path::new(program)
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Shell")
        .to_owned()
}

fn detected_profiles() -> Vec<(String, String, Vec<String>)> {
    let mut profiles = Vec::new();
    let mut programs = BTreeSet::new();
    let mut add = |name: &str, program: PathBuf, arguments: Vec<String>| {
        if !program.is_file() {
            return;
        }
        let program = program.to_string_lossy().into_owned();
        if programs.insert(program.to_lowercase()) {
            profiles.push((name.to_owned(), program, arguments));
        }
    };

    #[cfg(target_os = "windows")]
    {
        if let Some(program_files) = env::var_os("ProgramFiles").map(PathBuf::from) {
            add(
                "PowerShell 7",
                program_files.join("PowerShell/7/pwsh.exe"),
                Vec::new(),
            );
            add(
                "Git Bash",
                program_files.join("Git/bin/bash.exe"),
                vec!["--login".to_owned(), "-i".to_owned()],
            );
        }
        if let Some(windows) = env::var_os("WINDIR").map(PathBuf::from) {
            add(
                "Windows PowerShell",
                windows.join("System32/WindowsPowerShell/v1.0/powershell.exe"),
                Vec::new(),
            );
            add("WSL", windows.join("System32/wsl.exe"), Vec::new());
        }
        if let Some(command_prompt) = env::var_os("COMSPEC").map(PathBuf::from) {
            add("CMD", command_prompt, Vec::new());
        }
    }

    #[cfg(not(target_os = "windows"))]
    if let Some(shell) = env::var_os("SHELL").map(PathBuf::from) {
        let name = profile_name_from_program(&shell.to_string_lossy());
        add(&name, shell, Vec::new());
    }

    profiles
}

fn settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|directory| directory.join("slint-terminal").join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::{AppSettings, parse_environment, parse_shortcut, update_shortcut};

    #[test]
    fn environment_parser_accepts_values_containing_equals() {
        let environment = parse_environment("A=one\nTOKEN=a=b=c\n\n").unwrap();
        assert_eq!(environment.get("A").map(String::as_str), Some("one"));
        assert_eq!(environment.get("TOKEN").map(String::as_str), Some("a=b=c"));
    }

    #[test]
    fn environment_parser_reports_the_invalid_line() {
        let error = parse_environment("A=one\nINVALID").unwrap_err();
        assert!(error.contains("第 2 行"));
    }

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
