#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
// 集成测试按路径复用纯策略函数；同文件中的原生窗口操作由应用目标覆盖，避免为测试扩大公开 API。
#![allow(dead_code)]

use std::{fs, path::Path};

#[path = "../src/windowLifecycle.rs"]
mod windowLifecycle;

use windowLifecycle::{
    CloseBehavior, closeBehaviorForWindow, floatingWindowLabel, mainWindowLabel,
};

const independentWindowPattern: &str = "app-window-*";
const createWebviewPermission: &str = "core:webview:allow-create-webview-window";
const closeWindowPermission: &str = "core:window:allow-close";
const hideWindowPermission: &str = "core:window:allow-hide";
const startDraggingPermission: &str = "core:window:allow-start-dragging";

/// 验证常驻窗口各自采用稳定策略，确保主窗口后台运行和悬浮面板重复唤起不会销毁 Webview。
#[test]
fn persistentWindowsUseBackgroundLifecycle() {
    assert_eq!(
        closeBehaviorForWindow(mainWindowLabel),
        CloseBehavior::EnterBackground
    );
    assert_eq!(
        closeBehaviorForWindow(floatingWindowLabel),
        CloseBehavior::HideWindow
    );
}

/// 验证动态工具窗口不继承常驻窗口策略，使设置、证书和映射窗口的关闭按钮只作用于自身。
#[test]
fn independentWindowsKeepNativeCloseBehavior() {
    for windowLabel in ["settings", "ssl-settings", "protocol-settings", "mapping"] {
        assert_eq!(
            closeBehaviorForWindow(windowLabel),
            CloseBehavior::CloseWindow
        );
    }
}

/// 验证动态窗口能力与前端 label 合同保持一致，并允许独立窗口关闭自身或继续打开下一级业务窗口。
#[test]
fn independentWindowCapabilityMatchesManagedWindowContract() {
    let capabilityPath = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("capabilities")
        .join("independentWindows.json");
    let capability: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(capabilityPath).expect("读取独立窗口能力清单失败"),
    )
    .expect("解析独立窗口能力清单失败");
    let windows = capability["windows"]
        .as_array()
        .expect("能力清单缺少 windows 数组");
    let permissions = capability["permissions"]
        .as_array()
        .expect("能力清单缺少 permissions 数组");

    assert!(
        windows
            .iter()
            .any(|value| value == independentWindowPattern)
    );
    for permission in [createWebviewPermission, closeWindowPermission] {
        assert!(
            permissions.iter().any(|value| value == permission),
            "独立窗口能力缺少权限 {permission}"
        );
    }
}

/// 验证常驻窗口能力清单允许前端调用自绘标题区拖动 API，避免无标题栏后失去移动入口。
#[test]
fn defaultWindowCapabilityAllowsFloatingDrag() {
    let capabilityPath = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("capabilities")
        .join("default.json");
    let capability: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(capabilityPath).expect("读取常驻窗口能力清单失败"),
    )
    .expect("解析常驻窗口能力清单失败");
    let permissions = capability["permissions"]
        .as_array()
        .expect("常驻窗口能力清单缺少 permissions 数组");
    assert!(
        permissions
            .iter()
            .any(|value| value == startDraggingPermission),
        "常驻窗口能力缺少悬浮窗拖动权限"
    );
    assert!(
        permissions
            .iter()
            .any(|value| value == hideWindowPermission),
        "常驻窗口能力缺少互斥隐藏权限"
    );
}
