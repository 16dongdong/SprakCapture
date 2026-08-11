#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::time::{Duration, Instant};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tower::ServiceExt;

use proxy_backend::controlApi::{EventMessage, ServiceState, createControlRouter};
use socks5_core::SessionState;

#[path = "support/controlApiSupport.rs"]
mod controlApiSupport;

use controlApiSupport::{
    configurationJson, findAvailablePort, newControlState, parseJsonResponse,
    relayTrafficThroughSocks, requestJson, requestThroughHttpProxy, waitForCompletedTransaction,
    waitForTransactionCount,
};

/// 验证会话和累计指标跨 stop/start 保留，显式 clear 只删除历史会话且不会重复累计指标。
#[tokio::test]
async fn runtimeHistoryPersistsUntilExplicitClear() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (putStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(findAvailablePort()),
    )
    .await;
    assert_eq!(putStatus, axum::http::StatusCode::OK);

    let started = state.startService().await.expect("启动历史测试服务");
    let proxyAddress: std::net::SocketAddr = started
        .listeners
        .socks5
        .boundEndpoint
        .expect("历史测试缺少绑定端点")
        .parse()
        .expect("解析历史测试绑定端点");
    relayTrafficThroughSocks(proxyAddress).await;
    let transaction = waitForCompletedTransaction(&router).await;
    assert_eq!(transaction["protocol"], "socks");
    assert_eq!(transaction["method"], "CONNECT");
    assert_eq!(transaction["status"], "complete");
    assert_eq!(transaction["sizes"]["requestBodyBytes"], 7);
    assert_eq!(transaction["sizes"]["responseBodyBytes"], 7);
    let transactionId = transaction["transactionId"]
        .as_str()
        .expect("SOCKS 事务缺少标识");
    for side in ["request", "response"] {
        let (bodyStatus, body) = requestJson(
            router.clone(),
            Method::GET,
            &format!("/api/v1/transactions/{transactionId}/{side}/body"),
            Value::Null,
        )
        .await;
        assert_eq!(bodyStatus, axum::http::StatusCode::OK);
        assert_eq!(body["base64"], "aGlzdG9yeQ==");
        assert_eq!(body["meta"]["storedBytes"], 7);
        assert_eq!(body["meta"]["originalBytes"], 7);
        assert_eq!(body["meta"]["contentType"], "application/octet-stream");
        assert_eq!(body["meta"]["encoding"], "binary");
    }
    let stopped = state.stopService().await.expect("停止历史测试服务");
    assert_eq!(stopped.sessions.len(), 1);
    assert!(matches!(
        stopped.sessions[0].state,
        SessionState::Closed | SessionState::Failed
    ));
    assert!(stopped.sessions[0].closedAtMilliseconds > 0);
    assert!(stopped.sessions[0].capturedBytesUp.is_empty());
    assert!(stopped.sessions[0].capturedBytesDown.is_empty());
    assert_eq!(stopped.metrics.activeConnections, 0);
    assert_eq!(stopped.metrics.acceptedConnections, 1);
    assert_eq!(stopped.metrics.bytesUp, 7);
    assert_eq!(stopped.metrics.bytesDown, 7);

    let restarted = state.startService().await.expect("再次启动历史测试服务");
    assert_eq!(restarted.sessions.len(), 1);
    assert_eq!(restarted.metrics.acceptedConnections, 1);
    let stoppedAgain = state.stopService().await.expect("再次停止历史测试服务");
    assert_eq!(stoppedAgain.metrics.acceptedConnections, 1);

    let cleared = state.clearSessions().await;
    assert!(cleared.sessions.is_empty());
    assert_eq!(cleared.metrics.acceptedConnections, 1);
    assert_eq!(cleared.metrics.bytesUp, 7);
    assert_eq!(cleared.metrics.bytesDown, 7);
}

