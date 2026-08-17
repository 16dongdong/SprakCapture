#![allow(non_snake_case)]

use std::net::{IpAddr, Ipv4Addr};

use capture_core::{RecordingConfiguration, RecordingSession, TransactionStatus};
use http_proxy_core::{
    HttpProxyConfig, SocksHttpTarget, SocksHttpTunnelHandler, SslMitmManager, ToolPipeline,
};
use plugin_host::PluginHost;
use tokio::{
    io::{AsyncWriteExt, duplex},
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

/// 创建完全隔离的 SOCKS HTTP 处理器与录制会话；证书和正文只写入测试指定的 D 盘临时根。
/// 失败语义：任一运行时组件初始化失败会立即终止测试，不允许退回用户配置或系统临时目录。
async fn testHandler() -> (SocksHttpTunnelHandler, RecordingSession, tempfile::TempDir) {
    let certificateDirectory = tempfile::tempdir().expect("测试证书目录必须创建");
    let recordingDirectory = tempfile::tempdir().expect("测试录制目录必须创建");
    let recording = RecordingSession::new(RecordingConfiguration {
        spillDirectory: recordingDirectory.path().to_path_buf(),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("测试录制会话必须创建");
    let ssl = SslMitmManager::load(certificateDirectory.path()).expect("测试证书必须初始化");
    let handler = SocksHttpTunnelHandler::new(
        HttpProxyConfig {
            listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
            listenPort: 0,
            connectTimeoutMilliseconds: 250,
            ..HttpProxyConfig::default()
        },
        recording.clone(),
        ssl,
        ToolPipeline::new(),
        PluginHost::disabled(),
    )
    .expect("SOCKS HTTP 处理器必须初始化");
    (handler, recording, recordingDirectory)
}

/// 构造无固定路由的已验证 SOCKS 目标；测试只关心连接接管后的失败可见性。
fn target(host: &str, port: u16) -> SocksHttpTarget {
    SocksHttpTarget {
        host: host.to_owned(),
        port,
        fixedAddress: None,
        clientProcessName: Some("test-client".to_owned()),
        clientProcessId: Some(42),
    }
}

/// 读取唯一失败事务；截止时间确保异步录制不稳定时测试明确失败而不是无限等待。
async fn failedTransaction(recording: &RecordingSession) -> capture_core::TransactionSummary {
    timeout(Duration::from_secs(1), async {
        loop {
            let transactions = recording.listMetadata().await.expect("事务列表必须可读");
            if let Some(transaction) = transactions
                .into_iter()
                .find(|transaction| transaction.status == TransactionStatus::Failed)
            {
                return transaction;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("失败事务必须在截止时间内可见")
}

/// 验证客户端拒绝动态证书时仍生成明确的 TLS 握手失败事务，而不是只增加内部统计后从事务树消失。
#[tokio::test]
async fn tlsHandshakeFailureCreatesVisibleTransaction() {
    let (handler, recording, _recordingDirectory) = testHandler().await;
    let (mut client, server) = duplex(4 * 1024);
    let task = tokio::spawn(async move {
        handler
            .serveInterceptedHttps(
                server,
                "127.0.0.1:50000".parse().unwrap(),
                target("handshake.example", 443),
                CancellationToken::new(),
            )
            .await
    });

    // TLS fatal certificate_unknown 警报复现真实客户端不信任代理根证书的握手失败。
    client
        .write_all(&[0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x2e])
        .await
        .expect("TLS 警报必须写入");
    client.shutdown().await.expect("测试客户端必须关闭");
    assert!(task.await.expect("处理任务必须退出").is_err());

    let transaction = failedTransaction(&recording).await;
    assert_eq!(transaction.urlDisplay, "https://handshake.example:443");
    assert_eq!(
        transaction.clientProcessName.as_deref(),
        Some("test-client")
    );
    assert_eq!(transaction.clientProcessId, Some(42));
    assert_eq!(
        transaction.error.as_ref().map(|error| error.code.as_str()),
        Some("sslDownstreamHandshakeFailed")
    );
}

/// 验证 Hyper 在生成 Request 前拒绝畸形首行时也创建失败事务，覆盖同类“有目标但没有请求对象”隐藏路径。
#[tokio::test]
async fn malformedHttpBeforeRequestCreatesVisibleTransaction() {
    let (handler, recording, _recordingDirectory) = testHandler().await;
    let (mut client, server) = duplex(4 * 1024);
    let task = tokio::spawn(async move {
        handler
            .servePlainHttp(
                server,
                "127.0.0.1:50001".parse().unwrap(),
                target("plain.example", 80),
                CancellationToken::new(),
            )
            .await
    });

    client
        .write_all(b"INVALID REQUEST\r\n\r\n")
        .await
        .expect("畸形请求必须写入");
    client.shutdown().await.expect("测试客户端必须关闭");
    assert!(task.await.expect("处理任务必须退出").is_err());

    let transaction = failedTransaction(&recording).await;
    assert_eq!(transaction.urlDisplay, "http://plain.example:80");
    assert_eq!(
        transaction.error.as_ref().map(|error| error.code.as_str()),
        Some("httpProxyInvalidRequest")
    );
}
