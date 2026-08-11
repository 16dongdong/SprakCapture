#![allow(non_snake_case)]

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::Duration,
};

use capture_core::{RecordingConfiguration, RecordingSession, TransactionStatus};
use http_proxy_core::{
    HttpProxyConfig, SslMitmManager, ThrottleProfile, ThrottlingConfiguration, ThrottlingTool,
    ToolPipeline, startHttpProxy,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

/// 创建独立捕获会话，确保节流端到端测试不会读取或污染其他代理测试的正文存储。
async fn createCapture(directory: &TempDir) -> RecordingSession {
    RecordingSession::new(RecordingConfiguration {
        spillDirectory: directory.path().join("capture"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("节流测试捕获会话必须创建成功")
}

/// 为每个端到端测试构造临时证书管理器；本测试仅走 HTTP，但监听器启动仍需要完整代理依赖。
fn createSsl() -> SslMitmManager {
    let directory = tempfile::tempdir().expect("临时证书目录必须创建成功");
    SslMitmManager::load(directory.path()).expect("测试证书管理器必须初始化")
}

/// 返回不会截断低速响应的代理配置；请求超时只覆盖等待上游响应头，不覆盖节流后的下行复制。
fn createProxyConfig() -> HttpProxyConfig {
    HttpProxyConfig {
        listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
        listenPort: 0,
        connectTimeoutMilliseconds: 2_000,
        requestTimeoutMilliseconds: 10_000,
        shutdownTimeoutMilliseconds: 2_000,
        ..HttpProxyConfig::default()
    }
}

/// 读取完整 HTTP 请求头后返回，避免 TCP 分段使上游测试服务在收到不完整请求时提前写回响应。
async fn readRequestHeaders(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let bytes = stream.read(&mut buffer).await.expect("上游请求必须可读");
        assert_ne!(bytes, 0, "请求头未完成时上游连接不应关闭");
        request.extend_from_slice(&buffer[..bytes]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
}

/// 等待指定数量的完整事务，确保正文泵结束、capture 提交和 flags 写入均已对断言可见。
async fn waitForCompletedTransactions(
    capture: &RecordingSession,
    expectedCount: usize,
) -> Vec<capture_core::TransactionSummary> {
    timeout(Duration::from_secs(3), async {
        loop {
            let transactions = capture.listMetadata().await.expect("事务列表必须可读");
            if transactions.len() >= expectedCount
                && transactions
                    .iter()
                    .take(expectedCount)
                    .all(|transaction| transaction.status == TransactionStatus::Complete)
            {
                return transactions;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("节流事务必须在超时前完成")
}

/// 启动两次响应同一大正文的本地上游；两次请求分别用于验证开启节流和热更新关闭后的可观察差异。
async fn startUpstream(body: Arc<Vec<u8>>) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("上游监听器必须绑定成功");
    let address = listener.local_addr().expect("上游地址必须可读");
    let task = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.expect("上游必须接受请求");
            readRequestHeaders(&mut stream).await;
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("上游响应头必须写入");
            stream.write_all(&body).await.expect("上游响应正文必须写入");
        }
    });
    (address, task)
}

/// 真实代理下行必须按令牌桶限速，热更新关闭后同一上游正文立即恢复；两次事务的 capture flags 也必须反映实际命中状态。
#[tokio::test]
async fn throttlingSlowsRealDownloadAndHotDisableRestoresThroughput() {
    let body = Arc::new(vec![0x5A; 1_024]);
    let (upstreamAddress, upstreamTask) = startUpstream(body.clone()).await;
    let captureDirectory = tempfile::tempdir().expect("捕获目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let throttle = Arc::new(
        ThrottlingTool::new(ThrottlingConfiguration {
            enabled: true,
            activePresetId: None,
            custom: ThrottleProfile {
                downloadBytesPerSecond: 1_024,
                uploadBytesPerSecond: 1_024,
                latencyMilliseconds: 0,
                latencyJitterMilliseconds: 0,
                reliabilityPercent: 100,
                mtu: 64,
            },
            locations: Vec::new(),
            userPresets: Vec::new(),
        })
        .expect("低速节流配置必须有效"),
    );
    let pipeline = ToolPipeline::new();
    pipeline
        .register(throttle.clone())
        .expect("节流工具必须注册成功");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://{}", proxy.boundAddress()))
                .expect("代理地址必须有效"),
        )
        .build()
        .expect("测试客户端必须创建成功");
    let url = format!("http://{upstreamAddress}/payload.bin");

    let slowStartedAt = Instant::now();
    let slowResponse = client.get(&url).send().await.expect("节流响应必须返回");
    let slowBody = slowResponse.bytes().await.expect("节流正文必须可读");
    let slowElapsed = slowStartedAt.elapsed();
    assert_eq!(slowBody.as_ref(), body.as_slice());
    assert!(
        slowElapsed >= Duration::from_millis(650),
        "1024 B/s、64 B MTU 的 1024 B 下行必须产生可观察限速"
    );

    throttle
        .updateConfiguration(ThrottlingConfiguration::default())
        .expect("关闭节流的热更新必须成功");
    let fastStartedAt = Instant::now();
    let fastResponse = client.get(&url).send().await.expect("关闭后响应必须返回");
    let fastBody = fastResponse.bytes().await.expect("关闭后正文必须可读");
    let fastElapsed = fastStartedAt.elapsed();
    assert_eq!(fastBody.as_ref(), body.as_slice());
    assert!(
        slowElapsed > fastElapsed + Duration::from_millis(400),
        "关闭节流后吞吐必须明显恢复"
    );

    let transactions = waitForCompletedTransactions(&capture, 2).await;
    assert!(transactions[0].flags.throttled);
    assert!(!transactions[1].flags.throttled);
    assert!(
        transactions[0]
            .appliedTools
            .iter()
            .any(|tool| tool == "throttling")
    );
    assert!(
        !transactions[1]
            .appliedTools
            .iter()
            .any(|tool| tool == "throttling")
    );

    upstreamTask.await.expect("上游任务必须完成");
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}
