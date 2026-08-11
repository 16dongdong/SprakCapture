#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    net::{Ipv4Addr, TcpListener as StandardTcpListener},
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD as base64Standard};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};
use tower::ServiceExt;

use proxy_backend::controlApi::{EventMessage, createControlRouter};

#[path = "support/controlApiSupport.rs"]
mod controlApiSupport;

use controlApiSupport::{
    configurationJson, findAvailablePort, newControlState, parseJsonResponse,
    requestConfigurationBody, requestInvalidConfiguration, requestJson, requestThroughHttpProxy,
    waitForCompletedTransaction, waitForTransactionCount,
};

/// 验证 SSE 端点首帧直接携带权威快照；工作台建立连接后无需先轮询 REST 才能渲染。
#[tokio::test]
async fn serverSentEventsStartWithAuthoritativeSnapshot() {
    let state = newControlState().await;
    let response = createControlRouter((*state).clone())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/events/sse?locale=zh-Hans")
                .header("origin", "http://127.0.0.1:5173")
                .body(Body::empty())
                .expect("构造 SSE 请求"),
        )
        .await
        .expect("执行 SSE 请求");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"))
    );

    let mut body = response.into_body().into_data_stream();
    let firstChunk = timeout(Duration::from_secs(1), body.next())
        .await
        .expect("SSE 首帧必须在一秒内到达")
        .expect("SSE 响应必须包含首帧")
        .expect("读取 SSE 首帧");
    let firstFrame = std::str::from_utf8(&firstChunk).expect("SSE 必须使用 UTF-8");
    assert!(firstFrame.starts_with("event: control\n"));
    let payload = firstFrame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("SSE 首帧必须包含 data 字段");
    let message: Value = serde_json::from_str(payload).expect("解析 SSE 快照 JSON");
    assert_eq!(message["type"], "snapshot");
    assert_eq!(message["snapshot"]["serviceState"], "stopped");
}

/// 验证关闭标记早于 SSE 流首次轮询时仍能在首快照后立即收口；该竞态不得等待下一次 watch 变更或强制排空超时。
#[tokio::test]
async fn serverSentEventsCloseWhenShutdownWasAlreadyPublished() {
    let state = newControlState().await;
    state.beginShutdown();
    let response = createControlRouter((*state).clone())
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/events/sse")
                .header("origin", "http://127.0.0.1:5173")
                .body(Body::empty())
                .expect("构造已关闭控制面的 SSE 请求"),
        )
        .await
        .expect("执行已关闭控制面的 SSE 请求");
    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body().into_data_stream();
    timeout(Duration::from_secs(1), body.next())
        .await
        .expect("SSE 首快照必须及时到达")
        .expect("SSE 必须保留首快照")
        .expect("读取 SSE 首快照");
    let trailingChunk = timeout(Duration::from_millis(200), body.next())
        .await
        .expect("已关闭控制面的 SSE 必须立即结束");
    assert!(trailingChunk.is_none(), "首快照后不得继续保持 SSE 长连接");
}

/// 验证控制面关闭后高级重复作业的 watch 投影立即退出；作业运行时不得重新唤醒并自持整个 ControlState。
#[tokio::test]
async fn advancedRepeatForwarderStopsAfterControlShutdown() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let mut events = state.subscribeEvents();
    state.beginShutdown();
    let (status, _) = requestJson(
        router,
        Method::POST,
        "/api/v1/loadTests",
        json!({
            "name": "关闭生命周期验证",
            "base": {
                "method": "GET",
                "url": "http://127.0.0.1:9/",
                "headers": [],
                "bodyBase64": "",
                "viaProxy": false
            },
            "concurrency": 1,
            "totalIterations": 1,
            "intervalMilliseconds": 0,
            "recordEach": false,
            "stopOnError": true,
            "confirmed": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    tokio::time::sleep(Duration::from_millis(120)).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, EventMessage::AdvancedRepeats { .. }),
            "控制面关闭后不得继续发布高级重复作业事件"
        );
    }
}

/// 验证插件控制路由注册到统一 API，并对空宿主和未知标识返回稳定的集合与机器错误语义。
#[tokio::test]
async fn pluginControlRoutesExposeEmptyListAndStableNotFound() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let (listStatus, plugins) =
        requestJson(router.clone(), Method::GET, "/api/v1/plugins", json!({})).await;
    assert_eq!(listStatus, StatusCode::OK);
    assert_eq!(plugins, json!([]));

    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/plugins/not-installed")
        .body(Body::empty())
        .expect("构建插件详情请求");
    let (status, error) =
        parseJsonResponse(router.oneshot(request).await.expect("执行插件详情请求")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "pluginNotFound");
}

/// 验证完整扩展平台的授权、顺序、预算和秘密引用通过控制 API 原子保存并可幂等删除。
#[tokio::test]
async fn extensionPlatformConfigurationAndDiagnosticsExposeStableContracts() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let configuration = json!({
        "enabled": false,
        "activeVersion": "2.1.0",
        "moduleOrder": ["streamTransformer"],
        "subscriptionOverrides": {},
        "failurePolicy": "failClosed",
        "limits": null,
        "configurationSchemaVersion": "1",
        "configuration": { "framing": "lengthPrefix" },
        "secretReferences": { "key": "secret://example.protocol/key" },
        "automaticRestart": true
    });
    let (updateStatus, updated) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/extensions/configuration/example.protocol",
        configuration.clone(),
    )
    .await;
    assert_eq!(updateStatus, StatusCode::OK);
    assert_eq!(updated["plugins"]["example.protocol"], configuration);

    let (getStatus, loaded) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/extensions/configuration",
        json!({}),
    )
    .await;
    assert_eq!(getStatus, StatusCode::OK);
    assert_eq!(loaded, updated);

    let (runtimeStatus, runtime) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/extensions/runtime",
        json!({}),
    )
    .await;
    assert_eq!(runtimeStatus, StatusCode::OK);
    assert_eq!(runtime, json!([]));

    let (removeStatus, removed) = requestJson(
        router.clone(),
        Method::DELETE,
        "/api/v1/extensions/configuration/example.protocol",
        json!({}),
    )
    .await;
    assert_eq!(removeStatus, StatusCode::OK);
    assert_eq!(removed["plugins"], json!({}));
}

