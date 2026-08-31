//! 程序入口：选择 Slint 的窗口与渲染后端，然后把控制权交给应用层。

mod app;
mod renderer;
mod terminal;

// 由 build.rs 编译 ui/main.slint 后生成 MainWindow、TabData 等 Rust 类型。
slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 终端画面由自定义 WGPU 渲染器生成，因此这里固定使用可共享纹理的后端组合。
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("femtovg-wgpu".into())
        .require_wgpu_29(slint::wgpu_29::WGPUConfiguration::default())
        .select()?;
    app::run()
}
