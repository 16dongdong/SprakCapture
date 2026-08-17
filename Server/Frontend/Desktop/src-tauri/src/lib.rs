mod closePreference;
mod desktopState;
mod processJob;
mod proxyService;
mod trayLifecycle;
mod windowLifecycle;

// 向桌面宿主和独立集成测试公开稳定的服务守护配置与生命周期 API，窗口实现细节仍保持私有。
pub use proxyService::{ProxyServiceConfig, ProxyServiceSupervisor};

use closePreference::{ClosePreferenceStore, MainWindowCloseAction};
use desktopState::DesktopState;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State, Window, WindowEvent};
use windowLifecycle::{
    CloseBehavior, closeBehaviorForWindow, enterBackground, restoreMainWorkspace,
};

const mainWindowCloseRequestedEvent: &str = "desktop://main-window-close-requested";

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
            let closePreferenceStore = ClosePreferenceStore::load(&resourceDirectory)?;
            let proxyServiceConfig = ProxyServiceConfig::fromRuntime(&resourceDirectory);
            let proxyServiceSupervisor = ProxyServiceSupervisor::start(proxyServiceConfig)?;
            app.manage(DesktopState::new(
                proxyServiceSupervisor,
                closePreferenceStore,
            ));
            trayLifecycle::createTray(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            pendingMainWindowClosePrompt,
            cancelMainWindowClose,
            resolveMainWindowClose
        ])
        .on_window_event(|window, event| {
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    let desktopState = window.state::<DesktopState>();
                    if desktopState.isExitRequested() {
                        return;
                    }
                    match closeBehaviorForWindow(window.label()) {
                        CloseBehavior::EnterBackground => {
                            // 系统关闭只负责拦截并通知 WebView；选择和错误均在品牌化自绘对话框内呈现。
                            api.prevent_close();
                            if let Err(error) = processMainWindowClose(window, &desktopState) {
                                eprintln!("处理主窗口关闭请求失败：{error}");
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

/// 处理原生主窗口关闭事件；已记住的动作直接执行，否则向当前 `WebView` 发送唯一自绘询问事件。
fn processMainWindowClose<R: Runtime>(
    window: &Window<R>,
    desktopState: &DesktopState,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(action) = desktopState.rememberedMainWindowCloseAction()? {
        return applyMainWindowCloseAction(window.app_handle(), action);
    }

    if !desktopState.beginMainWindowClosePrompt() {
        return Ok(());
    }
    if let Err(error) = window.emit(mainWindowCloseRequestedEvent, ()) {
        desktopState.finishMainWindowClosePrompt();
        return Err(error.into());
    }
    Ok(())
}

/// 返回是否存在前端尚未接收的关闭询问；该命令与事件订阅共同消除 `WebView` 启动阶段的丢事件窗口。
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri 命令状态必须按提取器值传入，引用无法实现命令参数解析。
fn pendingMainWindowClosePrompt(desktopState: State<'_, DesktopState>) -> bool {
    desktopState.isMainWindowClosePromptOpen()
}

/// 取消当前关闭询问；主窗口保持可见，下一次点击关闭按钮会重新显示询问。
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri 命令状态必须按提取器值传入，引用无法实现命令参数解析。
fn cancelMainWindowClose(desktopState: State<'_, DesktopState>) -> Result<(), String> {
    if !desktopState.cancelMainWindowClosePrompt() {
        return Err("当前没有待取消的主窗口关闭询问".to_owned());
    }
    Ok(())
}

/// 提交自绘询问中的关闭动作；持久化和窗口动作任一失败都会恢复询问状态并返回可显示诊断。
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // AppHandle 与 State 均由 Tauri 命令运行时按值注入。
fn resolveMainWindowClose(
    appHandle: AppHandle,
    desktopState: State<'_, DesktopState>,
    enterTray: bool,
    remember: bool,
) -> Result<(), String> {
    if !desktopState.beginMainWindowCloseResolution() {
        return Err("主窗口关闭询问已经取消或正在处理".to_owned());
    }
    // WebView 只提交问题的布尔答案；持久化枚举留在桌面层，避免前端维护第二套关闭模式模型。
    let action = if enterTray {
        MainWindowCloseAction::EnterTray
    } else {
        MainWindowCloseAction::ExitApplication
    };
    match action {
        MainWindowCloseAction::EnterTray => {
            if let Err(error) = enterBackground(&appHandle) {
                desktopState.reopenMainWindowClosePrompt();
                return Err(format!("进入系统托盘失败：{error}"));
            }
            if remember {
                if let Err(error) = desktopState.rememberMainWindowCloseAction(action) {
                    let restoreResult = restoreMainWorkspace(&appHandle);
                    desktopState.reopenMainWindowClosePrompt();
                    return match restoreResult {
                        Ok(()) => Err(format!("保存关闭选择失败：{error}")),
                        Err(restoreError) => Err(format!(
                            "保存关闭选择失败：{error}；恢复主窗口失败：{restoreError}"
                        )),
                    };
                }
            }
            desktopState.finishMainWindowClosePrompt();
            Ok(())
        }
        MainWindowCloseAction::ExitApplication => {
            // 退出会立即结束 WebView，必须在触发退出前完成原子持久化；失败时应用保持运行并重新开放询问。
            if remember && let Err(error) = desktopState.rememberMainWindowCloseAction(action) {
                desktopState.reopenMainWindowClosePrompt();
                return Err(format!("保存关闭选择失败：{error}"));
            }
            desktopState.finishMainWindowClosePrompt();
            trayLifecycle::exitApplication(&appHandle);
            Ok(())
        }
    }
}

/// 执行统一的关闭动作；进入托盘保留后台服务，直接退出则走托盘菜单共用的完整回收序列。
fn applyMainWindowCloseAction<R: Runtime>(
    appHandle: &AppHandle<R>,
    action: MainWindowCloseAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        MainWindowCloseAction::EnterTray => enterBackground(appHandle).map_err(Into::into),
        MainWindowCloseAction::ExitApplication => {
            trayLifecycle::exitApplication(appHandle);
            Ok(())
        }
    }
}
