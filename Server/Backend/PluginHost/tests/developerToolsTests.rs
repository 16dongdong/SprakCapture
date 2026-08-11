#![allow(non_snake_case)]

use std::{fs, process::Command};

use plugin_host::{
    ScaffoldOptions, ScaffoldRuntime, checkPluginDirectory, createPluginScaffold, readStageFixture,
    writeDeveloperSchemas,
};
use tempfile::tempdir;

/// 验证开发者从空目录生成的项目立即通过同一权威校验器，并包含可重放阶段夹具。
#[test]
fn createsACompleteExternallyCheckablePluginProject() {
    let workspace = tempdir().expect("创建开发者临时目录");
    let pluginDirectory = workspace.path().join("example.protocol");
    createPluginScaffold(ScaffoldOptions {
        destination: &pluginDirectory,
        pluginId: "example.protocol",
        displayName: "示例协议",
        runtime: ScaffoldRuntime::Wasm,
    })
    .expect("创建插件骨架");

    let manifest = checkPluginDirectory(&pluginDirectory).expect("校验插件骨架");
    assert_eq!(manifest.id, "example.protocol");
    assert_eq!(manifest.modules.len(), 1);
    let fixture =
        readStageFixture(&pluginDirectory.join("fixtures/tcpChunk.json")).expect("校验阶段夹具");
    assert_eq!(fixture.manifest.id, "example.protocol");
}

/// 验证声明入口缺失时目录整体拒绝，避免安装后才暴露半成品包。
#[test]
fn rejectsScaffoldAfterItsDeclaredRuntimeEntryIsRemoved() {
    let workspace = tempdir().expect("创建开发者临时目录");
    let pluginDirectory = workspace.path().join("example.worker");
    createPluginScaffold(ScaffoldOptions {
        destination: &pluginDirectory,
        pluginId: "example.worker",
        displayName: "工作进程协议",
        runtime: ScaffoldRuntime::NativeWorker,
    })
    .expect("创建插件骨架");
    fs::remove_file(pluginDirectory.join("dist/worker.exe")).expect("移除声明入口");

    assert!(checkPluginDirectory(&pluginDirectory).is_err());
}

/// 验证生成的四份公共 Schema 均为可解析 JSON 对象，供多语言 SDK 生成器直接消费。
#[test]
fn generatesAuthoritativeSchemasForExternalSdkTooling() {
    let workspace = tempdir().expect("创建 Schema 临时目录");
    let paths = writeDeveloperSchemas(workspace.path()).expect("生成公共 Schema");
    assert_eq!(paths.len(), 5);
    for path in paths {
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("读取 Schema")).expect("解析 Schema");
        assert!(value.is_object());
    }
}

/// 从外部空目录走通 CLI 创建、校验、Schema 和夹具命令，证明开发入口不依赖宿主私有调用。
#[test]
fn commandLineDeveloperJourneyWorksFromAnEmptyDirectory() {
    let workspace = tempdir().expect("创建 CLI 临时目录");
    let pluginDirectory = workspace.path().join("example.cli");
    let schemaDirectory = workspace.path().join("schemas");
    let executable = env!("CARGO_BIN_EXE_capture-plugin");

    for arguments in [
        vec![
            "new".to_owned(),
            pluginDirectory.display().to_string(),
            "example.cli".to_owned(),
            "--runtime".to_owned(),
            "wasm".to_owned(),
        ],
        vec!["check".to_owned(), pluginDirectory.display().to_string()],
        vec!["schema".to_owned(), schemaDirectory.display().to_string()],
        vec![
            "fixture".to_owned(),
            pluginDirectory
                .join("fixtures/tcpChunk.json")
                .display()
                .to_string(),
        ],
    ] {
        let output = Command::new(executable)
            .args(arguments)
            .output()
            .expect("执行 capture-plugin");
        assert!(
            output.status.success(),
            "命令失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(
        schemaDirectory
            .join("pluginPlatformConfiguration.schema.json")
            .is_file()
    );
}
