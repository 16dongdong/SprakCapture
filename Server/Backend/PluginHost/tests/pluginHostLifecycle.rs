use std::{fs, io::Write};

use plugin_host::{
    ConnectionMetadata, HookActionResult, PluginHost, PluginHostError, PluginState,
    StreamDirection, TransportKind,
};
use serde_json::json;
use tempfile::tempdir;
use zip::{ZipWriter, write::SimpleFileOptions};

/** 写入仅包含文件条目的内存 ZIP，用于验证受控安装路径，不向仓库保留构建夹具。 */
fn create_plugin_package(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, content) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .expect("创建压缩包条目");
        writer.write_all(content).expect("写入压缩包条目");
    }
    writer.finish().expect("完成压缩包").into_inner()
}

/// 验证空 manifest 可以被发现并启用，且没有 Hook 时字节路径保持透明。
#[test]
fn discovers_and_enables_manifest_only_plugin() {
    let directory = tempdir().expect("创建临时目录");
    let plugin_directory = directory.path().join("sample");
    fs::create_dir_all(&plugin_directory).expect("创建插件目录");
    fs::write(
        plugin_directory.join("plugin.json"),
        r#"{"id":"sample.plugin","name":"样例","version":"1.0.0","apiVersion":1}"#,
    )
    .expect("写入 manifest");
    let host = PluginHost::new(directory.path()).expect("创建宿主");
    assert_eq!(host.snapshots()[0].state, PluginState::Disabled);
    host.enable("sample.plugin").expect("启用空插件");
    let connection = host.openConnection(ConnectionMetadata {
        transport: TransportKind::Tcp,
        clientAddress: "127.0.0.1:1".to_owned(),
        targetHost: "example.test".to_owned(),
        targetPort: 443,
    });
    let mut bytes = [1_u8, 2, 3];
    assert_eq!(
        host.processStreamData(&connection, StreamDirection::ClientToServer, &mut bytes),
        HookActionResult::Forward { length: 3 }
    );
    host.closeConnection(connection);
    drop(host);

    let restored_host = PluginHost::new(directory.path()).expect("重建插件宿主");
    assert_eq!(restored_host.snapshots()[0].state, PluginState::Enabled);
}

/// 验证宿主会在生命周期变更后唤醒控制面；通知只表达“快照已变化”，避免把内部锁状态复制到事件协议。
#[test]
fn publishes_plugin_lifecycle_changes() {
    let directory = tempdir().expect("创建临时目录");
    let plugin_directory = directory.path().join("sample");
    fs::create_dir_all(&plugin_directory).expect("创建插件目录");
    fs::write(
        plugin_directory.join("plugin.json"),
        r#"{"id":"sample.plugin","name":"样例","version":"1.0.0","apiVersion":1}"#,
    )
    .expect("写入 manifest");
    let host = PluginHost::new(directory.path()).expect("创建宿主");
    let receiver = host.subscribeChanges();

    host.enable("sample.plugin").expect("启用插件");

    assert!(receiver.has_changed().expect("读取插件变化通知"));
    assert_eq!(host.snapshots()[0].state, PluginState::Enabled);
}

/// 验证 apiVersion 不兼容插件不会被启用，避免半兼容 ABI 进入数据面。
#[test]
fn rejects_incompatible_api_version() {
    let directory = tempdir().expect("创建临时目录");
    let plugin_directory = directory.path().join("sample");
    fs::create_dir_all(&plugin_directory).expect("创建插件目录");
    fs::write(
        plugin_directory.join("plugin.json"),
        r#"{"id":"sample.plugin","name":"样例","version":"1.0.0","apiVersion":2}"#,
    )
    .expect("写入 manifest");
    let host = PluginHost::new(directory.path()).expect("创建宿主");
    assert!(host.enable("sample.plugin").is_err());
}

