#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(unused_imports)]

use axum::http::{Method, StatusCode};
use proxy_backend::controlApi::{ControlState, createControlRouter};
use serde_json::{Value, json};

#[path = "support/controlApiSupport.rs"]
mod controlApiSupport;

use controlApiSupport::requestJson;

/// 返回覆盖域名拒绝与进程不录制动作的完整规则配置，供写入与恢复断言共用。
fn recordingRulesConfiguration() -> Value {
    json!({
        "enabled": true,
        "defaultAction": "record",
        "ruleSets": [{
            "id": "default",
            "name": "默认规则",
            "enabled": true,
            "rules": [{
                "id": "rejectAds",
                "enabled": true,
                "kind": "domainSuffix",
                "value": "ads.example",
                "action": "reject"
            }, {
                "id": "ignoreUpdater",
                "enabled": true,
                "kind": "processName",
                "value": "updater.exe",
                "action": "doNotRecord"
            }]
        }]
    })
}

#[tokio::test]
async fn recordingRulesPersistAndReloadFromUnifiedConfiguration() {
    let directory = tempfile::tempdir().expect("创建隔离配置目录");
    let state = ControlState::newWithDataDirectory(directory.path())
        .await
        .expect("创建首代控制状态");
    let router = createControlRouter(state.clone());
    let expected = recordingRulesConfiguration();
    let (status, response) = requestJson(
        router,
        Method::PUT,
        "/api/v1/tools/recordingRules",
        expected.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["recordingRules"], expected);
    drop(state);

    let restored = ControlState::newWithDataDirectory(directory.path())
        .await
        .expect("从统一配置恢复控制状态");
    let (readStatus, readConfiguration) = requestJson(
        createControlRouter(restored),
        Method::GET,
        "/api/v1/tools/recordingRules",
        json!({}),
    )
    .await;
    assert_eq!(readStatus, StatusCode::OK);
    assert_eq!(readConfiguration, expected);
    assert!(directory.path().join("configuration.json").is_file());
}
