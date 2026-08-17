#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{net::Ipv4Addr, sync::Arc, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD as base64Standard};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use tower::ServiceExt;

use proxy_backend::controlApi::createControlRouter;

#[path = "support/controlApiSupport.rs"]
mod controlApiSupport;

use controlApiSupport::{
    ControlHttpTestServer, WebSocketEventClient, newControlState, requestJson, requestNoContent,
    requestThroughHttpProxy, startHttpProxyControlService, waitForBreakpointQueue,
    waitForCompletedTransaction,
};

/// 验证单工具读写、snapshot 投影与真实 WebSocket Tools 增量事件共享同一份热更新配置。
#[tokio::test]
async fn toolsApiPublishesSnapshotAndWebSocketEvent() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let server = ControlHttpTestServer::start(router.clone()).await;
    let mut events = WebSocketEventClient::connect(server.address).await;
    let snapshotEvent = events.readEvent().await;
    assert_eq!(snapshotEvent["type"], "snapshot");
    assert_eq!(
        snapshotEvent["snapshot"]["tools"]["rewrite"]["enabled"],
        false
    );

    let dnsConfiguration = json!({
        "enabled": true,
        "rules": [{
            "id": "dnsControlTest",
            "enabled": true,
            "hostPattern": "*.fixture.test",
            "ipAddress": "127.0.0.1"
        }]
    });
    let (dnsUpdateStatus, dnsUpdateResponse) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/dnsSpoofing",
        dnsConfiguration.clone(),
    )
    .await;
    assert_eq!(dnsUpdateStatus, StatusCode::OK);
    assert_eq!(dnsUpdateResponse["dnsSpoofing"], dnsConfiguration);
    let dnsToolsEvent = events.waitForEventType("tools").await;
    assert_eq!(dnsToolsEvent["tools"]["dnsSpoofing"], dnsConfiguration);

    // DNS 规则替换必须原子完成：控制面拒绝非法 IP 后，已发布配置不得被部分覆盖。
    let (invalidDnsStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/dnsSpoofing",
        json!({
            "enabled": true,
            "rules": [{
                "id": "invalidDnsControlTest",
                "enabled": true,
                "hostPattern": "invalid.fixture.test",
                "ipAddress": "999.1.1.1"
            }]
        }),
    )
    .await;
    assert_eq!(invalidDnsStatus, StatusCode::BAD_REQUEST);
    let (dnsReadStatus, dnsReadConfiguration) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/tools/dnsSpoofing",
        json!({}),
    )
    .await;
    assert_eq!(dnsReadStatus, StatusCode::OK);
    assert_eq!(dnsReadConfiguration, dnsConfiguration);

    let packetFilterConfiguration = json!({
        "enabled": true,
        "rules": [{
            "id": "rewriteHandshake",
            "name": "修改握手标记",
            "enabled": true,
            "transport": "tcp",
            "direction": "up",
            "host": "*.fixture.test",
            "port": 443,
            "minimumLength": 4,
            "maximumLength": 1024,
            "pattern": "01 ?? 03 04",
            "replacement": "AA ?? BB CC",
            "action": "modify",
            "replaceAll": true,
            "continueMatching": false
        }]
    });
    let (packetFilterStatus, packetFilterResponse) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/packetFilters",
        packetFilterConfiguration.clone(),
    )
    .await;
    assert_eq!(packetFilterStatus, StatusCode::OK);
    assert_eq!(
        packetFilterResponse["packetFilters"],
        packetFilterConfiguration
    );
    let packetFilterEvent = events.waitForEventType("tools").await;
    assert_eq!(
        packetFilterEvent["tools"]["packetFilters"],
        packetFilterConfiguration
    );

    // WPE 规则允许变长替换，控制面必须完整保留替换字节而不能沿用早期的等长限制。
    let mut variableLengthPacketFilter = packetFilterConfiguration.clone();
    variableLengthPacketFilter["rules"][0]["replacement"] = json!("AA BB");
    let (variableLengthPacketFilterStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/packetFilters",
        variableLengthPacketFilter.clone(),
    )
    .await;
    assert_eq!(variableLengthPacketFilterStatus, StatusCode::OK);
    let variableLengthPacketFilterEvent = events.waitForEventType("tools").await;
    assert_eq!(
        variableLengthPacketFilterEvent["tools"]["packetFilters"],
        variableLengthPacketFilter
    );
    let (packetFilterReadStatus, packetFilterReadConfiguration) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/tools/packetFilters",
        json!({}),
    )
    .await;
    assert_eq!(packetFilterReadStatus, StatusCode::OK);
    assert_eq!(packetFilterReadConfiguration, variableLengthPacketFilter);

    let (initialStatus, initialConfiguration) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/tools/rewrite",
        json!({}),
    )
    .await;
    assert_eq!(initialStatus, StatusCode::OK);
    assert_eq!(initialConfiguration, json!({"enabled": false, "sets": []}));

    let rewriteConfiguration = json!({
        "enabled": true,
        "sets": [{
            "id": "responseHeaders",
            "name": "响应头验证",
            "enabled": true,
            "locations": [],
            "rules": [{
                "id": "addTraceHeader",
                "enabled": true,
                "type": "responseHeader",
                "matchRegex": ".*",
                "replace": "enabled",
                "headerName": "x-control-test",
                "matchValueRegex": null,
                "headerAction": "add",
                "caseSensitive": true,
                "matchAllOccurrences": true
            }]
        }]
    });
    let (updateStatus, updateResponse) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/rewrite",
        rewriteConfiguration.clone(),
    )
    .await;
    assert_eq!(updateStatus, StatusCode::OK);
    assert_eq!(updateResponse["rewrite"], rewriteConfiguration);
    assert_eq!(
        updateResponse["pipelineOrder"],
        json!([
            "dnsSpoofing",
            "blockList",
            "noCaching",
            "blockCookies",
            "mapRemote",
            "mapLocal",
            "rewrite",
            "breakpoints",
            "throttling",
            "mirror",
            "autoSave",
            "packetFilters"
        ])
    );
    assert!(updateResponse.get("recordingRules").is_none());
    // 已删除工具既不能出现在能力快照，也不能继续接受旧控制端点；返回 404 可阻止旧客户端误以为更新成功。
    let (retiredToolStatus, _) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/tools/recordingRules",
        json!({}),
    )
    .await;
    assert_eq!(retiredToolStatus, StatusCode::NOT_FOUND);

    // 工具顺序同时承担控制面能力清单，逐项读取可阻止未实现槽位再次成为虚假 UI 入口。
    for toolId in updateResponse["pipelineOrder"]
        .as_array()
        .expect("pipelineOrder 必须是工具标识数组")
    {
        let toolId = toolId.as_str().expect("工具标识必须是字符串");
        let (toolStatus, _) = requestJson(
            router.clone(),
            Method::GET,
            &format!("/api/v1/tools/{toolId}"),
            json!({}),
        )
        .await;
        assert_eq!(toolStatus, StatusCode::OK, "公开工具必须具有可用读取端点");
    }

    let toolsEvent = events.waitForEventType("tools").await;
    assert_eq!(toolsEvent["tools"]["rewrite"], rewriteConfiguration);
    let (readStatus, readConfiguration) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/tools/rewrite",
        json!({}),
    )
    .await;
    assert_eq!(readStatus, StatusCode::OK);
    assert_eq!(readConfiguration, rewriteConfiguration);
    let (snapshotStatus, snapshot) =
        requestJson(router.clone(), Method::GET, "/api/v1/snapshot", json!({})).await;
    assert_eq!(snapshotStatus, StatusCode::OK);
    assert_eq!(snapshot["tools"]["rewrite"], rewriteConfiguration);

    drop(events);
    state.beginShutdown();
    server.stop().await;
}