/// 验证实例标识在单个控制进程内稳定，并在新控制进程中重新生成且贯穿完整事件。
#[tokio::test]
async fn serverInstanceIdentityIsStableAndProcessScoped() {
    let firstState = newControlState().await;
    let firstSnapshot = firstState.snapshot().await;
    let repeatedSnapshot = firstState.snapshot().await;
    assert_eq!(
        firstSnapshot.serverInstanceId,
        repeatedSnapshot.serverInstanceId
    );

    let secondState = newControlState().await;
    let secondSnapshot = secondState.snapshot().await;
    assert_ne!(
        firstSnapshot.serverInstanceId,
        secondSnapshot.serverInstanceId
    );

    match firstState.snapshotEvent().await {
        EventMessage::Snapshot {
            serverInstanceId,
            snapshot,
        } => {
            assert_eq!(serverInstanceId, firstSnapshot.serverInstanceId);
            assert_eq!(snapshot.serverInstanceId, firstSnapshot.serverInstanceId);
        }
        _ => panic!("完整事件必须使用 snapshot 判别类型"),
    }
}

/// 走通 SSL 控制面的读取、更新、事件、导出和根证书再生，确保公开契约只暴露证书元数据。
#[tokio::test]
async fn sslLifecycleExportsPublicCertificateWithoutPrivateMaterial() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let mut events = state.subscribeEvents();

    let (getStatus, initialState) =
        requestJson(router.clone(), Method::GET, "/api/v1/ssl", json!({})).await;
    assert_eq!(getStatus, StatusCode::OK);
    assert_eq!(initialState["enabled"], false);
    assert_eq!(initialState["ca"]["installed"], true);
    assert!(
        !initialState
            .to_string()
            .to_ascii_lowercase()
            .contains("private"),
        "SSL 公开状态不得包含私钥字段"
    );
    let originalFingerprint = initialState["ca"]["fingerprintSha256"]
        .as_str()
        .expect("根证书指纹必须存在")
        .to_owned();

    let update = json!({
        "enabled": true,
        "includeLocations": [{
            "protocol": "https",
            "host": "*.example.com",
            "port": "",
            "path": "",
            "query": null
        }],
        "excludeLocations": [{
            "protocol": "https",
            "host": "private.example.com",
            "port": "",
            "path": "",
            "query": null
        }],
        "maxCachedCertificates": 128,
        "useClientSni": true
    });
    let (updateStatus, updatedState) =
        requestJson(router.clone(), Method::PUT, "/api/v1/ssl", update).await;
    assert_eq!(updateStatus, StatusCode::OK);
    assert_eq!(updatedState["enabled"], true);
    assert_eq!(updatedState["includeLocations"][0]["host"], "*.example.com");
    match timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("SSL 更新事件必须及时发布")
        .expect("SSL 更新事件通道不得关闭")
    {
        EventMessage::Ssl { ssl, .. } => {
            assert!(ssl.enabled);
            assert_eq!(ssl.maxCachedCertificates, 128);
        }
        event => panic!("SSL 更新必须发布 ssl 事件，实际为 {event:?}"),
    }

    for (format, expectedType, expectedName) in [
        ("pem", "application/x-pem-file", "root.pem"),
        ("cer", "application/pkix-cert", "root.cer"),
    ] {
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("/api/v1/ssl/ca/export?format={format}"))
            .body(Body::empty())
            .expect("构建根证书导出请求");
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("执行根证书导出请求");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            expectedType,
            "根证书导出必须保留准确媒体类型"
        );
        assert!(
            response.headers()["content-disposition"]
                .to_str()
                .expect("附件文件名必须为文本")
                .contains(expectedName)
        );
        let certificate = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("读取导出的根证书");
        assert!(!certificate.is_empty());
    }

    let (regenerateStatus, regeneratedState) =
        requestJson(router, Method::POST, "/api/v1/ssl/ca/generate", json!({})).await;
    assert_eq!(regenerateStatus, StatusCode::OK);
    assert_ne!(
        regeneratedState["ca"]["fingerprintSha256"],
        originalFingerprint
    );
    assert_eq!(regeneratedState["cachedLeafCount"], 0);
}