/// 验证读取详情会剥离秘密字段，更新时空缺秘密字段沿用磁盘值，不会回传到控制面。
#[test]
fn redacts_and_preserves_secret_configuration() {
    let directory = tempdir().expect("创建临时目录");
    let plugin_directory = directory.path().join("sample");
    fs::create_dir_all(&plugin_directory).expect("创建插件目录");
    fs::write(
        plugin_directory.join("plugin.json"),
        r#"{
            "id":"sample.plugin",
            "name":"样例",
            "version":"1.0.0",
            "apiVersion":1,
            "configSchema":{
                "type":"object",
                "properties":{
                    "endpoint":{"type":"string","title":"端点"},
                    "token":{"type":"string","format":"password","title":"令牌"}
                },
                "required":["endpoint","token"]
            }
        }"#,
    )
    .expect("写入 manifest");
    fs::write(
        plugin_directory.join("config.json"),
        r#"{"endpoint":"https://before.test","token":"secret-value"}"#,
    )
    .expect("写入配置");
    let host = PluginHost::new(directory.path()).expect("创建宿主");
    let details = host.details("sample.plugin").expect("读取脱敏详情");
    assert_eq!(
        details.configuration,
        json!({ "endpoint": "https://before.test" })
    );
    assert_eq!(details.configuredSecretFields, vec!["token"]);

    let details = host
        .updateConfiguration("sample.plugin", json!({ "endpoint": "https://after.test" }))
        .expect("保存非秘密字段");
    assert_eq!(
        details.configuration,
        json!({ "endpoint": "https://after.test" })
    );
    assert_eq!(details.configuredSecretFields, vec!["token"]);
    let stored: serde_json::Value = serde_json::from_slice(
        &fs::read(plugin_directory.join("config.json")).expect("读取持久化配置"),
    )
    .expect("解析持久化配置");
    assert_eq!(
        stored,
        json!({ "endpoint": "https://after.test", "token": "secret-value" })
    );
    drop(host);

    let restored_host = PluginHost::new(directory.path()).expect("重建插件配置宿主");
    let restored_details = restored_host
        .details("sample.plugin")
        .expect("读取重启后的脱敏配置");
    assert_eq!(
        restored_details.configuration,
        json!({ "endpoint": "https://after.test" })
    );
    assert_eq!(restored_details.configuredSecretFields, vec!["token"]);
}

/// 验证根 manifest 的插件包安装、重复安装拒绝和卸载目录清理共用同一生命周期状态机。
#[test]
fn installs_and_uninstalls_plugin_package() {
    let directory = tempdir().expect("创建临时目录");
    let package = create_plugin_package(&[(
        "plugin.json",
        r#"{"id":"sample.plugin","name":"样例","version":"1.0.0","apiVersion":1}"#.as_bytes(),
    )]);
    let host = PluginHost::new(directory.path()).expect("创建宿主");
    let snapshot = host.installPackage(&package).expect("安装插件包");
    assert_eq!(snapshot.id, "sample.plugin");
    assert_eq!(snapshot.state, PluginState::Disabled);
    assert!(directory.path().join("sample.plugin/plugin.json").is_file());
    assert!(matches!(
        host.installPackage(&package),
        Err(PluginHostError::AlreadyInstalled)
    ));

    host.uninstall("sample.plugin").expect("卸载插件包");
    assert!(host.snapshots().is_empty());
    assert!(!directory.path().join("sample.plugin").exists());
}

/// 验证压缩包内含导航路径时安装整体失败，暂存目录和宿主根目录都不保留越界文件。
#[test]
fn rejects_path_traversal_in_plugin_package() {
    let directory = tempdir().expect("创建临时目录");
    let package = create_plugin_package(&[
        (
            "plugin.json",
            r#"{"id":"sample.plugin","name":"样例","version":"1.0.0","apiVersion":1}"#.as_bytes(),
        ),
        ("../outside.txt", b"forbidden"),
    ]);
    let host = PluginHost::new(directory.path()).expect("创建宿主");
    assert!(matches!(
        host.installPackage(&package),
        Err(PluginHostError::Package)
    ));
    assert!(host.snapshots().is_empty());
    assert!(!directory.path().join("outside.txt").exists());
}
