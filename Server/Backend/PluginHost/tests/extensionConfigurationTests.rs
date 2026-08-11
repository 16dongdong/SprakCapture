#![allow(non_snake_case)]

use std::collections::BTreeMap;

use plugin_host::{ExtensionConfigurationStore, FailurePolicy, PluginUserConfiguration};
use serde_json::json;
use tempfile::tempdir;

/// 构造包含顺序、配置和秘密引用的完整用户配置。
fn sampleConfiguration() -> PluginUserConfiguration {
    PluginUserConfiguration {
        enabled: true,
        activeVersion: Some("2.3.4".to_owned()),
        moduleOrder: vec!["decoder".to_owned(), "transformer".to_owned()],
        subscriptionOverrides: BTreeMap::new(),
        failurePolicy: FailurePolicy::FailClosed,
        limits: None,
        configurationSchemaVersion: Some("3".to_owned()),
        configuration: json!({ "mode": "strict" }),
        secretReferences: BTreeMap::from([(
            "key".to_owned(),
            "secret://example.protocol/key".to_owned(),
        )]),
        automaticRestart: true,
    }
}

#[test]
fn persistsCompletePluginIntentAndRestoresItAfterRestart() {
    let directory = tempdir().expect("创建配置目录");
    let store = ExtensionConfigurationStore::open(directory.path()).expect("创建配置存储");
    let expected = sampleConfiguration();
    store
        .updatePlugin("example.protocol", expected.clone())
        .expect("持久化插件配置");

    let restored = ExtensionConfigurationStore::open(directory.path()).expect("重建配置存储");
    assert_eq!(
        restored.snapshot().plugins.get("example.protocol"),
        Some(&expected)
    );
}

#[test]
fn rejectsDuplicateModuleOrderWithoutChangingPersistedState() {
    let directory = tempdir().expect("创建配置目录");
    let store = ExtensionConfigurationStore::open(directory.path()).expect("创建配置存储");
    let expected = sampleConfiguration();
    store
        .updatePlugin("example.protocol", expected.clone())
        .expect("写入初始配置");
    let mut invalid = expected.clone();
    invalid.moduleOrder.push("decoder".to_owned());

    assert!(store.updatePlugin("example.protocol", invalid).is_err());
    assert_eq!(
        ExtensionConfigurationStore::open(directory.path())
            .expect("重建配置存储")
            .snapshot()
            .plugins
            .get("example.protocol"),
        Some(&expected)
    );
}

#[test]
fn removesPluginConfigurationIdempotently() {
    let directory = tempdir().expect("创建配置目录");
    let store = ExtensionConfigurationStore::open(directory.path()).expect("创建配置存储");
    store
        .updatePlugin("example.protocol", sampleConfiguration())
        .expect("写入插件配置");
    store
        .removePlugin("example.protocol")
        .expect("移除插件配置");
    store
        .removePlugin("example.protocol")
        .expect("重复移除插件配置");
    assert!(store.snapshot().plugins.is_empty());
}