/// 验证真实 HTTP 代理命中请求断点后，队列端点与 WebSocket 事件同步展示草稿，continue/abort 均能解除等待任务。
#[tokio::test]
async fn breakpointApiQueuesContinuesAndAbortsRealProxyTraffic() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let breakpointConfiguration = json!({
        "enabled": true,
        "rules": [{
            "id": "requestPause",
            "enabled": true,
            "location": {},
            "onRequest": true,
            "onResponse": false
        }],
        "suspendTimeoutSeconds": 30,
        "maxSuspended": 4,
        "onTimeout": "continue"
    });
    let (breakpointStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/breakpoints",
        breakpointConfiguration,
    )
    .await;
    assert_eq!(breakpointStatus, StatusCode::OK);
    let proxyAddress = startHttpProxyControlService(&router).await;
    let upstream = Arc::new(
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("绑定断点测试上游服务"),
    );
    let upstreamAddress = upstream.local_addr().expect("读取断点测试上游地址");
    let continuationListener = upstream.clone();
    let continuationTask = tokio::spawn(async move {
        let (mut stream, _) = continuationListener
            .accept()
            .await
            .expect("断点 continue 后上游必须收到请求");
        let mut request = [0_u8; 4 * 1024];
        let count = stream
            .read(&mut request)
            .await
            .expect("读取 continue 上游请求");
        assert!(
            String::from_utf8_lossy(&request[..count]).contains("/continue"),
            "continue 必须恢复原始 HTTP 代理请求"
        );
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 9\r\nConnection: close\r\n\r\ncontinued",
            )
            .await
            .expect("写入 continue 上游响应");
    });
    let server = ControlHttpTestServer::start(router.clone()).await;
    let mut events = WebSocketEventClient::connect(server.address).await;
    assert_eq!(events.readEvent().await["type"], "snapshot");

    let continueRequest = tokio::spawn(requestThroughHttpProxy(
        proxyAddress,
        upstreamAddress,
        "continue",
    ));
    let queuedEvent = events.waitForBreakpointEventCount(1).await;
    assert_eq!(queuedEvent["suspended"].as_array().map(Vec::len), Some(1));
    let queued = waitForBreakpointQueue(&router, 1).await;
    let pending = queued
        .first()
        .expect("请求断点必须保留一个暂停草稿")
        .clone();
    assert_eq!(pending["phase"], "request");
    let transactionId = pending["transactionId"]
        .as_str()
        .expect("断点队列必须包含事务标识")
        .to_owned();
    let continueStatus = requestNoContent(
        router.clone(),
        Method::POST,
        &format!("/api/v1/breakpoints/suspended/{transactionId}/continue"),
        pending["draft"].clone(),
    )
    .await;
    assert_eq!(continueStatus, StatusCode::NO_CONTENT);
    let continuedEvent = events.waitForBreakpointEventCount(0).await;
    assert_eq!(continuedEvent["suspended"], json!([]));
    assert!(waitForBreakpointQueue(&router, 0).await.is_empty());
    let continuedResponse = continueRequest
        .await
        .expect("continue 客户端任务不应 panic");
    assert!(
        String::from_utf8_lossy(&continuedResponse).contains("continued"),
        "continue 后客户端必须收到上游响应"
    );
    continuationTask.await.expect("continue 上游任务不应 panic");

    let abortRequest = tokio::spawn(requestThroughHttpProxy(
        proxyAddress,
        upstreamAddress,
        "abort",
    ));
    let queuedAbortEvent = events.waitForBreakpointEventCount(1).await;
    assert_eq!(
        queuedAbortEvent["suspended"].as_array().map(Vec::len),
        Some(1)
    );
    let queuedAbort = waitForBreakpointQueue(&router, 1).await;
    let abortTransactionId = queuedAbort[0]["transactionId"]
        .as_str()
        .expect("待中止断点必须包含事务标识")
        .to_owned();
    let abortStatus = requestNoContent(
        router.clone(),
        Method::POST,
        &format!("/api/v1/breakpoints/suspended/{abortTransactionId}/abort"),
        json!({}),
    )
    .await;
    assert_eq!(abortStatus, StatusCode::NO_CONTENT);
    let abortedEvent = events.waitForBreakpointEventCount(0).await;
    assert_eq!(abortedEvent["suspended"], json!([]));
    let abortedResponse = abortRequest.await.expect("abort 客户端任务不应 panic");
    assert!(
        abortedResponse.starts_with(b"HTTP/1.1 502"),
        "abort 必须向客户端返回确定的 502 响应"
    );
    assert!(
        timeout(Duration::from_millis(150), upstream.accept())
            .await
            .is_err(),
        "请求断点 abort 不得继续建立上游连接"
    );

    let (stopStatus, _) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/service/stop",
        json!({}),
    )
    .await;
    assert_eq!(stopStatus, StatusCode::OK);
    drop(events);
    state.beginShutdown();
    server.stop().await;
}