/// 验证运行中的 SOCKS5 服务在清空录制后继续接收新会话，旧事务不会残留在新的录制集合中。
#[tokio::test]
async fn clearRecordingKeepsRunningSocksServiceUsable() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (configurationStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(findAvailablePort()),
    )
    .await;
    assert_eq!(configurationStatus, StatusCode::OK);
    let started = state.startService().await.expect("启动清空录制测试服务");
    let proxyAddress = started
        .listeners
        .socks5
        .boundEndpoint
        .expect("清空录制测试缺少 SOCKS5 监听端点")
        .parse()
        .expect("解析 SOCKS5 监听端点");

    relayTrafficThroughSocks(proxyAddress).await;
    assert_eq!(waitForTransactionCount(&router, 1).await["total"], 1);

    let (clearStatus, clearResponse) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/recording/clear",
        json!({}),
    )
    .await;
    assert_eq!(clearStatus, StatusCode::OK);
    assert_eq!(clearResponse["recording"]["transactionCount"], 0);

    relayTrafficThroughSocks(proxyAddress).await;
    assert_eq!(waitForTransactionCount(&router, 1).await["total"], 1);
    assert_eq!(state.snapshot().await.serviceState, ServiceState::Running);
    state.stopService().await.expect("停止清空录制测试服务");
}

/// 验证运行中提交配置会先关闭当前数据面、断开活动 SOCKS5 连接，再使用新监听端口恢复服务。
#[tokio::test]
async fn runningConfigurationUpdateForcesRestartAndDisconnectsActiveProxyConnections() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let initialPort = findAvailablePort();
    let mut replacementPort = findAvailablePort();
    while replacementPort == initialPort {
        replacementPort = findAvailablePort();
    }
    let (initialConfigurationStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(initialPort),
    )
    .await;
    assert_eq!(initialConfigurationStatus, StatusCode::OK);

    let started = state.startService().await.expect("启动配置重启测试服务");
    let initialAddress: std::net::SocketAddr = started
        .listeners
        .socks5
        .boundEndpoint
        .expect("配置重启测试缺少初始 SOCKS5 端点")
        .parse()
        .expect("解析初始 SOCKS5 端点");
    let mut activeClient = TcpStream::connect(initialAddress)
        .await
        .expect("连接待强制断开的 SOCKS5 客户端");
    activeClient
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .expect("写入 SOCKS5 协商请求");
    let mut methodReply = [0_u8; 2];
    activeClient
        .read_exact(&mut methodReply)
        .await
        .expect("读取 SOCKS5 协商响应");
    assert_eq!(methodReply, [0x05, 0x00]);
    timeout(Duration::from_secs(2), async {
        loop {
            if state.snapshot().await.metrics.activeConnections == 1 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("活动 SOCKS5 连接未进入运行快照");

    let (updateStatus, updated) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(replacementPort),
    )
    .await;
    assert_eq!(updateStatus, StatusCode::OK);
    assert_eq!(updated["serviceState"], "running");
    assert_eq!(updated["configuration"]["listenPort"], replacementPort);
    let replacementAddress: std::net::SocketAddr = updated["listeners"]["socks5"]["boundEndpoint"]
        .as_str()
        .expect("强制重启后缺少 SOCKS5 端点")
        .parse()
        .expect("解析重启后的 SOCKS5 端点");
    let mut closedBuffer = [0_u8; 1];
    let closedBytes = timeout(Duration::from_secs(2), activeClient.read(&mut closedBuffer))
        .await
        .expect("旧 SOCKS5 客户端未在重启中断开")
        .expect("读取旧 SOCKS5 客户端关闭状态");
    assert_eq!(closedBytes, 0);
    assert_eq!(updated["metrics"]["activeConnections"], 0);
    let replacementClient = TcpStream::connect(replacementAddress)
        .await
        .expect("新配置的 SOCKS5 监听器不可连接");
    drop(replacementClient);
    state.stopService().await.expect("停止配置重启测试服务");
}

/// 公开停止端点在活动长连接下必须快速返回 stopped，并保证旧监听端口已可立即重绑。
#[tokio::test]
async fn serviceStopForceClosesActiveSocketWithoutFaulting() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (configurationStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(findAvailablePort()),
    )
    .await;
    assert_eq!(configurationStatus, StatusCode::OK);
    let started = state
        .startService()
        .await
        .expect("强制停止测试服务必须启动");
    let proxyAddress: std::net::SocketAddr = started
        .listeners
        .socks5
        .boundEndpoint
        .expect("强制停止测试缺少监听端点")
        .parse()
        .expect("解析强制停止监听端点");
    let mut client = TcpStream::connect(proxyAddress)
        .await
        .expect("活动客户端必须连接");
    client
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .expect("活动客户端必须写入协商请求");
    let mut methodReply = [0_u8; 2];
    client
        .read_exact(&mut methodReply)
        .await
        .expect("活动客户端必须读取协商响应");
    assert_eq!(methodReply, [0x05, 0x00]);

    let startedAt = Instant::now();
    let (stopStatus, stopped) = timeout(
        Duration::from_secs(1),
        requestJson(router, Method::POST, "/api/v1/service/stop", json!({})),
    )
    .await
    .expect("停止端点不得等待活动连接排空");
    assert_eq!(stopStatus, StatusCode::OK);
    assert_eq!(stopped["serviceState"], "stopped");
    assert!(startedAt.elapsed() < Duration::from_secs(1));

    let mut byte = [0_u8; 1];
    let closeResult = timeout(Duration::from_millis(500), client.read(&mut byte))
        .await
        .expect("活动客户端必须立即观察到关闭");
    let connectionClosed = match closeResult {
        Ok(0) => true,
        Err(error) => matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
        ),
        Ok(_) => false,
    };
    assert!(connectionClosed);
    let rebound = TcpListener::bind(proxyAddress)
        .await
        .expect("停止响应返回后原端口必须立即可重绑");
    drop(rebound);
}

