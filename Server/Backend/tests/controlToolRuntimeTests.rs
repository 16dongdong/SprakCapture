#![allow(non_snake_case, non_upper_case_globals)]

use std::{net::Ipv4Addr, time::Duration};

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

use proxy_backend::controlApi::createControlRouter;

#[path = "support/controlApiSupport.rs"]
mod controlApiSupport;

use controlApiSupport::{
    newControlState, requestJson, requestNoContent, requestThroughHttpProxy,
    startHttpProxyControlService, waitForBreakpointQueue,
};

/// 启动只处理一次请求的真实 HTTP 上游，并返回其地址与任务。
///
/// `expectedPath` 用于确认 Map Remote 或 Rewrite 最终交付的请求路径，`body` 是回送给代理的数据。
/// 接受、读取或写入失败会直接使测试任务失败，不以宽松响应掩盖工具链路缺陷。
async fn startSingleResponseUpstream(
    expectedPath: &'static str,
    body: &'static str,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("工具运行时上游必须绑定成功");
    let address = listener.local_addr().expect("工具运行时上游地址必须可读");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("工具运行时上游必须接受连接");
        let mut request = vec![0_u8; 16 * 1024];
        let readBytes = stream
            .read(&mut request)
            .await
            .expect("工具运行时上游必须读取请求");
        let requestText = String::from_utf8_lossy(&request[..readBytes]);
        assert!(
            requestText.starts_with(&format!("GET {expectedPath} HTTP/1.1\r\n")),
            "上游收到的请求路径与工具规则结果不一致：{requestText}"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("工具运行时上游必须写入响应");
    });
    (address, task)
}

/// 将响应字节转换为可断言文本；代理响应头与本测试正文均限定为 ASCII/UTF-8。
///
/// 非法 UTF-8 表示数据面交付了损坏响应，函数会立即终止测试并保留原始错误位置。
fn responseText(response: &[u8]) -> &str {
    std::str::from_utf8(response).expect("工具运行时代理响应必须是 UTF-8")
}