/// 验证真实上游响应可在控制 API 的 response 阶段继续，防止只覆盖请求断点而遗漏响应分辨通道。
#[tokio::test]
async fn breakpointApiContinuesRealProxyResponse() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (breakpointStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/tools/breakpoints",
        json!({
            "enabled": true,
            "rules": [{
                "id": "responsePause",
                "enabled": true,
                "location": {},
                "onRequest": false,
                "onResponse": true
            }],
            "suspendTimeoutSeconds": 30,
            "maxSuspended": 4,
            "onTimeout": "continue"
        }),
    )
    .await;
    assert_eq!(breakpointStatus, StatusCode::OK);
    let proxyAddress = startHttpProxyControlService(&router).await;
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("绑定响应断点测试上游服务");
    let upstreamAddress = upstream.local_addr().expect("读取响应断点上游地址");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.expect("接受响应断点测试请求");
        let mut request = [0_u8; 4 * 1024];
        let _ = stream
            .read(&mut request)
            .await
            .expect("读取响应断点测试请求");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\nupstream",
            )
            .await
            .expect("写入响应断点上游响应");
    });
    let clientTask = tokio::spawn(requestThroughHttpProxy(
        proxyAddress,
        upstreamAddress,
        "response-continue",
    ));
    let queued = waitForBreakpointQueue(&router, 1).await;
    let pending = queued.first().expect("响应断点必须暴露暂停草稿");
    assert_eq!(pending["phase"], "response");
    let transactionId = pending["transactionId"]
        .as_str()
        .expect("响应断点必须包含事务标识");
    let mut editedDraft = pending["draft"].clone();
    editedDraft["statusCode"] = json!(201);
    editedDraft["headers"] = json!([{
        "name": "content-type",
        "value": "text/plain; charset=utf-8"
    }]);
    editedDraft["bodyBase64"] = json!(base64Standard.encode(b"edited-response"));
    let continueStatus = requestNoContent(
        router.clone(),
        Method::POST,
        &format!("/api/v1/breakpoints/suspended/{transactionId}/continue"),
        editedDraft,
    )
    .await;
    assert_eq!(continueStatus, StatusCode::NO_CONTENT);
    assert!(waitForBreakpointQueue(&router, 0).await.is_empty());
    let response = clientTask.await.expect("响应继续客户端任务不应 panic");
    assert!(
        response.starts_with(b"HTTP/1.1 201"),
        "响应 continue 后客户端必须收到修改后的状态码"
    );
    assert!(
        String::from_utf8_lossy(&response).contains("edited-response"),
        "响应 continue 后客户端必须收到修改后的正文"
    );
    upstreamTask.await.expect("响应断点上游任务不应 panic");

    let (stopStatus, _) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/service/stop",
        json!({}),
    )
    .await;
    assert_eq!(stopStatus, StatusCode::OK);
    state.beginShutdown();
}