/// 验证统一 start 真实启动 HTTP 监听，暂停只影响录制，恢复后继续写入，正文与 clear 语义完整。
#[tokio::test]
async fn httpRecordingLifecycleExposesMetadataBodyAndClear() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("绑定上游测试服务");
    let upstreamAddress = upstream.local_addr().expect("读取上游地址");
    let upstreamTask = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = upstream.accept().await.expect("接受上游连接");
            let mut request = vec![0_u8; 4_096];
            let _readBytes = stream.read(&mut request).await.expect("读取上游请求");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                )
                .await
                .expect("写入上游响应");
        }
    });

    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let socksPort = findAvailablePort();
    let httpPort = socksPort;
    let mut configuration = configurationJson(socksPort);
    configuration["httpProxy"] = json!({
        "enabled": true,
        "listenHost": "127.0.0.1",
        "listenPort": httpPort,
        "maxConnections": 16,
        "maxHeaderBytes": 16384,
        "maxCaptureBodyBytes": 65536,
        "connectTimeoutMilliseconds": 2000,
        "requestTimeoutMilliseconds": 5000,
        "headerReadTimeoutMilliseconds": 2000,
        "shutdownTimeoutMilliseconds": 2000
    });
    let (putStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configuration,
    )
    .await;
    assert_eq!(putStatus, StatusCode::OK);
    let (startStatus, startSnapshot) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/service/start",
        json!({}),
    )
    .await;
    assert_eq!(startStatus, StatusCode::OK);
    assert_eq!(startSnapshot["listeners"]["httpProxy"]["state"], "running");
    let proxyAddress: std::net::SocketAddr =
        startSnapshot["listeners"]["httpProxy"]["boundEndpoint"]
            .as_str()
            .expect("HTTP 监听地址")
            .parse()
            .expect("解析 HTTP 监听地址");
    let (emptyPageStatus, emptyPage) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/transactions?offset=0&limit=1",
        json!({}),
    )
    .await;
    assert_eq!(emptyPageStatus, StatusCode::OK);
    let appendStableToken = emptyPage["collectionToken"]
        .as_str()
        .expect("空事务页必须提供分页代际")
        .to_owned();

    let response = requestThroughHttpProxy(proxyAddress, upstreamAddress, "recorded").await;
    assert!(
        String::from_utf8_lossy(&response).contains("hello"),
        "代理响应应包含上游正文"
    );
    let transaction = waitForCompletedTransaction(&router).await;
    let transactionId = transaction["transactionId"].as_str().expect("事务 ID");
    let (appendedPageStatus, appendedPage) = requestJson(
        router.clone(),
        Method::GET,
        &format!("/api/v1/transactions?offset=0&limit=1&collectionToken={appendStableToken}"),
        json!({}),
    )
    .await;
    assert_eq!(appendedPageStatus, StatusCode::OK);
    assert_eq!(
        appendedPage["collectionToken"], appendStableToken,
        "尾部追加不能让正在读取的历史分页代际失效"
    );
    assert_eq!(appendedPage["items"][0]["transactionId"], transactionId);
    let detailResponse = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/transactions/{transactionId}"))
                .body(Body::empty())
                .expect("构建详情请求"),
        )
        .await
        .expect("读取事务详情");
    assert_eq!(
        detailResponse.headers()["cache-control"],
        "no-store",
        "详情不得进入浏览器缓存"
    );
    let (_, detail) = parseJsonResponse(detailResponse).await;
    assert_eq!(detail["transaction"]["method"], "GET");
    assert!(detail.get("revision").is_some());

    let bodyResponse = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/transactions/{transactionId}/response/body"
                ))
                .body(Body::empty())
                .expect("构建正文请求"),
        )
        .await
        .expect("读取事务正文");
    assert_eq!(bodyResponse.headers()["cache-control"], "no-store");
    let (_, body) = parseJsonResponse(bodyResponse).await;
    assert_eq!(
        base64Standard
            .decode(body["base64"].as_str().expect("base64 正文"))
            .expect("解码正文"),
        b"hello"
    );
    let mediaPreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/transactions/{transactionId}/response/media-preview"
                ))
                .body(Body::empty())
                .expect("构建媒体预览请求"),
        )
        .await
        .expect("读取媒体预览响应");
    assert_eq!(mediaPreview.status(), StatusCode::OK);
    assert_eq!(mediaPreview.headers()["x-media-preview-status"], "complete");
    assert_eq!(
        mediaPreview.headers()["x-media-preview-captured-bytes"],
        "5"
    );
    assert_eq!(mediaPreview.headers()["x-media-preview-total-bytes"], "5");
    assert_eq!(mediaPreview.headers()["x-media-preview-segment-count"], "1");
    let mediaPreviewBytes = to_bytes(mediaPreview.into_body(), usize::MAX)
        .await
        .expect("读取二进制媒体预览正文");
    assert_eq!(
        mediaPreviewBytes,
        b"hello".as_slice(),
        "媒体预览必须直接返回二进制正文，不得生成 JSON Base64 副本"
    );

    let (pauseStatus, paused) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/recording",
        json!({"state": "paused"}),
    )
    .await;
    assert_eq!(pauseStatus, StatusCode::OK);
    assert_eq!(paused["recording"]["state"], "paused");
    let pausedResponse = requestThroughHttpProxy(proxyAddress, upstreamAddress, "paused").await;
    assert!(String::from_utf8_lossy(&pausedResponse).contains("hello"));
    assert_eq!(
        waitForTransactionCount(&router, 1).await["total"],
        1,
        "暂停时数据面继续转发但不得新增录制事务"
    );

    let (resumeStatus, resumed) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/recording",
        json!({"state": "recording"}),
    )
    .await;
    assert_eq!(resumeStatus, StatusCode::OK);
    assert_eq!(resumed["recording"]["state"], "recording");
    let resumedResponse = requestThroughHttpProxy(proxyAddress, upstreamAddress, "resumed").await;
    assert!(String::from_utf8_lossy(&resumedResponse).contains("hello"));
    assert_eq!(waitForTransactionCount(&router, 2).await["total"], 2);
    upstreamTask.await.expect("上游任务不应 panic");

    let (clearStatus, clearResponse) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/recording/clear",
        json!({}),
    )
    .await;
    assert_eq!(clearStatus, StatusCode::OK);
    assert_eq!(clearResponse["recording"]["transactionCount"], 0);
    let (missingStatus, missing) = requestJson(
        router.clone(),
        Method::GET,
        &format!("/api/v1/transactions/{transactionId}"),
        json!({}),
    )
    .await;
    assert_eq!(missingStatus, StatusCode::NOT_FOUND);
    assert_eq!(missing["code"], "transactionNotFound");
    let (stopStatus, _) =
        requestJson(router, Method::POST, "/api/v1/service/stop", json!({})).await;
    assert_eq!(stopStatus, StatusCode::OK);
}

