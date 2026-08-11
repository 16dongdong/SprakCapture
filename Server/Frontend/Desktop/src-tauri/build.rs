/// 生成 Tauri 编译期配置与 Windows 资源，构建失败由构建脚本直接向 Cargo 返回。
fn main() {
    tauri_build::build();
}
