#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{fs, path::Path};

use serde_json::{Value, json};

const windowBackgroundColor: &str = "#eef0f3";

/// 读取桌面窗口配置；解析失败表示安装契约无效，测试必须报告原始配置路径。
fn readDesktopConfiguration() -> Value {
    let configurationPath = Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
    serde_json::from_str(
        &fs::read_to_string(&configurationPath)
            .unwrap_or_else(|error| panic!("读取 {} 失败：{error}", configurationPath.display())),
    )
    .unwrap_or_else(|error| panic!("解析 {} 失败：{error}", configurationPath.display()))
}

/// 按稳定标签查找窗口配置；窗口缺失属于桌面安装契约错误而不是可选能力。
fn findWindow<'a>(configuration: &'a Value, windowLabel: &str) -> &'a Value {
    configuration["app"]["windows"]
        .as_array()
        .expect("桌面配置缺少 app.windows 数组")
        .iter()
        .find(|window| window["label"] == windowLabel)
        .unwrap_or_else(|| panic!("桌面配置缺少窗口 {windowLabel}"))
}

/// 验证主窗口使用非透明原生装饰、系统阴影和工作区边界，避免透明 `WebView` 在 Windows 产生黑边或点击穿透。
#[test]
fn mainWindowUsesStableNativeChrome() {
    let configuration = readDesktopConfiguration();
    let mainWindow = findWindow(&configuration, "main");

    assert_eq!(mainWindow["decorations"], true);
    assert_eq!(mainWindow["transparent"], false);
    assert_eq!(mainWindow["shadow"], true);
    assert_eq!(mainWindow["preventOverflow"], true);
    assert_eq!(mainWindow["titleBarStyle"], "Transparent");
    assert_eq!(mainWindow["hiddenTitle"], true);
    assert_eq!(mainWindow["backgroundColor"], windowBackgroundColor);
}

/// 验证悬浮面板保持紧凑、无任务栏重复入口且不可最小化，关闭按钮仍由后台生命周期转换为隐藏。
#[test]
fn floatingWindowKeepsCompactPanelBehavior() {
    let configuration = readDesktopConfiguration();
    let floatingWindow = findWindow(&configuration, "floating");

    assert_eq!(floatingWindow["alwaysOnTop"], true);
    assert_eq!(floatingWindow["skipTaskbar"], true);
    assert_eq!(floatingWindow["maximizable"], false);
    assert_eq!(floatingWindow["minimizable"], false);
    assert_eq!(floatingWindow["closable"], true);
    assert_eq!(floatingWindow["maxWidth"], json!(520));
    assert_eq!(floatingWindow["maxHeight"], json!(420));
    assert_eq!(floatingWindow["backgroundColor"], windowBackgroundColor);
}