/// 验证媒体预览只重组具有同一强实体标识的 Range，并以二进制流返回连续正文。
///
/// 运行上下文：测试通过真实 HTTP 代理录制同 URL 的重叠 206 分段，同时覆盖接缝损坏和
/// 无校验器非零分段。随后模拟 Chrome 的尾部 seek Range。失败语义要求不可靠分段返回
/// 明确 incomplete/continuousPrefix，绝不把无法证明同代的字节伪装成完整媒体。
#[tokio::test]
async fn mediaPreviewStreamsValidatedRangesAndRejectsUnsafeAssembly() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("绑定媒体预览上游服务");
    let upstreamAddress = upstream.local_addr().expect("读取媒体预览上游地址");
    let mut mediaBytes = vec![0_u8; 92];
    mediaBytes[0..4].copy_from_slice(&16_u32.to_be_bytes());
    mediaBytes[4..8].copy_from_slice(b"ftyp");
    mediaBytes[8..12].copy_from_slice(b"isom");
    mediaBytes[16..20].copy_from_slice(&1_u32.to_be_bytes());
    mediaBytes[20..24].copy_from_slice(b"mdat");
    mediaBytes[24..32].copy_from_slice(&32_u64.to_be_bytes());
    // mdat 中故意放入伪 hdlr/soun，结构化解析必须按盒尺寸跳过，不能裸搜索命中。
    mediaBytes[32..36].copy_from_slice(b"hdlr");
    mediaBytes[44..48].copy_from_slice(b"soun");
    mediaBytes[48..52].copy_from_slice(&44_u32.to_be_bytes());
    mediaBytes[52..56].copy_from_slice(b"moov");
    mediaBytes[56..60].copy_from_slice(&36_u32.to_be_bytes());
    mediaBytes[60..64].copy_from_slice(b"trak");
    mediaBytes[64..68].copy_from_slice(&28_u32.to_be_bytes());
    mediaBytes[68..72].copy_from_slice(b"mdia");
    mediaBytes[72..76].copy_from_slice(&20_u32.to_be_bytes());
    mediaBytes[76..80].copy_from_slice(b"hdlr");
    mediaBytes[88..92].copy_from_slice(b"soun");
    let firstRange = mediaBytes[..51].to_vec();
    let secondRange = mediaBytes[48..].to_vec();
    let mismatchFirstRange = firstRange.clone();
    let mut mismatchSecondRange = secondRange.clone();
    mismatchSecondRange[2] ^= 0xff;
    let duplicateEntityTagBody = firstRange.clone();
    let duplicateEntityTagTailBody = secondRange.clone();
    let duplicateContentRangeBody = firstRange.clone();
    let afterClearBody = mediaBytes.clone();
    let upstreamTask = tokio::spawn(async move {
        let responses = [
            (
                "206 Partial Content",
                vec![
                    ("Content-Type", "audio/mpeg".to_owned()),
                    ("Content-Range", "bytes 0-50/92".to_owned()),
                    ("ETag", "\"media-generation-1\"".to_owned()),
                ],
                firstRange,
            ),
            (
                "206 Partial Content",
                vec![
                    ("Content-Type", "audio/mpeg".to_owned()),
                    ("Content-Range", "bytes 48-91/92".to_owned()),
                    ("ETag", "\"media-generation-1\"".to_owned()),
                ],
                secondRange,
            ),
            (
                "206 Partial Content",
                vec![
                    ("Content-Type", "audio/mpeg".to_owned()),
                    ("Content-Range", "bytes 0-50/92".to_owned()),
                    ("ETag", "\"media-generation-corrupt\"".to_owned()),
                ],
                mismatchFirstRange,
            ),
            (
                "206 Partial Content",
                vec![
                    ("Content-Type", "audio/mpeg".to_owned()),
                    ("Content-Range", "bytes 48-91/92".to_owned()),
                    ("ETag", "\"media-generation-corrupt\"".to_owned()),
                ],
                mismatchSecondRange,
            ),
            (
                "206 Partial Content",
                vec![
                    ("Content-Type", "audio/mp4".to_owned()),
                    ("Content-Range", "bytes 4-7/8".to_owned()),
                    ("Last-Modified", "Sun, 09 Aug 2026 12:00:00 GMT".to_owned()),
                ],
                b"efgh".to_vec(),
            ),
            (
                "206 Partial Content",
                vec![
                    ("Content-Type", "audio/mp4".to_owned()),
                    ("Content-Range", "bytes 0-50/92".to_owned()),
                    ("ETag", "\"media-generation-duplicate-a\"".to_owned()),
                    ("ETag", "\"media-generation-duplicate-b\"".to_owned()),
                ],
                duplicateEntityTagBody,
            ),
            (
                "206 Partial Content",
                vec![
                    ("Content-Type", "audio/mp4".to_owned()),
                    ("Content-Range", "bytes 48-91/92".to_owned()),
                    ("ETag", "\"media-generation-duplicate-a\"".to_owned()),
                    ("ETag", "\"media-generation-duplicate-c\"".to_owned()),
                ],
                duplicateEntityTagTailBody,
            ),
            (
                "206 Partial Content",
                vec![
                    ("Content-Type", "audio/mp4".to_owned()),
                    ("Content-Range", "bytes 0-50/92".to_owned()),
                    ("Content-Range", "bytes 0-50/100".to_owned()),
                    ("ETag", "\"media-generation-duplicate-range\"".to_owned()),
                ],
                duplicateContentRangeBody,
            ),
            (
                "200 OK",
                vec![("Content-Type", "audio/mp4".to_owned())],
                afterClearBody,
            ),
        ];
        for (status, headers, body) in responses {
            let (mut stream, _) = upstream.accept().await.expect("接受媒体预览请求");
            let mut request = vec![0_u8; 4_096];
            let _ = stream.read(&mut request).await.expect("读取媒体预览请求");
            let mut response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str("\r\n");
            stream
                .write_all(response.as_bytes())
                .await
                .expect("写入媒体预览响应头");
            stream.write_all(&body).await.expect("写入媒体预览响应正文");
        }
    });

    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let proxyPort = findAvailablePort();
    let (putStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(proxyPort),
    )
    .await;
    assert_eq!(putStatus, StatusCode::OK);
    let (startStatus, startSnapshot) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/service/start",
        json!({}),
    )
    .await;
    assert_eq!(startStatus, StatusCode::OK);
    let proxyAddress = startSnapshot["listeners"]["httpProxy"]["boundEndpoint"]
        .as_str()
        .expect("媒体预览代理监听地址")
        .parse()
        .expect("解析媒体预览代理地址");

    for path in [
        "strong",
        "strong",
        "overlap-mismatch",
        "overlap-mismatch",
        "unvalidated",
        "duplicate-etag",
        "duplicate-etag",
        "duplicate-range",
    ] {
        let _ = requestThroughHttpProxy(proxyAddress, upstreamAddress, path).await;
    }
    let page = waitForTransactionCount(&router, 8).await;
    let transactions = page["items"].as_array().expect("媒体预览事务列表");
    let findTransaction = |path: &str| {
        transactions
            .iter()
            .rev()
            .find(|transaction| {
                transaction["urlDisplay"]
                    .as_str()
                    .is_some_and(|url| url.ends_with(path))
            })
            .and_then(|transaction| transaction["transactionId"].as_str())
            .expect("查找媒体预览事务")
    };

    let previewPath = format!(
        "/api/v1/transactions/{}/response/media-preview",
        findTransaction("strong")
    );
    let previewMetadata = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri(&previewPath)
                .body(Body::empty())
                .expect("构建媒体预览 HEAD 请求"),
        )
        .await
        .expect("读取媒体预览 HEAD 响应");
    assert_eq!(previewMetadata.headers()["content-length"], "92");
    assert_eq!(previewMetadata.headers()["content-type"], "audio/mp4");
    assert!(
        to_bytes(previewMetadata.into_body(), usize::MAX)
            .await
            .expect("读取媒体预览 HEAD 空正文")
            .is_empty(),
        "HEAD 只能读取分段元数据，不能提前聚合媒体正文"
    );

    let completePreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&previewPath)
                .body(Body::empty())
                .expect("构建完整媒体预览请求"),
        )
        .await
        .expect("读取完整媒体预览");
    assert_eq!(
        completePreview.headers()["x-media-preview-status"],
        "complete"
    );
    assert_eq!(completePreview.headers()["content-type"], "audio/mp4");
    assert_eq!(
        completePreview.headers()["x-media-preview-segment-count"],
        "2"
    );
    assert_eq!(
        to_bytes(completePreview.into_body(), usize::MAX)
            .await
            .expect("读取重组媒体正文"),
        mediaBytes
    );

    let seekPreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&previewPath)
                .header("range", "bytes=48-")
                .body(Body::empty())
                .expect("构建浏览器媒体 seek 请求"),
        )
        .await
        .expect("读取浏览器媒体 seek 响应");
    assert_eq!(seekPreview.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(seekPreview.headers()["accept-ranges"], "bytes");
    assert_eq!(seekPreview.headers()["content-range"], "bytes 48-91/92");
    assert_eq!(seekPreview.headers()["content-length"], "44");
    assert_eq!(
        to_bytes(seekPreview.into_body(), usize::MAX)
            .await
            .expect("读取浏览器 seek 范围正文"),
        mediaBytes[48..]
    );

    let closedRangePreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&previewPath)
                .header("range", "bytes=52-55")
                .body(Body::empty())
                .expect("构建闭区间媒体请求"),
        )
        .await
        .expect("读取闭区间媒体响应");
    assert_eq!(closedRangePreview.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        closedRangePreview.headers()["content-range"],
        "bytes 52-55/92"
    );
    assert_eq!(closedRangePreview.headers()["content-length"], "4");
    assert_eq!(
        to_bytes(closedRangePreview.into_body(), usize::MAX)
            .await
            .expect("读取闭区间媒体正文"),
        b"moov".as_slice()
    );

    let tailMetadata = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri(&previewPath)
                .header("range", "bytes=-44")
                .body(Body::empty())
                .expect("构建尾部 moov HEAD 请求"),
        )
        .await
        .expect("读取尾部 moov HEAD 响应");
    assert_eq!(tailMetadata.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(tailMetadata.headers()["content-range"], "bytes 48-91/92");
    assert_eq!(tailMetadata.headers()["content-length"], "44");
    assert!(
        to_bytes(tailMetadata.into_body(), usize::MAX)
            .await
            .expect("读取尾部 moov HEAD 空正文")
            .is_empty()
    );

    let multipleRanges = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&previewPath)
                .header("range", "bytes=0-1,4-5")
                .body(Body::empty())
                .expect("构建不支持的多范围请求"),
        )
        .await
        .expect("读取多范围拒绝响应");
    assert_eq!(multipleRanges.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(multipleRanges.headers()["content-range"], "bytes */92");

    let mismatchPreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/transactions/{}/response/media-preview",
                    findTransaction("overlap-mismatch")
                ))
                .body(Body::empty())
                .expect("构建损坏重叠分段预览请求"),
        )
        .await
        .expect("读取损坏重叠分段预览");
    assert_eq!(
        mismatchPreview.headers()["x-media-preview-status"],
        "continuousPrefix"
    );
    assert_eq!(mismatchPreview.headers()["content-length"], "51");
    assert_eq!(
        to_bytes(mismatchPreview.into_body(), usize::MAX)
            .await
            .expect("读取接缝前连续前缀"),
        mediaBytes[..51]
    );

    let incompletePreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/transactions/{}/response/media-preview",
                    findTransaction("unvalidated")
                ))
                .body(Body::empty())
                .expect("构建无校验器媒体预览请求"),
        )
        .await
        .expect("读取无校验器媒体预览");
    assert_eq!(
        incompletePreview.headers()["x-media-preview-status"],
        "incomplete"
    );
    assert_eq!(incompletePreview.headers()["content-length"], "0");

    // 两个分段都带重复 ETag；若错误采用首值会被拼成完整 92 字节，严格解析必须拒绝非零尾段。
    let duplicateEntityTagPreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/transactions/{}/response/media-preview",
                    findTransaction("duplicate-etag")
                ))
                .body(Body::empty())
                .expect("构建重复 ETag 媒体预览请求"),
        )
        .await
        .expect("读取重复 ETag 媒体预览");
    assert_eq!(
        duplicateEntityTagPreview.headers()["x-media-preview-status"],
        "incomplete"
    );
    assert_eq!(
        duplicateEntityTagPreview.headers()["x-media-preview-segment-count"],
        "0"
    );
    assert_eq!(duplicateEntityTagPreview.headers()["content-length"], "0");
    assert!(
        to_bytes(duplicateEntityTagPreview.into_body(), usize::MAX)
            .await
            .expect("读取重复 ETag 空正文")
            .is_empty()
    );

    // 206 的重复 Content-Range 不是无区间的完整响应，必须在响应头前明确标记 incomplete。
    let duplicateContentRangePreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/transactions/{}/response/media-preview",
                    findTransaction("duplicate-range")
                ))
                .body(Body::empty())
                .expect("构建重复 Content-Range 媒体预览请求"),
        )
        .await
        .expect("读取重复 Content-Range 媒体预览");
    assert_eq!(
        duplicateContentRangePreview.headers()["x-media-preview-status"],
        "incomplete"
    );
    assert_eq!(
        duplicateContentRangePreview.headers()["x-media-preview-segment-count"],
        "0"
    );
    assert_eq!(
        duplicateContentRangePreview.headers()["content-length"],
        "0"
    );
    assert!(
        to_bytes(duplicateContentRangePreview.into_body(), usize::MAX)
            .await
            .expect("读取重复 Content-Range 空正文")
            .is_empty()
    );

    // HEAD 只在处理器内执行有界类型嗅探；响应对象本身不长期占用正文租约配额。
    let mut headResponses = Vec::new();
    for _ in 0..32 {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri(&previewPath)
                    .body(Body::empty())
                    .expect("构建媒体 HEAD 配额请求"),
            )
            .await
            .expect("读取媒体 HEAD 配额响应");
        assert_eq!(response.status(), StatusCode::OK);
        headResponses.push(response);
    }

    // 不轮询 Body 的慢消费者会持有租约；达到会话并发上限后必须在响应头前明确返回 429。
    let mut slowPreviews = Vec::new();
    for _ in 0..16 {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&previewPath)
                    .body(Body::empty())
                    .expect("构建慢媒体预览请求"),
            )
            .await
            .expect("建立慢媒体预览响应");
        assert_eq!(response.status(), StatusCode::OK);
        slowPreviews.push(response);
    }
    let saturatedPreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&previewPath)
                .body(Body::empty())
                .expect("构建超配额媒体请求"),
        )
        .await
        .expect("读取超配额媒体响应");
    assert_eq!(saturatedPreview.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(saturatedPreview.headers()["retry-after"], "1");

    let (clearStatus, clearSnapshot) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/recording/clear",
        json!({}),
    )
    .await;
    assert_eq!(clearStatus, StatusCode::OK);
    assert_eq!(clearSnapshot["recording"]["transactionCount"], 0);
    let _ = requestThroughHttpProxy(proxyAddress, upstreamAddress, "after-clear").await;
    let afterClearPage = waitForTransactionCount(&router, 1).await;
    let afterClearTransactionId = afterClearPage["items"][0]["transactionId"]
        .as_str()
        .expect("clear 后媒体事务 ID");
    let afterClearPreviewPath =
        format!("/api/v1/transactions/{afterClearTransactionId}/response/media-preview");
    let pinnedAcrossClear = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&afterClearPreviewPath)
                .body(Body::empty())
                .expect("构建跨 clear 配额请求"),
        )
        .await
        .expect("读取跨 clear 配额响应");
    assert_eq!(pinnedAcrossClear.status(), StatusCode::TOO_MANY_REQUESTS);

    let leasedPreview = slowPreviews.pop().expect("必须存在稳定租约响应");
    let leasedBytes = to_bytes(leasedPreview.into_body(), usize::MAX)
        .await
        .expect("clear 后读取稳定租约媒体响应");
    assert_eq!(
        leasedBytes.len(),
        92,
        "固定 Content-Length 必须与实际发送一致"
    );
    assert_eq!(leasedBytes, mediaBytes);
    let recoveredPreview = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&afterClearPreviewPath)
                .body(Body::empty())
                .expect("构建释放配额后的媒体请求"),
        )
        .await
        .expect("读取释放配额后的媒体响应");
    assert_eq!(recoveredPreview.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(recoveredPreview.into_body(), usize::MAX)
            .await
            .expect("读取 clear 后新媒体正文"),
        mediaBytes
    );
    drop(slowPreviews);
    drop(headResponses);

    let (stopStatus, _) =
        requestJson(router, Method::POST, "/api/v1/service/stop", json!({})).await;
    assert_eq!(stopStatus, StatusCode::OK);
    upstreamTask.await.expect("媒体预览上游任务不应 panic");
}

