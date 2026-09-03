//! Rust 设置状态到 Slint 属性和模型的同步。

use super::{
    AppSettings, MAX_FONT_SIZE, MAX_SCROLL_LINES, MAX_SCROLLBACK_LINES, MIN_FONT_SIZE, ProfileKind,
    ShellProfile, shortcuts::shortcut_label,
};
use crate::{
    MainWindow, SettingsLimits, ShellProfileData, ShortcutData, TerminalSettingsData,
    renderer::measure_cell,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

/// 把 Rust 侧的取值范围常量写入 Slint，设置页的输入控件据此限制范围；启动时调用一次即可。
pub(crate) fn sync_settings_limits(ui: &MainWindow) {
    let limits = ui.global::<SettingsLimits>();
    limits.set_min_font_size(MIN_FONT_SIZE);
    limits.set_max_font_size(MAX_FONT_SIZE);
    limits.set_max_scrollback_lines(MAX_SCROLLBACK_LINES as i32);
    limits.set_max_scroll_lines(MAX_SCROLL_LINES as i32);
}

/// 以窗口当前缩放在物理像素下测量单元尺寸，并写入 Slint 作为唯一的度量真源。
/// 缩放或字体变化时都必须重新调用。
pub(crate) fn sync_font_metrics(ui: &MainWindow, settings: &AppSettings) {
    let scale = ui.window().scale_factor().max(0.01);
    let metrics = measure_cell(&settings.font_family, settings.font_size as f32 * scale);
    ui.set_scale_factor(scale);
    ui.set_terminal_cell_width_px(metrics.width.min(i32::MAX as u32) as i32);
    ui.set_terminal_cell_height_px(metrics.height.min(i32::MAX as u32) as i32);
}

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
    sync_font_metrics(ui, settings);
    let font_index = mono_fonts
        .iter()
        .position(|family| family.eq_ignore_ascii_case(&settings.font_family))
        .unwrap_or(0);
    // 已保存的值覆盖设置页草稿：保存或热重载之后，草稿都应回到磁盘上的实际状态。
    ui.set_terminal_settings_draft(TerminalSettingsData {
        font_index: font_index.min(i32::MAX as usize) as i32,
        font_size: settings.font_size,
        // 上限已由 validate/normalize 保证在 i32 范围内。
        scrollback_lines: settings.scrollback_lines as i32,
        scroll_lines: settings.scroll_lines as i32,
        cursor_shape_index: settings.cursor_shape.index(),
        cursor_blink: settings.cursor_blink,
        copy_on_select: settings.copy_on_select,
        right_click_paste: settings.right_click_paste,
    });
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
