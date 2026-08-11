#![allow(non_snake_case)]

use std::{
    net::{IpAddr, Ipv4Addr},
    time::{Duration, Instant},
};

use capture_core::{RecordingConfiguration, RecordingSession};
use http_proxy_core::{
    AuxiliaryListenerConfiguration, HttpProxyConfig, PortForwardEntry, SslMitmManager,
    ToolPipeline, startAuxiliaryListeners,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

/// 创建隔离录制会话，端口转发本身不写 HTTP 事务，但辅助监听器初始化仍使用统一运行时依赖。
async fn createRecording(directory: &TempDir) -> RecordingSession {
    RecordingSession::new(RecordingConfiguration {
        spillDirectory: directory.path().join("capture"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("测试录制会话必须创建成功")
}

/// 构造端口零的 HTTP 配置以验证辅助监听器不依赖主 HTTP 监听端口；所有超时使用测试可控的短边界。
fn testHttpConfiguration() -> HttpProxyConfig {
    HttpProxyConfig {
        listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
        listenPort: 0,
        connectTimeoutMilliseconds: 500,
        requestTimeoutMilliseconds: 1_000,
        shutdownTimeoutMilliseconds: 1_000,
        ..HttpProxyConfig::default()
    }
}

/// 验证 TCP 端口转发可绑定、双向复制并在 stop 后释放端口，测试上游和客户端均为本机临时监听器。
#[tokio::test]
async fn portForwardRelaysBytesAndReleasesBinding() {
    let directory = tempfile::tempdir().expect("临时目录必须创建");
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("回显上游必须绑定");
    let upstreamAddress = upstream.local_addr().expect("回显上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.expect("回显上游必须接纳连接");
        let mut payload = [0_u8; 4];
        stream
            .read_exact(&mut payload)
            .await
            .expect("回显上游必须读取完整负载");
        stream
            .write_all(&payload)
            .await
            .expect("回显上游必须回写负载");
    });
    let reserve = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("转发端口必须预留");
    let forwardAddress = reserve.local_addr().expect("转发地址必须可读");
    drop(reserve);
    let certificateDirectory = tempfile::tempdir().expect("临时证书目录必须创建");
    let listeners = startAuxiliaryListeners(
        AuxiliaryListenerConfiguration {
            reverseProxies: Vec::new(),
            portForwards: vec![PortForwardEntry {
                id: "echo".to_owned(),
                enabled: true,
                listenHost: Ipv4Addr::LOCALHOST.to_string(),
                listenPort: forwardAddress.port(),
                targetHost: upstreamAddress.ip().to_string(),
                targetPort: upstreamAddress.port(),
            }],
        },
        testHttpConfiguration(),
        createRecording(&directory).await,
        SslMitmManager::load(certificateDirectory.path()).expect("测试 SSL 管理器必须初始化"),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("端口转发必须启动");
    assert_eq!(
        listeners.bindings().portForwards[0].boundEndpoint,
        forwardAddress.to_string()
    );

    let mut client = TcpStream::connect(forwardAddress)
        .await
        .expect("客户端必须连入转发端口");
    client
        .write_all(b"ping")
        .await
        .expect("客户端必须写入转发负载");
    let mut echoed = [0_u8; 4];
    client
        .read_exact(&mut echoed)
        .await
        .expect("客户端必须收到回显负载");
    assert_eq!(&echoed, b"ping");
    drop(client);
    listeners.stop().await.expect("停止转发必须完成");
    upstreamTask.await.expect("回显任务必须完成");
    assert!(
        TcpListener::bind(forwardAddress).await.is_ok(),
        "停止后原端口必须可重新绑定"
    );
}

/// 端口转发两端都保持长连接时，stop 必须强制析构套接字而不是等待复制任务自然结束。
#[tokio::test]
async fn portForwardStopForceClosesStalledSockets() {
    let directory = tempfile::tempdir().expect("临时目录必须创建");
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("悬挂上游必须绑定");
    let upstreamAddress = upstream.local_addr().expect("悬挂上游地址必须可读");
    let reserve = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("转发端口必须预留");
    let forwardAddress = reserve.local_addr().expect("转发地址必须可读");
    drop(reserve);
    let certificateDirectory = tempfile::tempdir().expect("临时证书目录必须创建");
    let listeners = startAuxiliaryListeners(
        AuxiliaryListenerConfiguration {
            reverseProxies: Vec::new(),
            portForwards: vec![PortForwardEntry {
                id: "stalled".to_owned(),
                enabled: true,
                listenHost: Ipv4Addr::LOCALHOST.to_string(),
                listenPort: forwardAddress.port(),
                targetHost: upstreamAddress.ip().to_string(),
                targetPort: upstreamAddress.port(),
            }],
        },
        testHttpConfiguration(),
        createRecording(&directory).await,
        SslMitmManager::load(certificateDirectory.path()).expect("测试 SSL 管理器必须初始化"),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("悬挂转发监听必须启动");
    let mut client = TcpStream::connect(forwardAddress)
        .await
        .expect("悬挂客户端必须连接");
    let (mut upstreamStream, _) = timeout(Duration::from_millis(500), upstream.accept())
        .await
        .expect("转发器必须及时连接上游")
        .expect("悬挂上游必须接纳连接");

    let startedAt = Instant::now();
    timeout(Duration::from_millis(500), listeners.stop())
        .await
        .expect("停止不得等待双向复制结束")
        .expect("强制停止必须成功");
    assert!(startedAt.elapsed() < Duration::from_millis(500));
    for stream in [&mut client, &mut upstreamStream] {
        let mut byte = [0_u8; 1];
        let result = timeout(Duration::from_millis(500), stream.read(&mut byte))
            .await
            .expect("套接字必须立即观察到关闭");
        let connectionClosed = match result {
            Ok(0) => true,
            Err(error) => matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ),
            Ok(_) => false,
        };
        assert!(connectionClosed);
    }
    let rebound = TcpListener::bind(forwardAddress)
        .await
        .expect("强制停止后端口必须立即可重绑");
    drop(rebound);
}