/// 验证停止后重新启动会创建可用的新数据面生命周期，后续 SOCKS5 流量仍会进入录制事务。
#[tokio::test]
async fn restartAfterStopCreatesUsableCaptureLifecycle() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (configurationStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(findAvailablePort()),
    )
    .await;
    assert_eq!(configurationStatus, StatusCode::OK);

    state
        .startService()
        .await
        .expect("启动首次 SOCKS5 生命周期");
    state.stopService().await.expect("停止首次 SOCKS5 生命周期");
    let restarted = state
        .startService()
        .await
        .expect("启动新的 SOCKS5 生命周期");
    let proxyAddress = restarted
        .listeners
        .socks5
        .boundEndpoint
        .expect("重启后缺少 SOCKS5 监听端点")
        .parse()
        .expect("解析重启后的 SOCKS5 监听端点");
    relayTrafficThroughSocks(proxyAddress).await;
    let transaction = waitForCompletedTransaction(&router).await;
    assert_eq!(transaction["protocol"], "socks");
    assert_eq!(state.snapshot().await.serviceState, ServiceState::Running);
    state
        .stopService()
        .await
        .expect("停止重启后的 SOCKS5 生命周期");
}

/// 连续启动和强制停止必须重复释放同一监听端口，前一代长连接不得污染下一代生命周期。
#[tokio::test]
async fn repeatedStartStopCyclesCloseEverySocketGeneration() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (configurationStatus, _) = requestJson(
        router,
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(findAvailablePort()),
    )
    .await;
    assert_eq!(configurationStatus, StatusCode::OK);

    for _ in 0..5 {
        let started = state
            .startService()
            .await
            .expect("连续生命周期必须成功启动");
        let proxyAddress: std::net::SocketAddr = started
            .listeners
            .socks5
            .boundEndpoint
            .expect("连续生命周期缺少监听端点")
            .parse()
            .expect("解析连续生命周期监听端点");
        let mut client = TcpStream::connect(proxyAddress)
            .await
            .expect("每一代客户端必须连接");
        client
            .write_all(&[0x05, 0x01, 0x00])
            .await
            .expect("每一代客户端必须写入协商请求");
        let mut reply = [0_u8; 2];
        client
            .read_exact(&mut reply)
            .await
            .expect("每一代客户端必须读取协商响应");
        assert_eq!(reply, [0x05, 0x00]);

        timeout(Duration::from_secs(1), state.stopService())
            .await
            .expect("连续强制停止不得等待长连接")
            .expect("连续强制停止必须成功");
        let mut byte = [0_u8; 1];
        let closeResult = timeout(Duration::from_millis(500), client.read(&mut byte))
            .await
            .expect("上一代客户端必须立即断开");
        assert!(matches!(closeResult, Ok(0) | Err(_)));
        let rebound = TcpListener::bind(proxyAddress)
            .await
            .expect("每次停止后端口必须立即可重绑");
        drop(rebound);
    }
}

