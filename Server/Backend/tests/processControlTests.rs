#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(unused_imports)]

use std::time::Duration;

use axum::http::{Method, StatusCode};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use prost::Message;
use prost_types::{DescriptorProto, FileDescriptorProto, FileDescriptorSet};
use serde_json::json;
use tokio::{io::AsyncReadExt, net::TcpStream, time::timeout};

use proxy_backend::controlApi::{ControlState, createControlRouter};

#[path = "support/controlApiSupport.rs"]
mod controlApiSupport;

use controlApiSupport::{configurationJson, findAvailablePort, requestJson};

/// 构造最小合法的 proto3 描述符，用于验证描述符文件和协议配置会一起跨控制进程恢复。
fn persistenceDescriptorBase64() -> String {
    let descriptor = FileDescriptorSet {
        file: vec![FileDescriptorProto {
            name: Some("persistent.proto".to_owned()),
            package: Some("persistent".to_owned()),
            syntax: Some("proto3".to_owned()),
            message_type: vec![DescriptorProto {
                name: Some("Envelope".to_owned()),
                ..DescriptorProto::default()
            }],
            ..FileDescriptorProto::default()
        }],
    };
    let mut bytes = Vec::new();
    descriptor
        .encode(&mut bytes)
        .expect("应编码持久化描述符夹具");
    STANDARD.encode(bytes)
}

/// 验证首次运行生成统一配置文件，路径选择写入后由新控制实例恢复且不依赖旧 PID。
#[tokio::test]
async fn unifiedConfigurationPersistsProcessPathsAcrossRestarts() {
    let dataDirectory = tempfile::tempdir().expect("应创建测试数据目录");
    let firstState = ControlState::newWithDataDirectory(dataDirectory.path())
        .await
        .expect("首次控制状态应创建配置文件");
    let configurationPath = dataDirectory.path().join("configuration.json");
    assert!(configurationPath.is_file());

    let firstRouter = createControlRouter(firstState);
    let (processStatus, processSnapshot) = requestJson(
        firstRouter.clone(),
        Method::GET,
        "/api/v1/processes",
        json!({}),
    )
    .await;
    assert_eq!(processStatus, StatusCode::OK);
    #[cfg(windows)]
    assert!(
        processSnapshot["processIcons"]
            .as_object()
            .is_some_and(|icons| icons.values().any(|icon| {
                icon.as_str()
                    .is_some_and(|value| value.starts_with("data:image/png;base64,iVBOR"))
            })),
        "Windows 进程快照应包含可直接显示的 PNG 图标",
    );
    let executablePath = processSnapshot["processes"]
        .as_array()
        .and_then(|processes| processes.first())
        .and_then(|process| process["executablePath"].as_str())
        .expect("测试进程表应包含至少一个可执行路径")
        .to_owned();
    let (updateStatus, updatedSnapshot) = requestJson(
        firstRouter,
        Method::PUT,
        "/api/v1/processes",
        json!({ "enabled": false, "selectedPaths": [executablePath] }),
    )
    .await;
    assert_eq!(updateStatus, StatusCode::OK);
    assert_eq!(
        updatedSnapshot["selectedPaths"].as_array().unwrap().len(),
        1
    );

    let secondState = ControlState::newWithDataDirectory(dataDirectory.path())
        .await
        .expect("新控制状态应恢复统一配置");
    let (restoredStatus, restoredSnapshot) = requestJson(
        createControlRouter(secondState.clone()),
        Method::GET,
        "/api/v1/processes",
        json!({}),
    )
    .await;
    assert_eq!(restoredStatus, StatusCode::OK);
    assert_eq!(
        restoredSnapshot["selectedPaths"],
        updatedSnapshot["selectedPaths"]
    );
}

