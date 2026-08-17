#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use proxy_backend::controlApi::createControlRouter;

#[path = "support/controlApiSupport.rs"]
mod controlApiSupport;

use controlApiSupport::{newControlState, requestJson};

/// 构造一个不含业务正文的界面心跳；测试只覆盖页面、焦点和稳定选择标识。
fn contextUpdate(instanceId: &str, sequence: u64, focused: bool) -> Value {
    json!({
        "instanceId": instanceId,
        "sequence": sequence,
        "windowKind": "main",
        "page": "connections",
        "section": null,
        "view": "overview",
        "selection": {
            "kind": "transaction",
            "ids": ["transaction-alpha"],
            "side": null,
            "sequence": null
        },
        "focused": focused,
        "visible": true
    })
}

/// 读取界面上下文 GET 响应；空正文请求不能复用 JSON 写入辅助函数。
async fn getContext(router: axum::Router) -> (StatusCode, Value) {
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/ui/context")
                .body(Body::empty())
                .expect("构造界面上下文读取请求"),
        )
        .await
        .expect("读取界面上下文");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("读取界面上下文响应");
    let value = serde_json::from_slice(&bytes).expect("解析界面上下文响应");
    (status, value)
}

/// 验证多个窗口可同时存在，聚合主上下文优先选择当前聚焦窗口。
#[tokio::test]
async fn uiContextSelectsFocusedWindowAsPrimary() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let firstId = "8f5606c4-f441-4c20-b120-e365b8451b64";
    let secondId = "7f3f9843-7337-4e7a-a47f-968e13dfc0e3";
    assert_eq!(
        requestJson(
            router.clone(),
            Method::PUT,
            "/api/v1/ui/context",
            contextUpdate(firstId, 1, false),
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, updated) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/ui/context",
        contextUpdate(secondId, 1, true),
    )
    .await;
    assert_eq!(updated["primary"]["instanceId"], secondId);
    assert_eq!(updated["contexts"].as_array().map(Vec::len), Some(2));

    let (status, readBack) = getContext(router).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(readBack["primary"]["instanceId"], secondId);
}

/// 验证慢请求不能覆盖同一窗口的新 sequence，避免快速切换事务后 MCP 观察到回退选择。
#[tokio::test]
async fn uiContextIgnoresOutOfOrderUpdates() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let instanceId = "696da94c-ed44-4e40-a0ad-e56d0047e394";
    let mut current = contextUpdate(instanceId, 2, true);
    current["view"] = json!("contents");
    requestJson(router.clone(), Method::PUT, "/api/v1/ui/context", current).await;
    let (_, snapshot) = requestJson(
        router,
        Method::PUT,
        "/api/v1/ui/context",
        contextUpdate(instanceId, 1, true),
    )
    .await;
    assert_eq!(snapshot["primary"]["sequence"], 2);
    assert_eq!(snapshot["primary"]["view"], "contents");
}

/// 验证无效包选择会整体拒绝，防止任意无界字段进入 MCP 可见状态。
#[tokio::test]
async fn uiContextRejectsInvalidPacketSelection() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let mut update = contextUpdate("f078a808-e9af-414c-b80e-6fcb0e386c42", 1, true);
    update["selection"] = json!({
        "kind": "streamPacket",
        "ids": ["transaction-alpha"],
        "side": null,
        "sequence": null
    });
    let (status, response) = requestJson(router, Method::PUT, "/api/v1/ui/context", update).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(response["code"], "invalidConfigurationRequest");
}
