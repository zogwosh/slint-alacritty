//! Shell Profile 的校验、规范化和系统探测。

use super::{AppSettings, ProfileKind, ShellProfile};
use std::{
    collections::{BTreeSet, HashMap},
    env,
    path::{Path, PathBuf},
};

// 参数与 Slint 的扁平 Profile 编辑回调一一对应，避免为表单传输引入额外领域模型。
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_profile(
    id: i32,
    name: &str,
    kind_index: i32,
    program: &str,
    arguments: &str,
    working_directory: &str,
    environment: &str,
    ssh_host: &str,
    ssh_user: &str,
    ssh_port: i32,
    ssh_identity_file: &str,
    ssh_password: &str,
) -> Result<ShellProfile, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("错误：Profile 名称不能为空".to_owned());
    }
    let kind = match kind_index {
        0 => ProfileKind::Local,
        1 => ProfileKind::Ssh,
        _ => return Err("错误：Profile 类型无效".to_owned()),
    };
    let arguments = parse_arguments(arguments);
    match kind {
        ProfileKind::Local => Ok(ShellProfile {
            id,
            name: name.to_owned(),
            kind,
            program: validate_program(program)?,
            arguments,
            working_directory: validate_working_directory(working_directory)?,
            environment: parse_environment(environment)?,
            ..ShellProfile::default()
        }),
        ProfileKind::Ssh => Ok(ShellProfile {
            id,
            name: name.to_owned(),
            kind,
            arguments,
            ssh_host: validate_ssh_host(ssh_host)?,
            ssh_user: validate_ssh_user(ssh_user)?,
            ssh_port: validate_ssh_port(ssh_port)?,
            ssh_identity_file: validate_identity_file(ssh_identity_file)?,
            ssh_password: validate_ssh_password(ssh_password)?,
            ..ShellProfile::default()
        }),
    }
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
        match profile.kind {
            ProfileKind::Local => {
                validate_program(&profile.program).map_err(|error| {
                    format!("Profile {}：{}", profile.id, error_without_prefix(&error))
                })?;
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
            ProfileKind::Ssh => {
                validate_ssh_host(&profile.ssh_host).map_err(|error| {
                    format!("Profile {}：{}", profile.id, error_without_prefix(&error))
                })?;
                validate_ssh_user(&profile.ssh_user).map_err(|error| {
                    format!("Profile {}：{}", profile.id, error_without_prefix(&error))
                })?;
                validate_ssh_port(i32::from(profile.ssh_port)).map_err(|error| {
                    format!("Profile {}：{}", profile.id, error_without_prefix(&error))
                })?;
                if let Some(identity_file) = &profile.ssh_identity_file {
                    validate_identity_file(&identity_file.to_string_lossy()).map_err(|error| {
                        format!("Profile {}：{}", profile.id, error_without_prefix(&error))
                    })?;
                }
                validate_ssh_password(&profile.ssh_password).map_err(|error| {
                    format!("Profile {}：{}", profile.id, error_without_prefix(&error))
                })?;
            }
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
    settings.profiles.retain(|profile| {
        !profile.name.trim().is_empty()
            && match profile.kind {
                ProfileKind::Local => !profile.program.trim().is_empty(),
                ProfileKind::Ssh => !profile.ssh_host.trim().is_empty(),
            }
    });
    for profile in &mut settings.profiles {
        profile.name = profile.name.trim().to_owned();
        profile.program = profile.program.trim().trim_matches('"').to_owned();
        profile.ssh_host = profile.ssh_host.trim().to_owned();
        profile.ssh_user = profile.ssh_user.trim().to_owned();
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

fn parse_arguments(arguments: &str) -> Vec<String> {
    arguments
        .lines()
        .map(str::trim)
        .filter(|argument| !argument.is_empty())
        .map(ToOwned::to_owned)
        .collect()
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

fn validate_ssh_host(host: &str) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("错误：SSH 主机不能为空".to_owned());
    }
    if host.chars().any(char::is_whitespace) || host.chars().any(char::is_control) {
        return Err("错误：SSH 主机不能包含空白或控制字符".to_owned());
    }
    if host.starts_with('-') {
        return Err("错误：SSH 主机不能以 - 开头".to_owned());
    }
    Ok(host.to_owned())
}

fn validate_ssh_user(user: &str) -> Result<String, String> {
    let user = user.trim();
    if user.chars().any(char::is_whitespace) || user.chars().any(char::is_control) {
        return Err("错误：SSH 用户名不能包含空白或控制字符".to_owned());
    }
    if user.contains('@') {
        return Err("错误：SSH 用户名不能包含 @".to_owned());
    }
    if user.starts_with('-') {
        return Err("错误：SSH 用户名不能以 - 开头".to_owned());
    }
    Ok(user.to_owned())
}

fn validate_ssh_port(port: i32) -> Result<u16, String> {
    u16::try_from(port)
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| "错误：SSH 端口必须在 1 到 65535 之间".to_owned())
}

fn validate_identity_file(path: &str) -> Result<Option<PathBuf>, String> {
    let path = path.trim().trim_matches('"');
    if path.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("错误：SSH 私钥文件必须是完整路径".to_owned());
    }
    if !path.is_file() {
        return Err(format!("错误：找不到 SSH 私钥文件：{}", path.display()));
    }
    Ok(Some(path))
}

fn validate_ssh_password(password: &str) -> Result<String, String> {
    if password.contains(['\0', '\r', '\n']) {
        return Err("错误：SSH 密码不能包含空字符或换行符".to_owned());
    }
    Ok(password.to_owned())
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
    use super::{build_profile, parse_environment, validate_profiles};
    use crate::app::settings::{AppSettings, ProfileKind, ShellProfile};
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

    #[test]
    fn builds_an_ssh_profile_without_local_shell_fields() {
        let profile = build_profile(
            -1,
            "Production",
            1,
            "",
            "-A\n-J\njump-host",
            "",
            "",
            "example.com",
            "deploy",
            2222,
            "",
            "secret",
        )
        .unwrap();
        assert_eq!(profile.kind, ProfileKind::Ssh);
        assert_eq!(profile.ssh_host, "example.com");
        assert_eq!(profile.ssh_user, "deploy");
        assert_eq!(profile.ssh_port, 2222);
        assert_eq!(profile.arguments, ["-A", "-J", "jump-host"]);
        assert_eq!(profile.ssh_password, "secret");
        assert!(profile.program.is_empty());
        assert!(profile.environment.is_empty());
    }

    #[test]
    fn ssh_profile_rejects_an_option_shaped_host() {
        let error = build_profile(-1, "Bad", 1, "", "", "", "", "-V", "", 22, "", "").unwrap_err();
        assert!(error.contains("不能以 - 开头"));
    }

    #[test]
    fn ssh_profile_rejects_a_multiline_password() {
        let error = build_profile(
            -1,
            "Bad password",
            1,
            "",
            "",
            "",
            "",
            "example.com",
            "deploy",
            22,
            "",
            "first\nsecond",
        )
        .unwrap_err();
        assert!(error.contains("换行符"));
    }
}