/// 验证规则型工具和 SSL 主机范围写入统一配置文件，并由新控制实例完整恢复。
#[tokio::test]
async fn unifiedConfigurationPersistsMappingAndSslRulesAcrossRestarts() {
    let dataDirectory = tempfile::tempdir().expect("应创建规则持久化测试目录");
    let firstState = ControlState::newWithDataDirectory(dataDirectory.path())
        .await
        .expect("首次控制状态应创建统一配置");
    let firstRouter = createControlRouter(firstState.clone());
    let mapLocalConfiguration = json!({
        "enabled": true,
        "rules": [{
            "id": "persistent-local-rule",
            "enabled": true,
            "location": {
                "protocol": "https",
                "host": "media.example.test",
                "port": "443",
                "path": "/audio/*",
                "query": null
            },
            "localPath": "fixtures/audio.mp4",
            "isDirectory": false,
            "statusCode": 200,
            "responseHeaders": [],
            "contentTypeOverride": "audio/mp4"
        }]
    });
    let sslConfiguration = json!({
        "enabled": true,
        "includeLocations": [{
            "protocol": "https",
            "host": "media.example.test",
            "port": "443",
            "path": "",
            "query": null
        }],
        "excludeLocations": [],
        "maxCachedCertificates": 64,
        "useClientSni": true
    });
    let auxiliaryConfiguration = json!([{
        "id": "persistent-forward",
        "enabled": true,
        "listenHost": "127.0.0.1",
        "listenPort": findAvailablePort(),
        "targetHost": "127.0.0.1",
        "targetPort": 6553
    }]);
    let validateConfiguration = json!({
        "enabled": false,
        "validators": [{"id": "htmlWellFormed", "enabled": true}],
        "allowOnlineValidators": false,
        "w3cEndpoint": "https://validator.w3.org/nu/?out=json"
    });
    let packetFilterConfiguration = json!({
        "enabled": true,
        "rules": [{
            "id": "persistent-packet-filter",
            "name": "替换协议标记",
            "enabled": true,
            "transport": "tcp",
            "direction": "up",
            "host": "*.example.test",
            "port": 443,
            "minimumLength": 4,
            "maximumLength": 4096,
            "pattern": "01 ?? 03 04",
            "replacement": "AA ?? BB CC",
            "action": "modify",
            "replaceAll": true,
            "continueMatching": false
        }]
    });

    let (mapStatus, mapResponse) = requestJson(
        firstRouter.clone(),
        Method::PUT,
        "/api/v1/tools/mapLocal",
        mapLocalConfiguration.clone(),
    )
    .await;
    assert_eq!(mapStatus, StatusCode::OK, "{mapResponse}");
    let (sslStatus, sslResponse) = requestJson(
        firstRouter,
        Method::PUT,
        "/api/v1/ssl",
        sslConfiguration.clone(),
    )
    .await;
    assert_eq!(sslStatus, StatusCode::OK, "{sslResponse}");
    let (auxiliaryStatus, auxiliaryResponse) = requestJson(
        createControlRouter(firstState.clone()),
        Method::PUT,
        "/api/v1/listeners/portForwards",
        auxiliaryConfiguration.clone(),
    )
    .await;
    assert_eq!(auxiliaryStatus, StatusCode::OK, "{auxiliaryResponse}");
    let (validateStatus, validateResponse) = requestJson(
        createControlRouter(firstState.clone()),
        Method::PUT,
        "/api/v1/tools/validate",
        validateConfiguration.clone(),
    )
    .await;
    assert_eq!(validateStatus, StatusCode::OK, "{validateResponse}");
    let (packetFilterStatus, packetFilterResponse) = requestJson(
        createControlRouter(firstState.clone()),
        Method::PUT,
        "/api/v1/tools/packetFilters",
        packetFilterConfiguration.clone(),
    )
    .await;
    assert_eq!(packetFilterStatus, StatusCode::OK, "{packetFilterResponse}");
    let (protobufStatus, protobufResponse) = requestJson(
        createControlRouter(firstState.clone()),
        Method::PUT,
        "/api/v1/tools/protobuf",
        json!({"enabled": true, "routes": []}),
    )
    .await;
    assert_eq!(protobufStatus, StatusCode::OK, "{protobufResponse}");
    let (descriptorStatus, descriptorResponse) = requestJson(
        createControlRouter(firstState.clone()),
        Method::POST,
        "/api/v1/tools/protobuf/schemas",
        json!({
            "name": "持久化描述符",
            "defaultMessageType": "persistent.Envelope",
            "base64": persistenceDescriptorBase64()
        }),
    )
    .await;
    assert_eq!(descriptorStatus, StatusCode::OK, "{descriptorResponse}");
    firstState.beginShutdown();
    drop(firstState);

    let secondState = ControlState::newWithDataDirectory(dataDirectory.path())
        .await
        .expect("新控制实例应恢复规则配置");
    let secondRouter = createControlRouter(secondState.clone());
    let (restoredMapStatus, restoredMap) = requestJson(
        secondRouter.clone(),
        Method::GET,
        "/api/v1/tools/mapLocal",
        json!({}),
    )
    .await;
    assert_eq!(restoredMapStatus, StatusCode::OK);
    assert_eq!(restoredMap, mapLocalConfiguration);
    let (restoredSslStatus, restoredSsl) =
        requestJson(secondRouter, Method::GET, "/api/v1/ssl", json!({})).await;
    assert_eq!(restoredSslStatus, StatusCode::OK);
    assert_eq!(restoredSsl["enabled"], sslConfiguration["enabled"]);
    assert_eq!(
        restoredSsl["includeLocations"],
        sslConfiguration["includeLocations"]
    );
    assert_eq!(
        restoredSsl["maxCachedCertificates"],
        sslConfiguration["maxCachedCertificates"]
    );
    let (restoredAuxiliaryStatus, restoredAuxiliary) = requestJson(
        createControlRouter(secondState.clone()),
        Method::GET,
        "/api/v1/listeners/portForwards",
        json!({}),
    )
    .await;
    assert_eq!(restoredAuxiliaryStatus, StatusCode::OK);
    assert_eq!(
        restoredAuxiliary["configuration"]["portForwards"],
        auxiliaryConfiguration
    );
    let (restoredValidateStatus, restoredValidate) = requestJson(
        createControlRouter(secondState.clone()),
        Method::GET,
        "/api/v1/tools/validate",
        json!({}),
    )
    .await;
    assert_eq!(restoredValidateStatus, StatusCode::OK);
    assert_eq!(restoredValidate, validateConfiguration);
    let (restoredPacketFilterStatus, restoredPacketFilter) = requestJson(
        createControlRouter(secondState.clone()),
        Method::GET,
        "/api/v1/tools/packetFilters",
        json!({}),
    )
    .await;
    assert_eq!(restoredPacketFilterStatus, StatusCode::OK);
    assert_eq!(restoredPacketFilter, packetFilterConfiguration);
    let (restoredProtobufStatus, restoredProtobuf) = requestJson(
        createControlRouter(secondState),
        Method::GET,
        "/api/v1/tools/protobuf",
        json!({}),
    )
    .await;
    assert_eq!(restoredProtobufStatus, StatusCode::OK);
    assert_eq!(restoredProtobuf["enabled"], true);
    assert_eq!(
        restoredProtobuf["schemas"].as_array().map(Vec::len),
        Some(1)
    );
}