/// 验证新协议严格要求 httpProxy，并拒绝生产端口零和双监听冲突，不保留旧请求兼容分支。
#[tokio::test]
async fn httpConfigurationIsStrictAndRejectsConflicts() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let socksPort = findAvailablePort();
    let mut legacyConfiguration = configurationJson(socksPort);
    legacyConfiguration
        .as_object_mut()
        .expect("配置必须是对象")
        .remove("httpProxy");
    let (legacyStatus, legacyError) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        legacyConfiguration,
    )
    .await;
    assert_eq!(legacyStatus, StatusCode::BAD_REQUEST);
    assert_eq!(legacyError["code"], "invalidConfigurationRequest");

    let mut maximumHeaderConfiguration = configurationJson(socksPort);
    maximumHeaderConfiguration["httpProxy"]["maxConnections"] = json!(256);
    maximumHeaderConfiguration["httpProxy"]["maxHeaderBytes"] = json!(1_048_576);
    let (maximumHeaderStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        maximumHeaderConfiguration.clone(),
    )
    .await;
    assert_eq!(maximumHeaderStatus, StatusCode::OK);
    maximumHeaderConfiguration["httpProxy"]["maxConnections"] = json!(257);
    let (headerBudgetStatus, headerBudgetError) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        maximumHeaderConfiguration,
    )
    .await;
    assert_eq!(headerBudgetStatus, StatusCode::BAD_REQUEST);
    assert_eq!(headerBudgetError["code"], "invalidHttpProxyConfiguration");

    let mut zeroPort = configurationJson(socksPort);
    zeroPort["httpProxy"] = json!({
        "enabled": true,
        "listenHost": "127.0.0.1",
        "listenPort": 0,
        "maxConnections": 16,
        "maxHeaderBytes": 16384,
        "maxCaptureBodyBytes": 65536,
        "connectTimeoutMilliseconds": 2000,
        "requestTimeoutMilliseconds": 5000,
        "headerReadTimeoutMilliseconds": 2000,
        "shutdownTimeoutMilliseconds": 2000
    });
    let (zeroStatus, zeroError) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        zeroPort,
    )
    .await;
    assert_eq!(zeroStatus, StatusCode::BAD_REQUEST);
    assert_eq!(zeroError["code"], "invalidHttpProxyConfiguration");

    let mut conflict = configurationJson(socksPort);
    conflict["httpProxy"] = json!({
        "enabled": true,
        "listenHost": "0.0.0.0",
        "listenPort": socksPort,
        "maxConnections": 16,
        "maxHeaderBytes": 16384,
        "maxCaptureBodyBytes": 65536,
        "connectTimeoutMilliseconds": 2000,
        "requestTimeoutMilliseconds": 5000,
        "headerReadTimeoutMilliseconds": 2000,
        "shutdownTimeoutMilliseconds": 2000
    });
    let (conflictStatus, conflictSnapshot) =
        requestJson(router, Method::PUT, "/api/v1/configuration", conflict).await;
    assert_eq!(conflictStatus, StatusCode::OK);
    assert_eq!(
        conflictSnapshot["configuration"]["httpProxy"]["listenHost"],
        "127.0.0.1"
    );
    assert_eq!(
        conflictSnapshot["configuration"]["httpProxy"]["listenPort"],
        socksPort
    );
}