/// 验证通过公开订阅接口可观察到一次服务启动对应的运行状态、会话和指标快照。
#[tokio::test]
async fn runtimeLifecyclePublishesObservableViews() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (configurationStatus, _) = requestJson(
        router,
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(findAvailablePort()),
    )
    .await;
    assert_eq!(configurationStatus, StatusCode::OK);

    let mut events = state.subscribeEvents();
    state.startService().await.expect("启动运行事件测试服务");
    timeout(Duration::from_secs(1), async {
        let mut observedRunning = false;
        let mut observedSessions = false;
        let mut observedMetrics = false;
        let mut observedProcessCapture = false;
        loop {
            match events.recv().await.expect("运行事件通道意外关闭") {
                EventMessage::ServiceState {
                    serviceState: ServiceState::Running,
                    ..
                } => observedRunning = true,
                EventMessage::Sessions { .. } => observedSessions = true,
                EventMessage::Metrics { .. } => observedMetrics = true,
                EventMessage::ProcessCapture { .. } => observedProcessCapture = true,
                _ => {}
            }
            if observedRunning && observedSessions && observedMetrics && observedProcessCapture {
                return;
            }
        }
    })
    .await
    .expect("服务启动后未发布完整运行视图");
    state.stopService().await.expect("停止运行事件测试服务");
}

