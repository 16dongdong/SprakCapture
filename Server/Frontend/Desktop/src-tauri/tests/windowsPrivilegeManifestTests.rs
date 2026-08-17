#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{fs, path::Path};

/// 验证桌面可执行文件声明管理员启动且保留 Tauri 所需的 Common Controls 依赖。
///
/// `WinDivert` 句柄由桌面监督的后端子进程打开，子进程只能继承桌面令牌；该测试防止构建脚本
/// 回退到默认无权限清单，使错误再次延迟到用户保存进程选择时才出现。
#[test]
fn desktopManifestRequiresAdministratorToken() {
    let manifestPath = Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-app-manifest.xml");
    let manifest = fs::read_to_string(manifestPath).expect("读取 Windows 应用清单失败");

    assert!(
        manifest.contains("requestedExecutionLevel level=\"requireAdministrator\""),
        "桌面应用清单必须声明管理员启动"
    );
    assert!(
        !manifest.contains("requestedExecutionLevel level=\"asInvoker\""),
        "桌面应用清单不得回退为普通调用者令牌"
    );
    assert!(
        manifest.contains("Microsoft.Windows.Common-Controls"),
        "自定义清单必须保留 Tauri 原生控件依赖"
    );
}

/// 验证构建脚本确实把权限清单交给 Tauri 资源编译器，而不是只把 XML 留在源码目录。
#[test]
fn buildScriptEmbedsPrivilegeManifest() {
    let buildScriptPath = Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs");
    let buildScript = fs::read_to_string(buildScriptPath).expect("读取桌面构建脚本失败");

    assert!(
        buildScript.contains("include_str!(\"windows-app-manifest.xml\")"),
        "构建脚本必须读取权限清单"
    );
    assert!(
        buildScript.contains(".app_manifest(windowsAppManifest)"),
        "构建脚本必须将权限清单嵌入可执行文件"
    );
}
