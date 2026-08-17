use std::{error::Error, fmt};

use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

pub const mainWindowLabel: &str = "main";
pub const floatingWindowLabel: &str = "floating";

/// 描述原生关闭按钮对应的生命周期策略；只有常驻工作区窗口需要拦截，按需创建的工具窗口保持系统默认关闭语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseBehavior {
    EnterBackground,
    HideWindow,
    CloseWindow,
}

/// 表示窗口不存在或原生窗口操作失败；保留窗口标签以便定位具体生命周期边界。
#[derive(Debug)]
pub struct WindowLifecycleError {
    message: String,
}

impl WindowLifecycleError {
    /// 使用窗口标签和底层错误构造精确诊断，不把显示、隐藏与聚焦失败合并为模糊状态。
    fn new(windowLabel: &str, operation: &str, source: impl fmt::Display) -> Self {
        Self {
            message: format!("窗口 `{windowLabel}` {operation}失败：{source}"),
        }
    }
}

impl fmt::Display for WindowLifecycleError {
    /// 输出包含窗口标签和操作名称的中文错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for WindowLifecycleError {}

/// 查找配置中已声明的窗口；窗口缺失属于配置错误而不是可忽略状态。
fn requireWindow<R: Runtime>(
    appHandle: &AppHandle<R>,
    windowLabel: &str,
) -> Result<WebviewWindow<R>, WindowLifecycleError> {
    appHandle
        .get_webview_window(windowLabel)
        .ok_or_else(|| WindowLifecycleError::new(windowLabel, "查找", "配置中不存在该窗口"))
}

/// 根据稳定窗口标签决定关闭行为；未知标签必须直接关闭，避免新增独立设置或工具窗口被主窗口的托盘策略劫持。
pub fn closeBehaviorForWindow(windowLabel: &str) -> CloseBehavior {
    match windowLabel {
        mainWindowLabel => CloseBehavior::EnterBackground,
        floatingWindowLabel => CloseBehavior::HideWindow,
        _ => CloseBehavior::CloseWindow,
    }
}

/// 显示、恢复并聚焦指定窗口；该顺序保证最小化窗口也能回到前台。
pub fn showWindow<R: Runtime>(
    appHandle: &AppHandle<R>,
    windowLabel: &str,
) -> Result<(), WindowLifecycleError> {
    let window = requireWindow(appHandle, windowLabel)?;
    window
        .unminimize()
        .map_err(|error| WindowLifecycleError::new(windowLabel, "恢复", error))?;
    window
        .show()
        .map_err(|error| WindowLifecycleError::new(windowLabel, "显示", error))?;
    window
        .set_focus()
        .map_err(|error| WindowLifecycleError::new(windowLabel, "聚焦", error))
}

/// 隐藏指定窗口但保留 Web 状态与后台服务，供关闭到托盘和悬浮面板复用。
pub fn hideWindow<R: Runtime>(
    appHandle: &AppHandle<R>,
    windowLabel: &str,
) -> Result<(), WindowLifecycleError> {
    requireWindow(appHandle, windowLabel)?
        .hide()
        .map_err(|error| WindowLifecycleError::new(windowLabel, "隐藏", error))
}

/// 根据当前可见状态切换窗口；显示悬浮面板时同步隐藏主窗口，保持两个常驻入口互斥。
pub fn toggleWindow<R: Runtime>(
    appHandle: &AppHandle<R>,
    windowLabel: &str,
) -> Result<(), WindowLifecycleError> {
    let window = requireWindow(appHandle, windowLabel)?;
    let isVisible = window
        .is_visible()
        .map_err(|error| WindowLifecycleError::new(windowLabel, "读取可见状态", error))?;
    if isVisible {
        return hideWindow(appHandle, windowLabel);
    }
    showWindow(appHandle, windowLabel)?;
    if windowLabel == floatingWindowLabel {
        // 托盘显示悬浮面板与主界面按钮使用同一互斥规则，避免两个常驻入口同时占用桌面。
        hideWindow(appHandle, mainWindowLabel)?;
    }
    Ok(())
}

/// 将主工作区切入后台：先显示悬浮面板，再隐藏主窗口，确保代理继续运行时始终保留可见入口。
///
/// 失败语义：悬浮面板无法显示时保留主窗口；悬浮面板已显示但主窗口隐藏失败时返回精确错误，由调用方记录。
pub fn enterBackground<R: Runtime>(appHandle: &AppHandle<R>) -> Result<(), WindowLifecycleError> {
    showWindow(appHandle, floatingWindowLabel)?;
    hideWindow(appHandle, mainWindowLabel)
}

/// 恢复主工作区并收起悬浮面板；先确保主窗口可用，再隐藏辅助入口，避免恢复失败后丢失全部交互窗口。
///
/// 失败语义：主窗口恢复失败时保持悬浮面板不变；悬浮面板隐藏失败时主窗口仍保持可用并返回诊断。
pub fn restoreMainWorkspace<R: Runtime>(
    appHandle: &AppHandle<R>,
) -> Result<(), WindowLifecycleError> {
    showWindow(appHandle, mainWindowLabel)?;
    hideWindow(appHandle, floatingWindowLabel)
}