/// 验证 HAR 导出端点使用附件响应头，并能把真实 HTTP 代理录制转换为 Chrome DevTools 可读取的 HAR 1.2 JSON。
#[tokio::test]
async fn harExportApiReturnsAttachmentHeadersAndRecordedJson() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let proxyAddress = startHttpProxyControlService(&router).await;
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("绑定 HAR 导出测试上游服务");
    let upstreamAddress = upstream.local_addr().expect("读取 HAR 导出测试上游地址");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.expect("接受 HAR 导出测试请求");
        let mut request = [0_u8; 4 * 1024];
        let _ = stream
            .read(&mut request)
            .await
            .expect("读取 HAR 导出测试请求");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\nhar-body",
            )
            .await
            .expect("写入 HAR 导出测试响应");
    });
    let response = requestThroughHttpProxy(proxyAddress, upstreamAddress, "har-export").await;
    assert!(
        String::from_utf8_lossy(&response).contains("har-body"),
        "HAR 导出测试请求必须经真实代理完成"
    );
    upstreamTask.await.expect("HAR 导出测试上游任务不应 panic");
    let transaction = waitForCompletedTransaction(&router).await;
    let transactionId = transaction["transactionId"]
        .as_str()
        .expect("HAR 导出测试事务必须包含标识");
    let exportRequest = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/recording/export")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "format": "har",
                "includeBodies": true,
                "transactionIds": [transactionId]
            })
            .to_string(),
        ))
        .expect("构建 HAR 导出请求");
    let exportResponse = router
        .clone()
        .oneshot(exportRequest)
        .await
        .expect("执行 HAR 导出请求");
    assert_eq!(exportResponse.status(), StatusCode::OK);
    assert_eq!(
        exportResponse.headers()["content-type"],
        "application/har+json",
        "HAR 导出必须使用专用 MIME 类型"
    );
    assert_eq!(exportResponse.headers()["cache-control"], "no-store");
    assert!(
        exportResponse.headers()["content-disposition"]
            .to_str()
            .expect("HAR 附件头必须是文本")
            .contains("attachment; filename=\"recording.har\""),
        "HAR 导出必须触发附件下载"
    );
    let archiveBytes = to_bytes(exportResponse.into_body(), usize::MAX)
        .await
        .expect("读取 HAR 导出正文");
    let archive: Value = serde_json::from_slice(&archiveBytes).expect("HAR 导出必须是有效 JSON");
    assert_eq!(archive["log"]["version"], "1.2");
    assert_eq!(archive["log"]["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        archive["log"]["entries"][0]["_capture"]["transactionId"],
        transactionId
    );
    assert_eq!(
        archive["log"]["entries"][0]["response"]["content"]["text"],
        "har-body"
    );

    let (stopStatus, _) =
        requestJson(router, Method::POST, "/api/v1/service/stop", json!({})).await;
    assert_eq!(stopStatus, StatusCode::OK);
    state.beginShutdown();
}
