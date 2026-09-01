//! Shell Profile 的校验、规范化和系统探测。

use super::{AppSettings, ShellProfile};
use std::{
    collections::{BTreeSet, HashMap},
    env,
    path::{Path, PathBuf},
};

pub(crate) fn build_profile(
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

pub(super) fn validate_profiles(settings: &AppSettings) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for profile in &settings.profiles {
        if profile.id < 1 {
            return Err("Profile 的 id 必须大于 0".to_owned());
        }
        if !ids.insert(profile.id) {
            return Err(format!("Profile id {} 重复", profile.id));
        }
        if profile.name.trim().is_empty() {
            return Err(format!("Profile {} 的名称不能为空", profile.id));
        }
        validate_program(&profile.program)
            .map_err(|error| format!("Profile {}：{}", profile.id, error_without_prefix(&error)))?;
        if let Some(directory) = &profile.working_directory {
            validate_working_directory(&directory.to_string_lossy()).map_err(|error| {
                format!("Profile {}：{}", profile.id, error_without_prefix(&error))
            })?;
        }
        for key in profile.environment.keys() {
            validate_environment_key(key).map_err(|message| {
                format!("Profile {}：环境变量名称无效：{message}", profile.id)
            })?;
        }
    }
    if let Some(id) = settings.default_profile_id
        && !ids.contains(&id)
    {
        return Err(format!("default_profile_id {id} 不存在"));
    }
    Ok(())
}

pub(super) fn normalize_profiles(settings: &mut AppSettings) {
    if settings.profiles.is_empty() && !settings.shells.is_empty() {
        let legacy_default = settings.default_shell.clone();
        let legacy_shells = std::mem::take(&mut settings.shells);
        for path in legacy_shells {
            let id = settings.allocate_profile_id();
            let name = profile_name_from_program(&path);
            if legacy_default
                .as_ref()
                .is_some_and(|default| default.eq_ignore_ascii_case(&path))
            {
                settings.default_profile_id = Some(id);
            }
            settings.profiles.push(ShellProfile {
                id,
                name,
                program: path,
                ..ShellProfile::default()
            });
        }
    }
    if settings.profiles.is_empty() {
        for (name, program, arguments) in detected_profiles() {
            let id = settings.allocate_profile_id();
            settings.profiles.push(ShellProfile {
                id,
                name,
                program,
                arguments,
                ..ShellProfile::default()
            });
        }
        settings.default_profile_id = settings.profiles.first().map(|profile| profile.id);
    }
    settings.shells.clear();
    settings.default_shell = None;
    let mut next_id = 1;
    settings
        .profiles
        .retain(|profile| !profile.name.trim().is_empty() && !profile.program.trim().is_empty());
    for profile in &mut settings.profiles {
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
    settings.next_profile_id = settings.next_profile_id.max(next_id);
    if settings
        .default_profile_id
        .is_some_and(|id| settings.profile(id).is_none())
    {
        settings.default_profile_id = settings.profiles.first().map(|profile| profile.id);
    }
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
        if validate_environment_key(key).is_err() {
            return Err(format!("错误：环境变量第 {} 行名称无效", line_index + 1));
        }
        environment.insert(key.to_owned(), value.trim().to_owned());
    }
    Ok(environment)
}

fn validate_environment_key(key: &str) -> Result<(), &str> {
    if key.is_empty() {
        return Err("名称不能为空");
    }
    if key.contains('\0') || key.contains('=') {
        return Err(key);
    }
    Ok(())
}

fn error_without_prefix(error: &str) -> &str {
    error.strip_prefix("错误：").unwrap_or(error)
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

#[cfg(test)]
mod tests {
    use super::{parse_environment, validate_profiles};
    use crate::app::settings::{AppSettings, ShellProfile};
    use std::path::PathBuf;

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
    fn external_profile_rejects_a_relative_program() {
        let mut settings = AppSettings::default();
        settings.profiles.push(ShellProfile {
            id: 1,
            name: "Broken".to_owned(),
            program: "relative-shell".to_owned(),
            ..ShellProfile::default()
        });
        assert!(
            validate_profiles(&settings)
                .unwrap_err()
                .contains("完整路径")
        );
    }

    #[test]
    fn external_profile_rejects_a_relative_working_directory() {
        let mut settings = AppSettings::default();
        settings.profiles.push(ShellProfile {
            id: 1,
            name: "Broken".to_owned(),
            program: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            working_directory: Some(PathBuf::from("relative-directory")),
            ..ShellProfile::default()
        });
        assert!(
            validate_profiles(&settings)
                .unwrap_err()
                .contains("工作目录必须是完整路径")
        );
    }
}
