#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// 进入 Tauri 桌面外壳；具体窗口、托盘和代理服务生命周期由库入口集中管理。
fn main() {
    desktop_shell_lib::run();
}