/// 验证录制暂停、忽略规则与隧道元数据开关写入统一配置，并由新控制进程恢复。
#[tokio::test]
async fn unifiedConfigurationPersistsRecordingPreferencesAcrossRestarts() {
    let dataDirectory = tempfile::tempdir().expect("应创建录制持久化测试目录");
    let firstState = ControlState::newWithDataDirectory(dataDirectory.path())
        .await
        .expect("首次控制状态应创建统一配置");
    let recordingUpdate = json!({
        "state": "paused",
        "ignoreLocations": [{
            "protocol": "https",
            "host": "telemetry.example.test",
            "port": "443",
            "path": "/events/*",
            "query": null
        }],
        "recordTunnelMetadata": false
    });
    let (updateStatus, updateResponse) = requestJson(
        createControlRouter(firstState.clone()),
        Method::PUT,
        "/api/v1/recording",
        recordingUpdate.clone(),
    )
    .await;
    assert_eq!(updateStatus, StatusCode::OK, "{updateResponse}");
    assert_eq!(updateResponse["recording"]["state"], "paused");
    assert_eq!(updateResponse["recording"]["recordTunnelMetadata"], false);
    let (invalidStatus, invalidResponse) = requestJson(
        createControlRouter(firstState.clone()),
        Method::PUT,
        "/api/v1/recording",
        json!({
            "ignoreLocations": [{
                "protocol": "invalid-protocol",
                "host": "telemetry.example.test",
                "port": "443",
                "path": "/events/*",
                "query": null
            }]
        }),
    )
    .await;
    assert_eq!(invalidStatus, StatusCode::BAD_REQUEST, "{invalidResponse}");
    firstState.beginShutdown();
    drop(firstState);

    let persisted: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dataDirectory.path().join("configuration.json"))
            .expect("应读取录制配置文件"),
    )
    .expect("录制配置文件应为合法 JSON");
    assert_eq!(persisted["recording"], recordingUpdate);

    let secondState = ControlState::newWithDataDirectory(dataDirectory.path())
        .await
        .expect("新控制进程应恢复录制偏好");
    let (restoredStatus, restoredResponse) = requestJson(
        createControlRouter(secondState),
        Method::GET,
        "/api/v1/recording",
        json!({}),
    )
    .await;
    assert_eq!(restoredStatus, StatusCode::OK);
    assert_eq!(restoredResponse["recording"]["state"], "paused");
    assert_eq!(
        restoredResponse["recording"]["ignoreLocations"],
        recordingUpdate["ignoreLocations"]
    );
    assert_eq!(restoredResponse["recording"]["recordTunnelMetadata"], false);
}

