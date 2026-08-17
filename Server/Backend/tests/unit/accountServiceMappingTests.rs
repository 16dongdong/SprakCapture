use axum::{Router, response::Redirect, routing::get};
use tokio::net::TcpListener;

use super::accountMappingClient;

/// 验证账号映射不会在服务端消费一次性登录跳转；Location 和 Set-Cookie 必须留给真实浏览器。
///
/// 运行上下文：本测试用本机临时监听器模拟账号服务，不启动代理或账号数据库。
/// 失败语义：若客户端重新启用自动重定向，响应会变成 200，测试立即暴露桌面免登录回归。
#[tokio::test]
async fn mappingClientPreservesAuthenticationRedirect() {
    let router = Router::new()
        .route(
            "/local-login",
            get(|| async { Redirect::temporary("/account-management") }),
        )
        .route("/account-management", get(|| async { "登录页" }));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定账号映射测试端口");
    let address = listener.local_addr().expect("读取账号映射测试端口");
    let serverTask = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("运行账号映射测试服务");
    });

    let response = accountMappingClient()
        .expect("创建账号映射客户端")
        .get(format!("http://{address}/local-login"))
        .send()
        .await
        .expect("请求一次性登录入口");

    assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .expect("跳转响应必须保留 Location"),
        "/account-management"
    );
    serverTask.abort();
    let _ = serverTask.await;
}
