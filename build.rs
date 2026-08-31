fn main() {
    // 编译 Slint 根文件及其导入项，并生成供 src/main.rs 引入的 Rust 绑定。
    slint_build::compile("ui/main.slint").unwrap();
}