/// 验证旧版统一配置缺少 SSL/工具字段时按默认值迁移，并在本次启动写回完整结构。
#[tokio::test]
async fn legacyConfigurationAddsPersistentRuleSectionsDuringStartup() {
    let dataDirectory = tempfile::tempdir().expect("应创建旧配置迁移测试目录");
    let configurationPath = dataDirectory.path().join("configuration.json");
    std::fs::write(
        &configurationPath,
        serde_json::to_vec_pretty(&json!({
            "service": null,
            "processSelection": {
                "enabled": false,
                "selectedPaths": []
            }
        }))
        .expect("应序列化旧配置夹具"),
    )
    .expect("应写入旧配置夹具");

    let state = ControlState::newWithDataDirectory(dataDirectory.path())
        .await
        .expect("旧配置应完成向后兼容迁移");
    state.beginShutdown();
    let migrated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(configurationPath).expect("应读取迁移后的统一配置"))
            .expect("迁移后的统一配置应为合法 JSON");
    assert_eq!(migrated["ssl"]["enabled"], false);
    assert_eq!(migrated["tools"]["mapLocal"]["rules"], json!([]));
    assert_eq!(migrated["tools"]["mapRemote"]["rules"], json!([]));
    assert_eq!(migrated["auxiliaryListeners"]["portForwards"], json!([]));
    assert_eq!(migrated["protocols"]["protobuf"]["enabled"], false);
    assert_eq!(migrated["recording"]["state"], "recording");
    assert_eq!(migrated["recording"]["ignoreLocations"], json!([]));
    assert_eq!(migrated["recording"]["recordTunnelMetadata"], true);
}

/// 验证捕获保持关闭时只持久化路径，不访问未运行的驱动，也不重启融合监听或断开现有客户端。
#[tokio::test]
async fn processPathUpdateKeepsFusedListenerConnectionsAlive() {
    let dataDirectory = tempfile::tempdir().expect("应创建热更新测试目录");
    let state = ControlState::newWithDataDirectory(dataDirectory.path())
        .await
        .expect("应创建热更新控制状态");
    let router = createControlRouter(state.clone());
    let listenPort = findAvailablePort();
    let (configurationStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(listenPort),
    )
    .await;
    assert_eq!(configurationStatus, StatusCode::OK);
    state.startService().await.expect("应启动融合监听");

    let mut connection = TcpStream::connect(("127.0.0.1", listenPort))
        .await
        .expect("应建立待观察连接");
    let executablePath = std::env::current_exe()
        .expect("应读取测试进程路径")
        .to_string_lossy()
        .into_owned();
    let (updateStatus, updateResponse) = requestJson(
        router,
        Method::PUT,
        "/api/v1/processes",
        json!({ "enabled": false, "selectedPaths": [executablePath] }),
    )
    .await;
    assert_eq!(updateStatus, StatusCode::OK, "{updateResponse}");

    let mut byte = [0_u8; 1];
    assert!(
        timeout(Duration::from_millis(150), connection.read(&mut byte))
            .await
            .is_err(),
        "路径热更新不得关闭既有代理连接",
    );
    state.stopService().await.expect("应停止热更新测试服务");
}
