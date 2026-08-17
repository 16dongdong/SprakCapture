const windowsAppManifest: &str = include_str!("windows-app-manifest.xml");

/// 生成 Tauri 编译期配置与 Windows 资源。
///
/// 桌面外壳必须以管理员令牌启动，受其监督的 `proxyService` 才能继承同一令牌并打开
/// `WinDivert` 的 SOCKET、FLOW 与 NETWORK 层。权限声明必须嵌入桌面可执行文件；普通
/// `Command` 子进程无法在不改变启动链的情况下自行提升权限。构建资源失败时直接终止
/// Cargo 构建，禁止产出会在运行期延迟报权限错误的桌面程序。
fn main() {
    let windowsAttributes = tauri_build::WindowsAttributes::new().app_manifest(windowsAppManifest);
    let attributes = tauri_build::Attributes::new().windows_attributes(windowsAttributes);
    tauri_build::try_build(attributes).expect("生成 Tauri 桌面资源失败");
}