/// 验证一个监听器绑定失败时另一监听器继续服务，并向控制面发布稳定错误键和原因码。
#[tokio::test]
async fn partialListenerFailureKeepsAvailableProxyRunning() {
    let occupiedHttpListener =
        StandardTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("占用 HTTP 测试端口");
    let occupiedHttpPort = occupiedHttpListener
        .local_addr()
        .expect("读取被占用的 HTTP 端口")
        .port();
    let socksPort = findAvailablePort();
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let mut configuration = configurationJson(socksPort);
    configuration["httpProxy"]["enabled"] = json!(true);
    configuration["httpProxy"]["listenPort"] = json!(occupiedHttpPort);
    let (putStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configuration,
    )
    .await;
    assert_eq!(putStatus, StatusCode::OK);

    let (startStatus, snapshot) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/service/start",
        json!({}),
    )
    .await;
    assert_eq!(startStatus, StatusCode::OK);
    assert_eq!(snapshot["serviceState"], "running");
    assert_eq!(snapshot["listeners"]["socks5"]["state"], "running");
    assert_eq!(snapshot["listeners"]["httpProxy"]["state"], "running");
    assert_eq!(
        snapshot["listeners"]["httpProxy"]["boundEndpoint"],
        snapshot["listeners"]["socks5"]["boundEndpoint"]
    );

    let (stopStatus, stopped) =
        requestJson(router, Method::POST, "/api/v1/service/stop", json!({})).await;
    assert_eq!(stopStatus, StatusCode::OK);
    assert_eq!(stopped["serviceState"], "stopped");
    drop(occupiedHttpListener);
}

