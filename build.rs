fn main() {
    // 控件与主题均由项目内 Slint 组件和 DesignTokens 提供。
    slint_build::compile("ui/main.slint").unwrap();
}