/// 通过真实代理发送指定 absolute-form URL，避免 Map Remote 用例在工具生效前依赖来源域名 DNS。
///
/// `absoluteUrl` 与 `hostHeader` 分别进入请求行和 Host 字段；连接或传输失败会直接终止测试，返回值
/// 包含完整响应头和正文，供调用方确认改写后的线级结果。
async fn requestAbsoluteThroughProxy(
    proxyAddress: std::net::SocketAddr,
    absoluteUrl: &str,
    hostHeader: &str,
) -> Vec<u8> {
    let mut client = TcpStream::connect(proxyAddress)
        .await
        .expect("工具运行时客户端必须连接代理");
    client
        .write_all(
            format!(
                "GET {absoluteUrl} HTTP/1.1\r\nHost: {hostHeader}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("工具运行时客户端必须写入请求");
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("工具运行时客户端必须读完响应");
    response
}

/// 验证运行中的融合 HTTP 监听器能够无重启热应用 Map Remote、Rewrite、Map Local 与 Breakpoint。
///
/// 测试刻意先启动服务再逐项写入控制配置，覆盖用户从界面添加规则后的真实生命周期；任一工具未
/// 命中、未短路、未改写或未进入暂停队列都会在对应网络结果上失败。
#[tokio::test]
async fn hotUpdatesEveryInteractiveHttpToolWithoutRestartingService() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let proxyAddress = startHttpProxyControlService(&router).await;

    let (remoteAddress, remoteTask) = startSingleResponseUpstream("/mapped/item", "remote").await;
    let (mapRemoteStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/mapRemote",
        json!({
            "enabled": true,
            "rules": [{
                "id": "runtime-remote",
                "enabled": true,
                "from": {"protocol": "http", "host": "source.invalid", "port": "80", "path": "/source/*", "query": null},
                "to": {"protocol": "http", "host": "127.0.0.1", "port": remoteAddress.port().to_string(), "path": "/mapped/*"}
            }]
        }),
    )
    .await;
    assert_eq!(mapRemoteStatus, StatusCode::OK);
    let remoteResponse = requestAbsoluteThroughProxy(
        proxyAddress,
        "http://source.invalid/source/item",
        "source.invalid",
    )
    .await;
    assert!(responseText(&remoteResponse).ends_with("remote"));
    remoteTask.await.expect("Map Remote 上游任务必须完成");

    requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/mapRemote",
        json!({"enabled": false, "rules": []}),
    )
    .await;
    let (rewriteAddress, rewriteTask) = startSingleResponseUpstream("/rewrite", "old-body").await;
    let (rewriteStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/rewrite",
        json!({
            "enabled": true,
            "sets": [{
                "id": "runtime-rewrite",
                "name": "运行时响应改写",
                "enabled": true,
                "locations": [],
                "rules": [{
                    "id": "replace-body",
                    "enabled": true,
                    "type": "responseBody",
                    "matchRegex": "old",
                    "replace": "new",
                    "headerName": null,
                    "matchValueRegex": null,
                    "headerAction": null,
                    "caseSensitive": true,
                    "matchAllOccurrences": true
                }]
            }]
        }),
    )
    .await;
    assert_eq!(rewriteStatus, StatusCode::OK);
    let rewriteResponse = requestThroughHttpProxy(proxyAddress, rewriteAddress, "rewrite").await;
    assert!(responseText(&rewriteResponse).ends_with("new-body"));
    rewriteTask.await.expect("Rewrite 上游任务必须完成");

    requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/rewrite",
        json!({"enabled": false, "sets": []}),
    )
    .await;
    let mappingDirectory = state.dataDirectory().join("mappings");
    tokio::fs::write(mappingDirectory.join("runtime-local.txt"), b"local")
        .await
        .expect("Map Local 受管夹具必须写入");
    let (mapLocalStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/mapLocal",
        json!({
            "enabled": true,
            "rules": [{
                "id": "runtime-local",
                "enabled": true,
                "location": {"protocol": "http", "host": "local.invalid", "port": "80", "path": "/fixture", "query": null},
                "localPath": "runtime-local.txt",
                "isDirectory": false,
                "statusCode": 200,
                "responseHeaders": [],
                "contentTypeOverride": "text/plain"
            }]
        }),
    )
    .await;
    assert_eq!(mapLocalStatus, StatusCode::OK);
    let localResponse = requestAbsoluteThroughProxy(
        proxyAddress,
        "http://local.invalid/fixture",
        "local.invalid",
    )
    .await;
    assert!(responseText(&localResponse).ends_with("local"));

    requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/mapLocal",
        json!({"enabled": false, "rules": []}),
    )
    .await;
    let (breakpointAddress, breakpointTask) =
        startSingleResponseUpstream("/breakpoint", "continued").await;
    let (breakpointStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/breakpoints",
        json!({
            "enabled": true,
            "rules": [{
                "id": "runtime-breakpoint",
                "enabled": true,
                "location": {"protocol": "http", "host": "127.0.0.1", "port": breakpointAddress.port().to_string(), "path": "/breakpoint", "query": null},
                "onRequest": true,
                "onResponse": false
            }],
            "suspendTimeoutSeconds": 5,
            "maxSuspended": 4,
            "onTimeout": "abort"
        }),
    )
    .await;
    assert_eq!(breakpointStatus, StatusCode::OK);
    let breakpointRequest = tokio::spawn(requestThroughHttpProxy(
        proxyAddress,
        breakpointAddress,
        "breakpoint",
    ));
    let suspended = waitForBreakpointQueue(&router, 1).await;
    let transactionId = suspended[0]["transactionId"]
        .as_str()
        .expect("断点队列必须携带事务标识");
    let draft: Value = suspended[0]["draft"].clone();
    assert_eq!(
        requestNoContent(
            router.clone(),
            Method::POST,
            &format!("/api/v1/breakpoints/suspended/{transactionId}/continue"),
            draft,
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let breakpointResponse = timeout(Duration::from_secs(2), breakpointRequest)
        .await
        .expect("断点继续后的请求必须完成")
        .expect("断点请求任务必须成功");
    assert!(responseText(&breakpointResponse).ends_with("continued"));
    breakpointTask.await.expect("Breakpoint 上游任务必须完成");

    assert_eq!(
        requestJson(router, Method::POST, "/api/v1/service/stop", json!({}),)
            .await
            .0,
        StatusCode::OK
    );
}
