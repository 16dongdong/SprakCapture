#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt;

use proxy_backend::controlApi::createControlRouter;

#[path = "support/controlApiSupport.rs"]
mod controlApiSupport;

use controlApiSupport::newControlState;

/// 新数据目录必须返回空任务和空产物，且结构保持严格 camelCase 控制协议。
#[tokio::test]
async fn emptyClientPackageSnapshotIsAvailable() {
    let state = newControlState().await;
    let response = createControlRouter((*state).clone())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/clientPackages")
                .header("origin", "http://127.0.0.1:5173")
                .body(Body::empty())
                .expect("构造客户端产物读取请求"),
        )
        .await
        .expect("执行客户端产物读取请求");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 4096)
        .await
        .expect("读取客户端产物快照");
    let document: Value = serde_json::from_slice(&body).expect("解析客户端产物快照");
    assert_eq!(document["activeJob"], Value::Null);
    assert_eq!(document["packages"], Value::Array(Vec::new()));
}

/// 历史产物不再提供二次下载入口；凭据已内置 APK，任何旧 UUID 路径都必须保持不可达。
#[tokio::test]
async fn historicalClientPackageDownloadRouteIsRemoved() {
    let state = newControlState().await;
    let response = createControlRouter((*state).clone())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/clientPackages/not-a-uuid/download?locale=zh-Hans")
                .header("origin", "http://127.0.0.1:5173")
                .body(Body::empty())
                .expect("构造无效客户端产物下载请求"),
        )
        .await
        .expect("执行无效客户端产物下载请求");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// 旧无凭据生成入口必须返回 405，确保所有 APK 都经过 SOCKS5 账号权威校验。
#[tokio::test]
async fn unauthenticatedClientPackageGenerationRouteIsRemoved() {
    let state = newControlState().await;
    let response = createControlRouter((*state).clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/clientPackages")
                .header("origin", "http://127.0.0.1:5173")
                .body(Body::empty())
                .expect("构造旧客户端生成请求"),
        )
        .await
        .expect("执行旧客户端生成请求");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

/// 下载请求在 JSON 解析前实施严格正文上限，避免公开页面用超大凭据长期占用控制进程内存。
#[tokio::test]
async fn clientPackageDownloadRequestBodyIsBounded() {
    let state = newControlState().await;
    let response = createControlRouter((*state).clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/clientPackages/download")
                .header("content-type", "application/json")
                .header("origin", "http://127.0.0.1:5173")
                // 请求上限需要容纳 1 MiB 图标的 Base64；超过完整 2 MiB 协议预算才应由路由层返回 413。
                .body(Body::from("x".repeat(2 * 1024 * 1024 + 1)))
                .expect("构造超限客户端生成请求"),
        )
        .await
        .expect("执行超限客户端生成请求");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/// 直连控制端口也必须执行账号服务权威校验，不能依赖外层远程页面替用户完成认证。
#[tokio::test]
async fn directClientPackageDownloadRejectsInvalidSocksCredentials() {
    let state = newControlState().await;
    let response = createControlRouter((*state).clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/clientPackages/download?locale=zh-Hans")
                .header("content-type", "application/json")
                .header("origin", "http://127.0.0.1:5173")
                .body(Body::from(
                    r#"{"username":"missing-account","password":"wrong-password"}"#,
                ))
                .expect("构造无效 SOCKS5 凭据下载请求"),
        )
        .await
        .expect("执行无效 SOCKS5 凭据下载请求");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), 4096)
        .await
        .expect("读取客户端认证错误响应");
    let document: Value = serde_json::from_slice(&body).expect("解析客户端认证错误响应");
    assert_eq!(document["code"], "clientPackageAuthenticationFailed");
}

/// 下载入口必须在调用账号服务前拒绝空密码；即使账号采用任意非空密码模式，生成的 APK 仍需内置可复用的完整凭据。
#[tokio::test]
async fn directClientPackageDownloadRejectsEmptyPassword() {
    let state = newControlState().await;
    let response = createControlRouter((*state).clone())
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/clientPackages/download?locale=zh-Hans")
                .header("content-type", "application/json")
                .header("origin", "http://127.0.0.1:5173")
                .body(Body::from(r#"{"username":"fixed-user","password":""}"#))
                .expect("构造空密码客户端生成请求"),
        )
        .await
        .expect("执行空密码客户端生成请求");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), 4096)
        .await
        .expect("读取空密码认证错误响应");
    let document: Value = serde_json::from_slice(&body).expect("解析空密码认证错误响应");
    assert_eq!(document["code"], "clientPackageAuthenticationFailed");
}
