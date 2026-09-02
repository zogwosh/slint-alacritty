#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! 程序入口：选择 Slint 的窗口与渲染后端，然后把控制权交给应用层。

mod app;
mod renderer;
mod terminal;

// 由 build.rs 编译 ui/main.slint 后生成 MainWindow、TabData 等 Rust 类型。
slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(result) = terminal::respond_to_askpass_if_requested() {
        result?;
        return Ok(());
    }
    terminal::setup_environment();
    // 终端画面由自定义 WGPU 渲染器生成，因此这里固定使用可共享纹理的后端组合。
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg-wgpu".into())
        .require_wgpu_29(slint::wgpu_29::WGPUConfiguration::default())
        .select()?;
    app::run()
}
