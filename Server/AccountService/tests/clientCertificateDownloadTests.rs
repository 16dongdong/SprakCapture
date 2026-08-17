#![allow(non_snake_case)]

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use account_service::{
    AccountDomainService, AccountPolicy, AccountServerConfig, AccountStore, CreateAccountRequest,
    startAccountService,
};
use axum::{
    Router,
    http::{HeaderValue, header},
    response::Response,
    routing::get,
};
use reqwest::{Client, StatusCode};
use tempfile::TempDir;
use tokio::net::TcpListener;

/// 构造不限制速率、连接和期限的证书下载账号策略。
fn unlimitedPolicy() -> AccountPolicy {
    AccountPolicy {
        maxUploadBytesPerSecond: -1,
        maxDownloadBytesPerSecond: -1,
        maxConnections: -1,
        maxOnlineIps: -1,
        expiresAt: -1,
    }
}

///
/// 验证根证书只向有效 SOCKS5 账号返回，并且由账号服务从回环控制端读取当前版本。
/// 错误密码不会触达控制端；响应禁止缓存且不包含控制地址，确保公开端点只承担同步公开证书的最小职责。
#[tokio::test]
async fn rootCertificateDownloadRequiresActiveSocksCredentials() {
    let temporaryDirectory = TempDir::new().expect("创建隔离目录");
    let databasePath = temporaryDirectory.path().join("accounts.db");
    let service =
        AccountDomainService::new(AccountStore::open(&databasePath).expect("创建账号数据库"));
    service
        .bootstrapManagement("Admin", "Admin123")
        .expect("初始化管理身份");
    service
        .createAccount(&CreateAccountRequest {
            username: "certificate-user".to_owned(),
            password: Some("certificate-password".to_owned()),
            policy: unlimitedPolicy(),
            remark: None,
        })
        .await
        .expect("创建证书下载账号");
    let mut expiredPolicy = unlimitedPolicy();
    expiredPolicy.expiresAt = 1;
    service
        .createAccount(&CreateAccountRequest {
            username: "expired-user".to_owned(),
            password: Some("expired-password".to_owned()),
            policy: expiredPolicy,
            remark: None,
        })
        .await
        .expect("创建过期账号");
    drop(service);

    let certificateBytes = Arc::new(vec![0x30, 0x03, 0x02, 0x01, 0x01]);
    let controlRequests = Arc::new(AtomicUsize::new(0));
    let controlListener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定控制夹具");
    let controlAddress = controlListener.local_addr().expect("读取控制地址");
    let responseBytes = certificateBytes.clone();
    let requestCounter = controlRequests.clone();
    let controlTask = tokio::spawn(async move {
        axum::serve(
            controlListener,
            Router::new().route(
                "/api/v1/ssl/ca/export",
                get(move || {
                    let body = responseBytes.clone();
                    let counter = requestCounter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let mut response =
                            Response::new(axum::body::Body::from(body.as_ref().clone()));
                        response.headers_mut().insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("application/pkix-cert"),
                        );
                        response
                    }
                }),
            ),
        )
        .await
    });
    let running = startAccountService(AccountServerConfig {
        databasePath,
        publicAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalToken: "certificate-internal-token-at-least-32-bytes".to_owned(),
        controlBaseUrl: format!("http://{controlAddress}"),
        webAssetsDirectory: None,
    })
    .await
    .expect("启动账号服务");
    let url = format!("http://{}/api/v1/client/ca.cer", running.publicAddress);
    let client = Client::new();

    let rejected = client
        .get(&url)
        .basic_auth("certificate-user", Some("wrong-password"))
        .send()
        .await
        .expect("发送错误凭据请求");
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(controlRequests.load(Ordering::SeqCst), 0);
    let expired = client
        .get(&url)
        .basic_auth("expired-user", Some("expired-password"))
        .send()
        .await
        .expect("发送过期账号请求");
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(controlRequests.load(Ordering::SeqCst), 0);

    let accepted = client
        .get(&url)
        .basic_auth("certificate-user", Some("certificate-password"))
        .send()
        .await
        .expect("下载根证书");
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(
        accepted.headers()[header::CONTENT_TYPE],
        "application/pkix-cert"
    );
    assert_eq!(
        accepted.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert_eq!(
        accepted.bytes().await.expect("读取根证书").as_ref(),
        certificateBytes.as_slice()
    );
    assert_eq!(controlRequests.load(Ordering::SeqCst), 1);

    running.stop().await.expect("停止账号服务");
    controlTask.abort();
}