/// 验证控制面的成功与失败响应统一禁止缓存，避免配置或抓包元数据残留在浏览器缓存。
#[tokio::test]
async fn controlResponsesAlwaysUseNoStore() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let successResponse = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/snapshot")
                .body(Body::empty())
                .expect("构建快照请求"),
        )
        .await
        .expect("读取快照响应");
    assert_eq!(successResponse.headers()["cache-control"], "no-store");

    let invalidResponse = router
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/api/v1/configuration")
                .header("content-type", "application/json")
                .body(Body::from(configurationJson(0).to_string()))
                .expect("构建无效配置请求"),
        )
        .await
        .expect("读取无效配置响应");
    assert_eq!(invalidResponse.status(), StatusCode::BAD_REQUEST);
    assert_eq!(invalidResponse.headers()["cache-control"], "no-store");
}

/// 验证分页集合令牌在 clear 后返回稳定 409，调用方不会在已变化集合上继续 offset 翻页。
#[tokio::test]
async fn transactionCollectionTokenRejectsChangedSet() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (firstStatus, firstPage) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/transactions?offset=0&limit=1",
        json!({}),
    )
    .await;
    assert_eq!(firstStatus, StatusCode::OK);
    let collectionToken = firstPage["collectionToken"]
        .as_str()
        .expect("事务页必须返回集合令牌")
        .to_owned();
    let (missingTokenStatus, missingTokenError) = requestJson(
        router.clone(),
        Method::GET,
        "/api/v1/transactions?offset=1&limit=1",
        json!({}),
    )
    .await;
    assert_eq!(missingTokenStatus, StatusCode::BAD_REQUEST);
    assert_eq!(missingTokenError["code"], "invalidTransactionsQuery");
    let (clearStatus, _) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/recording/clear",
        json!({}),
    )
    .await;
    assert_eq!(clearStatus, StatusCode::OK);
    let (changedStatus, changedError) = requestJson(
        router,
        Method::GET,
        &format!("/api/v1/transactions?offset=0&limit=1&collectionToken={collectionToken}"),
        json!({}),
    )
    .await;
    assert_eq!(changedStatus, StatusCode::CONFLICT);
    assert_eq!(changedError["code"], "transactionsCollectionChanged");
    assert_eq!(
        changedError["messageKey"],
        "error.transactionsCollectionChanged"
    );
    assert_eq!(changedError["params"], json!({}));
}

/// 验证录制状态仍支持部分更新，而任何会重新启用正文裁剪或事务淘汰的限额更新都会被拒绝。
#[tokio::test]
async fn recordingUpdateIsPartialRevisionedAndResourceBounded() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (_, before) =
        requestJson(router.clone(), Method::GET, "/api/v1/recording", json!({})).await;
    let (updateStatus, updated) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/recording",
        json!({"state": "paused"}),
    )
    .await;
    assert_eq!(updateStatus, StatusCode::OK);
    assert!(updated["revision"].as_u64() > before["revision"].as_u64());
    assert_eq!(updated["recording"]["state"], "paused");
    assert_eq!(
        updated["recording"]["limits"],
        before["recording"]["limits"]
    );

    let (limitStatus, limitError) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/recording",
        json!({"limits": {"maxTransactions": 123}}),
    )
    .await;
    assert_eq!(limitStatus, StatusCode::BAD_REQUEST);
    assert_eq!(limitError["code"], "invalidRecordingLimits");

    let (bodyLimitStatus, bodyLimitError) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/recording",
        json!({"limits": {"maxBodyBytes": 8388608}}),
    )
    .await;
    assert_eq!(bodyLimitStatus, StatusCode::BAD_REQUEST);
    assert_eq!(bodyLimitError["code"], "invalidRecordingLimits");

    let (_, afterRejectedUpdate) =
        requestJson(router, Method::GET, "/api/v1/recording", json!({})).await;
    assert_eq!(
        afterRejectedUpdate["recording"]["limits"],
        before["recording"]["limits"]
    );
}

