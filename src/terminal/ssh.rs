//! 系统 OpenSSH 的启动参数与明文密码 askpass 适配。

use crate::app::settings::ShellProfile;
use std::{collections::HashMap, env, io, io::Write, path::Path};

const ASKPASS_MODE_ENV: &str = "SLINT_TERMINAL_SSH_ASKPASS";
const PASSWORD_ENV: &str = "SLINT_TERMINAL_SSH_PASSWORD";
const OPENSSH_ASKPASS_ENV: &str = "SSH_ASKPASS";
const OPENSSH_ASKPASS_REQUIRE_ENV: &str = "SSH_ASKPASS_REQUIRE";

/// 密码存在时让当前可执行文件充当 OpenSSH askpass 助手。
pub(super) fn configure_askpass(
    environment: &mut HashMap<String, String>,
    profile: &ShellProfile,
) -> io::Result<()> {
    if profile.ssh_password.is_empty() {
        return Ok(());
    }
    let executable = env::current_exe()?;
    configure_askpass_with_executable(environment, &profile.ssh_password, &executable);
    Ok(())
}

fn configure_askpass_with_executable(
    environment: &mut HashMap<String, String>,
    password: &str,
    executable: &Path,
) {
    environment.insert(
        OPENSSH_ASKPASS_ENV.to_owned(),
        executable.to_string_lossy().into_owned(),
    );
    environment.insert(OPENSSH_ASKPASS_REQUIRE_ENV.to_owned(), "force".to_owned());
    environment.insert(ASKPASS_MODE_ENV.to_owned(), "1".to_owned());
    environment.insert(PASSWORD_ENV.to_owned(), password.to_owned());
}

/// OpenSSH 启动本程序作为 askpass 时，在创建 GUI 前把密码写到标准输出。
pub(crate) fn respond_to_askpass_if_requested() -> Option<io::Result<()>> {
    if env::var(ASKPASS_MODE_ENV).ok().as_deref() != Some("1") {
        return None;
    }
    let password = match env::var(PASSWORD_ENV) {
        Ok(password) => password,
        Err(error) => {
            return Some(Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("SSH askpass password is unavailable: {error}"),
            )));
        }
    };
    let prompt = env::args().nth(1);
    if !is_password_prompt(prompt.as_deref()) {
        return Some(Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSH askpass refused a non-password prompt",
        )));
    }
    Some(writeln!(io::stdout().lock(), "{password}"))
}

fn is_password_prompt(prompt: Option<&str>) -> bool {
    let Some(prompt) = prompt else {
        // 部分 Win32 OpenSSH 版本不会转发提示文字，但仍会读取 askpass 输出。
        return true;
    };
    let prompt = prompt.to_ascii_lowercase();
    prompt.contains("password") && !prompt.contains("passphrase")
}

#[cfg(test)]
mod tests {
    use super::{
        ASKPASS_MODE_ENV, OPENSSH_ASKPASS_ENV, OPENSSH_ASKPASS_REQUIRE_ENV, PASSWORD_ENV,
        configure_askpass_with_executable, is_password_prompt,
    };
    use std::{collections::HashMap, path::Path};

    #[test]
    fn configures_the_current_program_as_forced_askpass() {
        let mut environment = HashMap::new();
        configure_askpass_with_executable(
            &mut environment,
            "plain secret",
            Path::new("C:\\Apps\\slint-terminal.exe"),
        );
        assert_eq!(
            environment.get(OPENSSH_ASKPASS_ENV).map(String::as_str),
            Some("C:\\Apps\\slint-terminal.exe")
        );
        assert_eq!(
            environment
                .get(OPENSSH_ASKPASS_REQUIRE_ENV)
                .map(String::as_str),
            Some("force")
        );
        assert_eq!(
            environment.get(ASKPASS_MODE_ENV).map(String::as_str),
            Some("1")
        );
        assert_eq!(
            environment.get(PASSWORD_ENV).map(String::as_str),
            Some("plain secret")
        );
    }

    #[test]
    fn answers_only_password_prompts() {
        assert!(is_password_prompt(Some("deploy@example.com's password:")));
        assert!(is_password_prompt(Some("Password:")));
        assert!(is_password_prompt(None));
        assert!(!is_password_prompt(Some("Enter passphrase for key:")));
        assert!(!is_password_prompt(Some(
            "Are you sure you want to continue connecting?"
        )));
    }
}