/// 验证融合端口上的普通 HTTP 不再绕过服务指标，并通过控制事件实时发布真实套接字字节。
#[tokio::test]
async fn fusedHttpConnectionPublishesExactRuntimeMetrics() {
    let upstream = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定 HTTP 指标上游");
    let upstreamAddress = upstream.local_addr().expect("读取 HTTP 指标上游地址");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.expect("接受 HTTP 指标请求");
        let mut requestBytes = Vec::new();
        loop {
            let mut chunk = [0_u8; 256];
            let byteCount = stream.read(&mut chunk).await.expect("读取 HTTP 指标请求");
            if byteCount == 0 {
                break;
            }
            requestBytes.extend_from_slice(&chunk[..byteCount]);
            if requestBytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nmetric")
            .await
            .expect("写入 HTTP 指标响应");
    });

    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let (configurationStatus, _) = requestJson(
        router,
        Method::PUT,
        "/api/v1/configuration",
        configurationJson(findAvailablePort()),
    )
    .await;
    assert_eq!(configurationStatus, StatusCode::OK);
    let started = state.startService().await.expect("启动融合 HTTP 指标服务");
    let proxyAddress = started
        .listeners
        .httpProxy
        .boundEndpoint
        .expect("融合 HTTP 指标缺少监听端点")
        .parse()
        .expect("解析融合 HTTP 指标监听端点");
    let mut events = state.subscribeEvents();
    let requestLine = format!(
        "GET http://{upstreamAddress}/metrics HTTP/1.1\r\nHost: {upstreamAddress}\r\nConnection: close\r\n\r\n"
    );
    let response = requestThroughHttpProxy(proxyAddress, upstreamAddress, "metrics").await;
    upstreamTask.await.expect("HTTP 指标上游任务必须结束");

    let observedMetrics = timeout(Duration::from_secs(2), async {
        loop {
            if let EventMessage::Metrics { metrics, .. } =
                events.recv().await.expect("HTTP 指标事件通道意外关闭")
                && metrics.acceptedConnections == 1
                && metrics.activeConnections == 0
                && metrics.bytesUp == requestLine.len() as u64
                && metrics.bytesDown == response.len() as u64
            {
                return metrics;
            }
        }
    })
    .await
    .expect("融合 HTTP 指标未实时发布终态");
    assert_eq!(observedMetrics.failedConnections, 0);

    let running = state.snapshot().await;
    assert_eq!(running.metrics, observedMetrics);
    while events.try_recv().is_ok() {}

    // 第二条连接在 50ms 指标合并窗口内触发停止，覆盖发送端关闭与最终归档交错的边界。
    // 停止期间允许 active 归零，但 accepted 和累计字节绝不能发布为更小的高 revision 快照。
    let mut heldClient = TcpStream::connect(proxyAddress)
        .await
        .expect("建立停止竞态 HTTP 连接");
    heldClient
        .write_all(requestLine.as_bytes())
        .await
        .expect("写入停止竞态 HTTP 请求");
    timeout(Duration::from_secs(1), async {
        loop {
            let metrics = state.snapshot().await.metrics;
            if metrics.acceptedConnections == 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("融合监听必须接纳停止竞态 HTTP 连接");
    tokio::time::sleep(Duration::from_millis(5)).await;
    let stopped = state.stopService().await.expect("停止融合 HTTP 指标服务");
    drop(heldClient);
    // 等待超过合并窗口，确保任何已排队定时器都已执行或退出后再检查完整事件序列。
    tokio::time::sleep(Duration::from_millis(75)).await;
    assert_eq!(stopped.metrics.acceptedConnections, 2);
    assert_eq!(stopped.metrics.activeConnections, 0);
    assert!(stopped.metrics.bytesUp >= observedMetrics.bytesUp);
    assert!(stopped.metrics.bytesDown >= observedMetrics.bytesDown);
    while let Ok(event) = events.try_recv() {
        if let EventMessage::Metrics { metrics, .. } = event {
            assert!(
                metrics.acceptedConnections >= 2,
                "停止归档期间不得广播缺失当前 HTTP 生命周期的指标"
            );
            assert!(metrics.bytesUp >= observedMetrics.bytesUp);
            assert!(metrics.bytesDown >= observedMetrics.bytesDown);
        }
    }
}

/// 验证预检仅返回声明的本地 Origin、方法和 Content-Type。
#[tokio::test]
async fn corsPreflightAllowsDeclaredWebOrigins() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    for origin in [
        "http://127.0.0.1:5173",
        "http://localhost:5173",
        "http://127.0.0.1:5174",
        "http://localhost:5174",
        "http://127.0.0.1:5175",
        "http://localhost:5175",
    ] {
        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/v1/configuration")
            .header("origin", origin)
            .header("access-control-request-method", "PUT")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .expect("构建 CORS 预检");
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("执行 CORS 预检");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .expect("允许 Origin"),
            origin
        );
        let methods = response
            .headers()
            .get("access-control-allow-methods")
            .expect("允许方法")
            .to_str()
            .expect("方法头文本");
        assert!(methods.contains("PUT"));
        let headers = response
            .headers()
            .get("access-control-allow-headers")
            .expect("允许请求头")
            .to_str()
            .expect("请求头文本");
        assert!(headers.to_ascii_lowercase().contains("content-type"));
    }

    let deniedRequest = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/v1/configuration")
        .header("origin", "http://127.0.0.1:5176")
        .header("access-control-request-method", "PUT")
        .body(Body::empty())
        .expect("构建未声明 Origin 预检");
    let deniedResponse = router
        .oneshot(deniedRequest)
        .await
        .expect("执行未声明 Origin 预检");
    assert_eq!(deniedResponse.status(), axum::http::StatusCode::FORBIDDEN);
    assert!(
        deniedResponse
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

/// 验证恶意 Origin 在 HTTP 动作和 WebSocket 升级前均被拒绝，允许 Origin 与无 Origin 请求保持可用。
#[tokio::test]
async fn originBoundaryProtectsHttpAndWebSocketRoutes() {
    let state = newControlState().await;
    let router = createControlRouter(state.clone());
    let deniedStart = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/service/start")
        .header("origin", "https://example.invalid")
        .body(Body::empty())
        .expect("构建恶意 Origin 启动请求");
    let deniedStartResponse = router
        .clone()
        .oneshot(deniedStart)
        .await
        .expect("执行恶意 Origin 启动请求");
    assert_eq!(
        deniedStartResponse.status(),
        axum::http::StatusCode::FORBIDDEN
    );
    assert_eq!(state.snapshot().await.serviceState, ServiceState::Stopped);

    let deniedWebSocket = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/events?locale=zh-Hant-TW")
        .header("origin", "https://example.invalid")
        .header("accept-language", "en")
        .body(Body::empty())
        .expect("构建恶意 Origin WebSocket 请求");
    let deniedWebSocketResponse = router
        .clone()
        .oneshot(deniedWebSocket)
        .await
        .expect("执行恶意 Origin WebSocket 请求");
    assert_eq!(
        deniedWebSocketResponse.status(),
        axum::http::StatusCode::FORBIDDEN
    );
    let (_, deniedWebSocketError) = parseJsonResponse(deniedWebSocketResponse).await;
    assert_eq!(
        deniedWebSocketError["message"],
        "本機控制服務不允許此請求 Origin"
    );

    let allowedWebSocket = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/events")
        .header("origin", "http://tauri.localhost")
        .body(Body::empty())
        .expect("构建允许 Origin WebSocket 请求");
    let allowedWebSocketResponse = router
        .oneshot(allowedWebSocket)
        .await
        .expect("执行允许 Origin WebSocket 请求");
    assert_ne!(
        allowedWebSocketResponse.status(),
        axum::http::StatusCode::FORBIDDEN
    );
}

/// 验证关闭标记发布后新订阅方仍能立即观察到记忆状态。
#[tokio::test]
async fn lateShutdownSubscriberReadsPublishedStateImmediately() {
    let state = newControlState().await;
    state.beginShutdown();
    let receiver = state.subscribeShutdown();
    assert!(*receiver.borrow());
}