/// 验证 Accept-Language 与 query 覆盖能生成同一机器码、不同文案的严格错误结构。
#[tokio::test]
async fn localizedErrorsKeepStableMachineContract() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (japaneseStatus, japaneseError) =
        requestInvalidConfiguration(router.clone(), "/api/v1/configuration", "ja").await;
    assert_eq!(japaneseStatus, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(japaneseError["code"], "invalidListenPort");
    assert_eq!(japaneseError["messageKey"], "error.invalidListenPort");
    assert!(japaneseError.get("errorMessage").is_none());
    assert!(
        japaneseError["message"]
            .as_str()
            .expect("日文错误必须是字符串")
            .contains("65535")
    );

    let (_, englishError) =
        requestInvalidConfiguration(router, "/api/v1/configuration?locale=en", "zh-Hans").await;
    assert_eq!(
        englishError["message"],
        "listenPort must be between 1 and 65535"
    );
    assert_eq!(englishError["params"], json!({}));
}

/// 验证框架与数据面原始 detail 只进入 params，英文和日文 message 保持纯目录文案。
#[tokio::test]
async fn localizedErrorsKeepRawDetailOutOfMessages() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (_, frameworkError) = requestConfigurationBody(
        router.clone(),
        "/api/v1/configuration?locale=ja",
        "zh-Hans",
        "{".to_owned(),
    )
    .await;
    assert_eq!(frameworkError["message"], "設定リクエストが無効です。");
    let frameworkDetail = frameworkError["params"]["detail"]
        .as_str()
        .expect("Serde 拒绝必须保留 detail 参数");
    assert!(!frameworkDetail.is_empty());
    assert!(
        !frameworkError["message"]
            .as_str()
            .expect("日文 message 必须是字符串")
            .contains(frameworkDetail)
    );

    let mut invalidConfiguration = configurationJson(1_080);
    invalidConfiguration["maxConnections"] = json!(0);
    let (_, validationError) = requestConfigurationBody(
        router,
        "/api/v1/configuration?locale=en",
        "zh-Hans",
        invalidConfiguration.to_string(),
    )
    .await;
    assert_eq!(validationError["message"], "Configuration is invalid.");
    assert!(
        validationError["params"]["detail"]
            .as_str()
            .expect("数据面校验必须保留 detail 参数")
            .contains("必须")
    );
    assert!(
        !validationError["message"]
            .as_str()
            .expect("英文 message 必须是字符串")
            .contains("必须")
    );
}

/// 验证 HTTP 配置、启动、快照和停止契约，响应不得包含原始密码。
#[tokio::test]
async fn controlLifecycleReturnsStrictRedactedSnapshot() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let mut configuration = configurationJson(findAvailablePort());
    configuration["authenticationMode"] = json!("password");
    configuration["credentials"] = json!({
        "username": "alice",
        "password": "secretValue"
    });
    let (putStatus, putSnapshot) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configuration.clone(),
    )
    .await;
    assert_eq!(putStatus, axum::http::StatusCode::OK);
    assert_eq!(
        putSnapshot["configuration"]["authenticationUsernames"],
        json!(["alice"])
    );
    let serializedPut = putSnapshot.to_string();
    assert!(!serializedPut.contains("secretValue"));
    assert!(!serializedPut.contains("\"users\""));

    let mut preservedConfiguration = configuration.clone();
    preservedConfiguration["credentials"] = Value::Null;
    let (preserveStatus, preserveSnapshot) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        preservedConfiguration,
    )
    .await;
    assert_eq!(preserveStatus, axum::http::StatusCode::OK);
    assert_eq!(
        preserveSnapshot["configuration"]["authenticationUsernames"],
        json!(["alice"])
    );

    let mut missingCredentials = configuration;
    missingCredentials
        .as_object_mut()
        .expect("配置必须是对象")
        .remove("credentials");
    let (missingStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        missingCredentials,
    )
    .await;
    assert_eq!(missingStatus, axum::http::StatusCode::BAD_REQUEST);

    let (startStatus, startSnapshot) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/service/start",
        json!({}),
    )
    .await;
    assert_eq!(startStatus, axum::http::StatusCode::OK);
    assert_eq!(startSnapshot["serviceState"], "running");
    assert!(startSnapshot.get("boundEndpoint").is_none());
    assert!(startSnapshot["listeners"]["socks5"]["boundEndpoint"].is_string());
    assert!(startSnapshot["revision"].as_u64().is_some());
    assert_eq!(
        startSnapshot["metrics"],
        json!({
            "acceptedConnections": 0,
            "activeConnections": 0,
            "failedConnections": 0,
            "bytesUp": 0,
            "bytesDown": 0,
            "udpPacketsUp": 0,
            "udpPacketsDown": 0,
            "droppedUdpPackets": 0
        })
    );

    let (snapshotStatus, snapshot) =
        requestJson(router.clone(), Method::GET, "/api/v1/snapshot", json!({})).await;
    assert_eq!(snapshotStatus, axum::http::StatusCode::OK);
    assert!(!snapshot.to_string().contains("secretValue"));

    let (stopStatus, stopSnapshot) =
        requestJson(router, Method::POST, "/api/v1/service/stop", json!({})).await;
    assert_eq!(stopStatus, axum::http::StatusCode::OK);
    assert_eq!(stopSnapshot["serviceState"], "stopped");
    assert!(stopSnapshot["listeners"]["socks5"]["boundEndpoint"].is_null());
}

/// 验证插件认证模式无需静态凭据即可持久提交，并从权威快照恢复为同一模式。
#[tokio::test]
async fn pluginAuthenticationConfigurationPersistsWithoutCredentials() {
    let state = newControlState().await;
    let router = createControlRouter((*state).clone());
    let mut configuration = configurationJson(findAvailablePort());
    configuration["authenticationMode"] = json!("plugin");
    configuration["credentials"] = Value::Null;

    let (putStatus, putSnapshot) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configuration,
    )
    .await;
    assert_eq!(putStatus, axum::http::StatusCode::OK);
    assert_eq!(
        putSnapshot["configuration"]["authenticationMode"],
        json!("plugin")
    );
    assert_eq!(
        putSnapshot["configuration"]["authenticationUsernames"],
        json!([])
    );

    let (getStatus, getSnapshot) =
        requestJson(router, Method::GET, "/api/v1/snapshot", json!(null)).await;
    assert_eq!(getStatus, axum::http::StatusCode::OK);
    assert_eq!(
        getSnapshot["configuration"]["authenticationMode"],
        json!("plugin")
    );
}
