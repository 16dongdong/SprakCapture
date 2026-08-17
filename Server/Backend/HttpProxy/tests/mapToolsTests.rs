#![allow(non_snake_case)]

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use capture_core::{RecordingConfiguration, RecordingSession, TransactionStatus};
use http_proxy_core::{
    HttpProxyConfig, SslMitmManager, ToolPipeline, startHttpProxy,
    tools::{
        MapLocalConfiguration, MapLocalRule, MapLocalTool, MapRemoteConfiguration, MapRemoteRule,
        MapRemoteTarget, MapRemoteTool,
    },
};
use location_core::LocationPattern;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

/// 使用独立临时目录创建录制会话，避免端到端映射测试和其他代理测试共享 spill 正文。
async fn createCapture(directory: &TempDir) -> RecordingSession {
    RecordingSession::new(RecordingConfiguration {
        spillDirectory: directory.path().to_path_buf(),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("测试录制会话必须创建成功")
}

/// 为每个测试创建隔离根证书目录；这两个 HTTP 测试不进入 TLS 解密链路。
fn testSsl() -> SslMitmManager {
    let directory = tempfile::tempdir().expect("临时证书目录必须创建");
    SslMitmManager::load(directory.path()).expect("测试 SSL 管理器必须初始化")
}

/// 使用端口零避免并行测试争用监听端口，并缩短失败路径等待时间。
fn testConfig() -> HttpProxyConfig {
    HttpProxyConfig {
        listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
        listenPort: 0,
        connectTimeoutMilliseconds: 500,
        requestTimeoutMilliseconds: 1_000,
        shutdownTimeoutMilliseconds: 1_000,
        ..HttpProxyConfig::default()
    }
}

/// 在截止时间内等待首条 complete 事务，确保异步响应泵和 capture 提交已经完成。
async fn completeTransaction(capture: &RecordingSession) -> capture_core::TransactionSummary {
    timeout(Duration::from_secs(1), async {
        loop {
            let transactions = capture.listMetadata().await.expect("事务列表必须可读");
            if let Some(transaction) = transactions
                .into_iter()
                .find(|transaction| transaction.status == TransactionStatus::Complete)
            {
                return transaction;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("事务必须在截止时间内完成")
}

/// 启动只计数接受次数的本地监听器；Map Local 命中时该监听器绝不能收到代理出站连接。
async fn startConnectionCounter() -> (SocketAddr, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("计数上游必须绑定");
    let address = listener.local_addr().expect("计数上游地址必须读取");
    let acceptedConnections = Arc::new(AtomicUsize::new(0));
    let counter = acceptedConnections.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((_stream, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    (address, acceptedConnections, task)
}

/// Map Local 命中后必须直接返回本地正文、记录 mappedLocal，并且不连接原始请求指定的上游端口。
#[tokio::test]
async fn mapLocalShortCircuitsRealProxyTraffic() {
    let (upstreamAddress, acceptedConnections, upstreamTask) = startConnectionCounter().await;
    let mappingDirectory = tempfile::tempdir().expect("映射目录必须创建");
    let localFile = mappingDirectory.path().join("fixture.json");
    tokio::fs::write(&localFile, br#"{"source":"local"}"#)
        .await
        .expect("本地 JSON 夹具必须写入");
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(
            MapLocalTool::new(
                MapLocalConfiguration {
                    enabled: true,
                    rules: vec![MapLocalRule {
                        id: "local-e2e".to_owned(),
                        enabled: true,
                        location: LocationPattern {
                            protocol: "http".to_owned(),
                            host: "127.0.0.1".to_owned(),
                            port: upstreamAddress.port().to_string(),
                            path: "/fixture.json".to_owned(),
                            query: None,
                        },
                        localPath: localFile.to_string_lossy().into_owned(),
                        isDirectory: false,
                        statusCode: 200,
                        responseHeaders: Vec::new(),
                        contentTypeOverride: String::new(),
                    }],
                },
                mappingDirectory.path(),
            )
            .expect("Map Local 配置必须有效"),
        ))
        .expect("Map Local 必须注册");
    let captureDirectory = tempfile::tempdir().expect("录制目录必须创建");
    let capture = createCapture(&captureDirectory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        testSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://{}", proxy.boundAddress()))
                .expect("测试代理地址必须有效"),
        )
        .build()
        .expect("测试客户端必须构建");

    let response = client
        .get(format!("http://{upstreamAddress}/fixture.json"))
        .send()
        .await
        .expect("Map Local 响应必须返回");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.text().await.expect("响应正文必须读取"),
        r#"{"source":"local"}"#
    );
    sleep(Duration::from_millis(50)).await;
    assert_eq!(
        acceptedConnections.load(Ordering::SeqCst),
        0,
        "Map Local 命中时不得建立出站连接"
    );
    let transaction = completeTransaction(&capture).await;
    assert!(transaction.flags.mappedLocal);
    assert!(
        transaction
            .appliedTools
            .iter()
            .any(|tool| tool == "mapLocal:local-e2e")
    );
    assert_eq!(transaction.statusCode, Some(200));
    proxy.stop().await.expect("HTTP 代理必须有序停止");
    upstreamTask.abort();
}

/// Map Remote 必须把真实上游改写到本地服务，在事务摘要记录最终目标，并保留规则级痕迹。
///
/// 运行上下文：真实代理请求以 `source.test` 进入，映射后连接本机随机端口；测试同时验证线上的
/// Host 头、最终事务位置和原子停止。监听、转发、录制或关闭失败时直接终止测试，不接受只改写
/// 请求但事务摘要仍指向原地址的混合状态。
#[tokio::test]
async fn mapRemoteRewritesRealProxyUpstream() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("映射目标上游必须绑定");
    let upstreamAddress = listener.local_addr().expect("上游地址必须读取");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("上游必须接受连接");
        let mut requestBytes = vec![0_u8; 4 * 1024];
        let readBytes = stream
            .read(&mut requestBytes)
            .await
            .expect("上游请求必须读取");
        let request = String::from_utf8_lossy(&requestBytes[..readBytes]);
        assert!(request.starts_with("GET /v2/users HTTP/1.1\r\n"));
        assert!(
            request
                .lines()
                .any(|line| line.eq_ignore_ascii_case(&format!("host: {upstreamAddress}")))
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\nConnection: close\r\n\r\nmapped")
            .await
            .expect("映射上游响应必须写入");
    });
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(
            MapRemoteTool::new(MapRemoteConfiguration {
                enabled: true,
                rules: vec![MapRemoteRule {
                    id: "remote-e2e".to_owned(),
                    enabled: true,
                    r#from: LocationPattern {
                        protocol: "http".to_owned(),
                        host: "source.test".to_owned(),
                        port: "80".to_owned(),
                        path: "/v1/*".to_owned(),
                        query: None,
                    },
                    to: MapRemoteTarget {
                        protocol: "http".to_owned(),
                        host: "127.0.0.1".to_owned(),
                        port: upstreamAddress.port().to_string(),
                        path: "/v2/*".to_owned(),
                    },
                }],
            })
            .expect("Map Remote 配置必须有效"),
        ))
        .expect("Map Remote 必须注册");
    let captureDirectory = tempfile::tempdir().expect("录制目录必须创建");
    let capture = createCapture(&captureDirectory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        testSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://{}", proxy.boundAddress()))
                .expect("测试代理地址必须有效"),
        )
        .build()
        .expect("测试客户端必须构建");

    let response = client
        .get("http://source.test/v1/users")
        .send()
        .await
        .expect("Map Remote 请求必须成功");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await.expect("响应正文必须读取"), "mapped");
    upstreamTask.await.expect("映射上游必须正常完成");
    let transaction = completeTransaction(&capture).await;
    assert!(transaction.flags.mappedRemote);
    // 事务摘要用于解释实际出站目标；原始地址仍由流水线的 originalLocation 独立保留。
    assert_eq!(transaction.host, "127.0.0.1");
    assert!(
        transaction
            .appliedTools
            .iter()
            .any(|tool| tool == "mapRemote:remote-e2e")
    );
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}
