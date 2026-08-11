mod desktopState;
mod proxyService;
mod trayLifecycle;
mod windowLifecycle;

// 向桌面宿主和独立集成测试公开稳定的服务守护配置与生命周期 API，窗口实现细节仍保持私有。
pub use proxyService::{ProxyServiceConfig, ProxyServiceSupervisor};

use desktopState::DesktopState;
use tauri::{Manager, RunEvent, WindowEvent};
use windowLifecycle::{
    CloseBehavior, closeBehaviorForWindow, enterBackground, restoreMainWorkspace,
};

/// 启动桌面外壳；窗口内容完全由同级 Web 工程提供，Rust 侧独占原生生命周期与后台进程。
///
/// # Panics
///
/// Tauri 配置、系统托盘或代理服务监督器初始化失败时终止启动；子进程首启失败由监督器重试，不触发 panic。
pub fn run() {
    let application = tauri::Builder::default()
        // 单实例插件必须首先注册，后续插件和窗口创建才不会在第二实例中产生副作用。
        .plugin(tauri_plugin_single_instance::init(
            |appHandle, _arguments, _workingDirectory| {
                if let Err(error) = restoreMainWorkspace(appHandle) {
                    eprintln!("激活主窗口失败：{error}");
                }
            },
        ))
        .setup(|app| {
            let resourceDirectory = app.path().resource_dir()?;
            let proxyServiceConfig = ProxyServiceConfig::fromRuntime(&resourceDirectory);
            let proxyServiceSupervisor = ProxyServiceSupervisor::start(proxyServiceConfig)?;
            app.manage(DesktopState::new(proxyServiceSupervisor));
            trayLifecycle::createTray(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    let desktopState = window.state::<DesktopState>();
                    if desktopState.isExitRequested() {
                        return;
                    }
                    match closeBehaviorForWindow(window.label()) {
                        CloseBehavior::EnterBackground => {
                            // 主窗口是后台服务的常驻入口：关闭时先启用悬浮面板，再从任务栏收起主窗口。
                            api.prevent_close();
                            if let Err(error) = enterBackground(window.app_handle()) {
                                eprintln!("主窗口转入后台失败：{error}");
                            }
                        }
                        CloseBehavior::HideWindow => {
                            // 悬浮面板由配置声明且需支持托盘重复唤起，因此关闭按钮仅隐藏而不销毁 Webview。
                            api.prevent_close();
                            if let Err(error) = window.hide() {
                                eprintln!("隐藏悬浮面板失败：{error}");
                            }
                        }
                        CloseBehavior::CloseWindow => {
                            // 独立设置与工具窗口保留原生关闭语义，不共享主窗口的后台生命周期。
                        }
                    }
                }
                WindowEvent::Resized(_) if window.label() == windowLifecycle::mainWindowLabel => {
                    // Windows 最小化会产生尺寸事件；只在原生状态确认最小化后进入后台，普通布局调整不受影响。
                    match window.is_minimized() {
                        Ok(true) => {
                            if let Err(error) = enterBackground(window.app_handle()) {
                                eprintln!("主窗口最小化到托盘失败：{error}");
                            }
                        }
                        Ok(false) => {}
                        Err(error) => eprintln!("读取主窗口最小化状态失败：{error}"),
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("桌面外壳初始化失败");

    application.run(|appHandle, event| {
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            let desktopState = appHandle.state::<DesktopState>();
            if desktopState.beginExit() {
                if let Err(error) = desktopState.stopProxyService() {
                    eprintln!("应用退出时停止代理服务失败：{error}");
                }
            }
        }
    });
}
