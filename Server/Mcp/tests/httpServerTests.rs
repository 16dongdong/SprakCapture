#![allow(non_snake_case)]

use std::net::SocketAddr;

use capture_mcp::httpServer::HttpMcpRuntime;

/// 验证集成 MCP 能绑定临时回环端口、公开标准路径，并在停止返回前释放监听器。
#[tokio::test]
async fn integratedHttpServerReleasesPortAfterStop() {
    let runtime = HttpMcpRuntime::start(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        "http://127.0.0.1:17890".to_owned(),
        Some("zh-Hans".to_owned()),
    )
    .await
    .expect("临时回环 MCP 应启动成功");
    let endpoint = runtime.endpoint();
    let address = endpoint
        .strip_prefix("http://")
        .and_then(|value| value.strip_suffix("/mcp"))
        .expect("端点应使用固定 HTTP /mcp 格式")
        .parse::<SocketAddr>()
        .expect("端点应包含真实套接字地址");
    let response = reqwest::Client::new()
        .get(&endpoint)
        .send()
        .await
        .expect("MCP 路由应可建立 HTTP 连接");
    assert!(response.status().is_client_error());
    runtime.stop().await.expect("MCP 应完成协作式停止");
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("停止返回后端口必须可立即重新绑定");
    drop(listener);
}
