//! Rust 设置状态到 Slint 属性和模型的同步。

use super::{AppSettings, ProfileKind, ShellProfile, shortcuts::shortcut_label};
use crate::{MainWindow, ShellProfileData, ShortcutData, renderer::measure_cell};
use slint::{ModelRc, SharedString, VecModel};

pub(crate) fn sync_settings_ui(ui: &MainWindow, settings: &AppSettings, mono_fonts: &[String]) {
    let profiles = settings
        .profiles
        .iter()
        .map(|profile| ShellProfileData {
            id: profile.id,
            name: profile.name.clone().into(),
            program: profile.program.clone().into(),
            is_ssh: profile.kind == ProfileKind::Ssh,
            is_default: settings.default_profile_id == Some(profile.id),
        })
        .collect::<Vec<_>>();
    ui.set_shell_profiles(ModelRc::new(VecModel::from(profiles)));
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
    let (cell_width, cell_height) = measure_cell(&settings.font_family, settings.font_size as f32);
    ui.set_terminal_cell_width(cell_width);
    ui.set_terminal_cell_height(cell_height);
    ui.set_settings_font_size(settings.font_size);
    let font_index = mono_fonts
        .iter()
        .position(|family| family.eq_ignore_ascii_case(&settings.font_family))
        .unwrap_or(0);
    ui.set_settings_font_index(font_index.min(i32::MAX as usize) as i32);
}

pub(crate) fn sync_profile_draft(ui: &MainWindow, profile: Option<&ShellProfile>) {
    let Some(profile) = profile else {
        ui.set_selected_profile_id(-1);
        ui.set_profile_name_draft("".into());
        ui.set_profile_kind_index(0);
        ui.set_profile_program_draft("".into());
        ui.set_profile_arguments_draft("".into());
        ui.set_profile_working_directory_draft("".into());
        ui.set_profile_environment_draft("".into());
        ui.set_profile_ssh_host_draft("".into());
        ui.set_profile_ssh_user_draft("".into());
        ui.set_profile_ssh_port_draft(22);
        ui.set_profile_ssh_identity_file_draft("".into());
        ui.set_profile_ssh_password_draft("".into());
        return;
    };
    ui.set_selected_profile_id(profile.id);
    ui.set_profile_name_draft(profile.name.clone().into());
    ui.set_profile_kind_index(match profile.kind {
        ProfileKind::Local => 0,
        ProfileKind::Ssh => 1,
    });
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
    ui.set_profile_environment_draft(
        profile
            .environment
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into(),
    );
    ui.set_profile_ssh_host_draft(profile.ssh_host.clone().into());
    ui.set_profile_ssh_user_draft(profile.ssh_user.clone().into());
    ui.set_profile_ssh_port_draft(i32::from(profile.ssh_port));
    ui.set_profile_ssh_identity_file_draft(
        profile
            .ssh_identity_file
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    ui.set_profile_ssh_password_draft(profile.ssh_password.clone().into());
}
