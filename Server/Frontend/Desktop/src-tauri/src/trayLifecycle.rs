use tauri::{
    App, AppHandle, Manager, Runtime,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::{
    desktopState::DesktopState,
    windowLifecycle::{floatingWindowLabel, restoreMainWorkspace, toggleWindow},
};

const trayIdentifier: &str = "desktop-tray";
const restoreMainMenuIdentifier: &str = "restore-main";
const toggleFloatingMenuIdentifier: &str = "toggle-floating";
const exitMenuIdentifier: &str = "exit";

/// 创建系统托盘与状态切换菜单；菜单项只触发统一切换函数，不复制窗口显示和隐藏分支。
pub fn createTray<R: Runtime>(app: &App<R>) -> tauri::Result<()> {
    let restoreMainMenu = MenuItem::with_id(
        app,
        restoreMainMenuIdentifier,
        "打开主窗口",
        true,
        None::<&str>,
    )?;
    let toggleFloatingMenu = MenuItem::with_id(
        app,
        toggleFloatingMenuIdentifier,
        "显示或隐藏悬浮面板",
        true,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let exitMenu = MenuItem::with_id(app, exitMenuIdentifier, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&restoreMainMenu, &toggleFloatingMenu, &separator, &exitMenu],
    )?;
    let trayIcon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("默认窗口图标".to_owned()))?;

    TrayIconBuilder::with_id(trayIdentifier)
        .icon(trayIcon)
        .tooltip("Sprak Capture 网络数据工作台")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|appHandle, event| processMenuEvent(appHandle, &event))
        .on_tray_icon_event(|trayIcon, event| processTrayEvent(trayIcon, &event))
        .build(app)?;
    Ok(())
}

/// 处理托盘菜单事件；窗口动作按标签复用，退出动作先停止代理服务再结束事件循环。
fn processMenuEvent<R: Runtime>(appHandle: &AppHandle<R>, event: &tauri::menu::MenuEvent) {
    let actionResult = match event.id().as_ref() {
        restoreMainMenuIdentifier => restoreMainWorkspace(appHandle),
        toggleFloatingMenuIdentifier => toggleWindow(appHandle, floatingWindowLabel),
        exitMenuIdentifier => {
            exitApplication(appHandle);
            return;
        }
        _ => return,
    };

    if let Err(error) = actionResult {
        eprintln!("托盘窗口操作失败：{error}");
    }
}

/// 处理托盘左键释放事件；仅在完整点击结束后恢复主窗口，避免按下与释放各触发一次。
fn processTrayEvent<R: Runtime>(trayIcon: &tauri::tray::TrayIcon<R>, event: &TrayIconEvent) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        let appHandle = trayIcon.app_handle();
        if let Err(error) = restoreMainWorkspace(appHandle) {
            eprintln!("托盘恢复主窗口失败：{error}");
        }
    }
}

/// 执行唯一的显式退出序列；状态原子门保证托盘重复点击不会并发回收同一子进程。
pub fn exitApplication<R: Runtime>(appHandle: &AppHandle<R>) {
    let desktopState = appHandle.state::<DesktopState>();
    if desktopState.beginExit() {
        if let Err(error) = desktopState.stopProxyService() {
            eprintln!("退出时停止代理服务失败：{error}");
        }
    }
    appHandle.exit(0);
}
