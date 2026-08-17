#![allow(non_snake_case)]

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use capture_core::{RecordingConfiguration, RecordingSession, TransactionProtocol};
use http_proxy_core::{HttpProxyConfig, SslMitmManager, ToolPipeline};
use plugin_host::PluginHost;
use proxy_backend::socksHttpInspection::SocksHttpInspector;
use socks5_core::{
    SessionApplicationProtocol, Socks5Config,
    address::readTargetAddress,
    interception::{TcpTunnel, TcpTunnelDisposition, TcpTunnelInterceptor},
    startSocks5ServerWithInterception,
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// 启动只服务一次有效 HTTP 请求的本地上游；SOCKS5 分类前建立并关闭的预连接不计为有效请求。
///
/// 运行上下文：用于验证 SOCKS5 CONNECT 的 HTTP 接管会重新进入统一 HTTP 转发链，而不是把 GET 写进原始中继。
/// 参数：无；返回上游地址与任务句柄，调用方必须在验收后等待任务结束。
/// 失败语义：监听、读取或写入失败会让测试任务提前结束，外层断言会报告响应或事务缺失。
async fn startHttpUpstream() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("HTTP 上游监听必须成功");
    let address = listener.local_addr().expect("HTTP 上游地址必须可读取");
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1_024];
            loop {
                let Ok(byteCount) = stream.read(&mut buffer).await else {
                    return;
                };
                if byteCount == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..byteCount]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            if request.is_empty() {
                continue;
            }
            assert!(
                std::str::from_utf8(&request)
                    .expect("HTTP 请求必须为 UTF-8 头部")
                    .starts_with("GET /fixture HTTP/1.1")
            );
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                )
                .await
                .expect("HTTP 上游响应必须可写入");
            return;
        }
    });
    (address, task)
}

/// 完成 SOCKS5 无认证协商并建立到指定 IPv4 端点的 CONNECT 隧道。
///
/// 运行上下文：端到端测试使用最小 RFC 1928 帧，避免客户端 SDK 行为掩盖核心分类结果。
/// 参数：`stream` 为已连接 SOCKS5 套接字，`target` 为本地 HTTP 上游地址。
/// 失败语义：任一协商或响应字段不符合成功帧时立即失败，测试不会继续发送 HTTP 请求。
async fn establishConnectTunnel(stream: &mut TcpStream, target: SocketAddr) {
    stream
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .expect("SOCKS5 无认证协商必须可写入");
    let mut authenticationReply = [0_u8; 2];
    stream
        .read_exact(&mut authenticationReply)
        .await
        .expect("SOCKS5 无认证响应必须可读取");
    assert_eq!(authenticationReply, [0x05, 0x00]);
    let IpAddr::V4(host) = target.ip() else {
        panic!("测试上游必须使用 IPv4");
    };
    let mut request = vec![0x05, 0x01, 0x00, 0x01];
    request.extend_from_slice(&host.octets());
    request.extend_from_slice(&target.port().to_be_bytes());
    stream
        .write_all(&request)
        .await
        .expect("SOCKS5 CONNECT 必须可写入");
    let mut responsePrefix = [0_u8; 3];
    stream
        .read_exact(&mut responsePrefix)
        .await
        .expect("SOCKS5 CONNECT 响应必须可读取");
    assert_eq!(responsePrefix, [0x05, 0x00, 0x00]);
    readTargetAddress(stream)
        .await
        .expect("SOCKS5 CONNECT 绑定端点必须可读取");
}

/// 验证 SOCKS5 CONNECT 中的匹配 HTTP/1.1 请求生成 HTTP 事务，而不是 TCP 原始流事务。
#[tokio::test]
async fn socksHttpRequestUsesHttpTransactionPipeline() {
    timeout(TEST_TIMEOUT, async {
        let certificateDirectory = tempdir().expect("测试证书目录必须创建");
        let recording = RecordingSession::new(RecordingConfiguration::default())
            .await
            .expect("测试录制会话必须创建");
        let ssl =
            SslMitmManager::load(certificateDirectory.path()).expect("测试 SSL 管理器必须初始化");
        let inspector = SocksHttpInspector::new(
            HttpProxyConfig::default(),
            recording.clone(),
            ssl,
            ToolPipeline::new(),
            PluginHost::disabled(),
        )
        .expect("SOCKS5 HTTP 分类器必须初始化");
        let socksConfiguration = Socks5Config {
            listenPort: 0,
            connectTimeoutMilliseconds: 2_000,
            readTimeoutMilliseconds: 2_000,
            idleTimeoutMilliseconds: 2_000,
            shutdownTimeoutMilliseconds: 2_000,
            ..Socks5Config::default()
        };
        let socksServer = startSocks5ServerWithInterception(
            socksConfiguration,
            PluginHost::disabled(),
            Some(Arc::new(inspector)),
        )
        .await
        .expect("SOCKS5 服务必须启动");
        let (upstreamAddress, upstreamTask) = startHttpUpstream().await;
        let mut client = TcpStream::connect(socksServer.boundAddress())
            .await
            .expect("SOCKS5 客户端必须连接");
        establishConnectTunnel(&mut client, upstreamAddress).await;
        client
            .write_all(
                format!(
                    "GET /fixture HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
                    upstreamAddress.port()
                )
                .as_bytes(),
            )
            .await
            .expect("SOCKS5 隧道内 HTTP 请求必须可写入");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("SOCKS5 隧道内 HTTP 响应必须可读取");
        assert!(response.ends_with(b"\r\n\r\nok"));
        upstreamTask.await.expect("HTTP 上游任务必须结束");
        let page = recording
            .pageView(None, 10, None)
            .await
            .expect("HTTP 事务页必须可读取");
        assert_eq!(page.total, 1);
        assert_eq!(page.transactions[0].protocol, TransactionProtocol::Http);
        assert_eq!(page.transactions[0].method, "GET");
        assert_eq!(page.transactions[0].host, "127.0.0.1");
        assert!(socksServer.stop().await.errorMessage.is_none());
    })
    .await
    .expect("SOCKS5 HTTP 端到端测试超时");
}

/// 创建一对本地 TCP 套接字，供透明协议探测测试验证 `peek` 不消费客户端首段。
async fn createTcpPair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("透明探测监听必须成功");
    let client = TcpStream::connect(listener.local_addr().expect("监听地址必须可读"));
    let accepted = listener.accept();
    let (clientResult, acceptedResult) = tokio::join!(client, accepted);
    (
        clientResult.expect("透明探测客户端必须连接"),
        acceptedResult.expect("透明探测服务端必须接收").0,
    )
}

/// 创建仅用于透明主机名恢复的分类器，证书与录制状态均使用隔离夹具。
async fn createTransparentInspector() -> SocksHttpInspector {
    let certificateDirectory = tempdir().expect("透明探测证书目录必须创建");
    let recording = RecordingSession::new(RecordingConfiguration::default())
        .await
        .expect("透明探测录制会话必须创建");
    let ssl =
        SslMitmManager::load(certificateDirectory.path()).expect("透明探测 SSL 管理器必须初始化");
    SocksHttpInspector::new(
        HttpProxyConfig::default(),
        recording,
        ssl,
        ToolPipeline::new(),
        PluginHost::disabled(),
    )
    .expect("透明探测分类器必须初始化")
}

/// 验证透明 HTTP 请求可从唯一 Host 恢复域名，同时原始请求字节仍留在套接字中。
#[tokio::test]
async fn transparentHttpHostRestoresLogicalHostWithoutDnsEquality() {
    let inspector = createTransparentInspector().await;
    let (mut client, mut server) = createTcpPair().await;
    let request = b"GET / HTTP/1.1\r\nHost: cdn-view.fixture.invalid\r\nConnection: close\r\n\r\n";
    client.write_all(request).await.expect("HTTP 首段必须写入");

    let host = inspector
        .resolveTransparentHost(
            &server,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &CancellationToken::new(),
        )
        .await
        .expect("HTTP 透明主机名探测必须成功");
    assert_eq!(host, "cdn-view.fixture.invalid");
    let mut preserved = vec![0_u8; request.len()];
    server
        .read_exact(&mut preserved)
        .await
        .expect("探测后原始 HTTP 首段必须仍可读取");
    assert_eq!(preserved, request);
}

/// 组装包含单个 SNI 扩展的最小 ClientHello，严格使用 TLS 网络字节序长度字段。
fn buildClientHello(serverName: &str) -> Vec<u8> {
    let name = serverName.as_bytes();
    let mut serverNameList = Vec::new();
    serverNameList.extend_from_slice(
        &u16::try_from(name.len() + 3)
            .expect("SNI 列表长度必须有效")
            .to_be_bytes(),
    );
    serverNameList.push(0);
    serverNameList.extend_from_slice(
        &u16::try_from(name.len())
            .expect("SNI 名称长度必须有效")
            .to_be_bytes(),
    );
    serverNameList.extend_from_slice(name);
    let mut extensions = vec![0, 0];
    extensions.extend_from_slice(
        &u16::try_from(serverNameList.len())
            .expect("SNI 扩展长度必须有效")
            .to_be_bytes(),
    );
    extensions.extend_from_slice(&serverNameList);
    let mut hello = vec![0x03, 0x03];
    hello.extend_from_slice(&[0_u8; 32]);
    hello.push(0);
    hello.extend_from_slice(&[0, 2, 0x13, 0x01]);
    hello.extend_from_slice(&[1, 0]);
    hello.extend_from_slice(
        &u16::try_from(extensions.len())
            .expect("扩展总长度必须有效")
            .to_be_bytes(),
    );
    hello.extend_from_slice(&extensions);
    let handshakeLength = u32::try_from(hello.len())
        .expect("握手长度必须有效")
        .to_be_bytes();
    let mut handshake = vec![
        1,
        handshakeLength[1],
        handshakeLength[2],
        handshakeLength[3],
    ];
    handshake.extend_from_slice(&hello);
    let mut record = vec![0x16, 0x03, 0x03];
    record.extend_from_slice(
        &u16::try_from(handshake.len())
            .expect("TLS 记录长度必须有效")
            .to_be_bytes(),
    );
    record.extend_from_slice(&handshake);
    record
}

/// 验证透明 TLS 连接可从完整 ClientHello 恢复 SNI，且域名不依赖当前 DNS 解析结果。
#[tokio::test]
async fn transparentTlsSniRestoresLogicalHostWithoutDnsEquality() {
    let inspector = createTransparentInspector().await;
    let (mut client, server) = createTcpPair().await;
    client
        .write_all(&buildClientHello("tls-view.fixture.invalid"))
        .await
        .expect("TLS ClientHello 必须写入");

    let host = inspector
        .resolveTransparentHost(
            &server,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &CancellationToken::new(),
        )
        .await
        .expect("TLS 透明主机名探测必须成功");
    assert_eq!(host, "tls-view.fixture.invalid");
}

/// 验证透明 GET 在延迟发送、请求头超过旧探测上限且 Host 与本机 DNS 视图不一致时仍进入 HTTP 处理链。
///
/// 运行上下文：用固定本地地址模拟 WinDivert 原目标，逻辑 Host 使用不可解析夹具域名，确保测试不依赖外部 DNS。
/// 失败语义：若分类仍使用 100ms、16KiB 或 DNS 等值限制，响应读取或协议归属断言会失败。
#[tokio::test]
async fn delayedLargeTransparentGetUsesHttpPipelineAndLogicalHost() {
    timeout(TEST_TIMEOUT, async {
        let certificateDirectory = tempdir().expect("测试证书目录必须创建");
        let recording = RecordingSession::new(RecordingConfiguration::default())
            .await
            .expect("测试录制会话必须创建");
        let ssl =
            SslMitmManager::load(certificateDirectory.path()).expect("测试 SSL 管理器必须初始化");
        let inspector = SocksHttpInspector::new(
            HttpProxyConfig::default(),
            recording.clone(),
            ssl,
            ToolPipeline::new(),
            PluginHost::disabled(),
        )
        .expect("透明 HTTP 分类器必须初始化");
        let (upstreamAddress, upstreamTask) = startHttpUpstream().await;
        let remoteStream = TcpStream::connect(upstreamAddress)
            .await
            .expect("透明预连接必须建立");
        let (mut applicationClient, proxyClient) = createTcpPair().await;
        let clientAddress = proxyClient.peer_addr().expect("客户端地址必须可读");
        let tunnel = TcpTunnel {
            clientStream: proxyClient,
            remoteStream,
            clientAddress,
            clientProcessName: Some("fixtureClient.exe".to_owned()),
            clientProcessId: Some(42_424),
            targetHost: upstreamAddress.ip().to_string(),
            connectHost: upstreamAddress.ip().to_string(),
            routePinned: true,
            targetPort: upstreamAddress.port(),
            cancellation: CancellationToken::new(),
            accountLease: None,
        };
        let inspectionTask = tokio::spawn(async move { inspector.intercept(tunnel).await });
        tokio::time::sleep(Duration::from_millis(250)).await;
        let padding = "a".repeat(20 * 1024);
        let request = format!(
            "GET /fixture HTTP/1.1\r\nHost: delayed.fixture.invalid:{}\r\nX-Padding: {padding}\r\nConnection: close\r\n\r\n",
            upstreamAddress.port()
        );
        applicationClient
            .write_all(request.as_bytes())
            .await
            .expect("延迟大请求头必须可写入");
        let mut response = Vec::new();
        applicationClient
            .read_to_end(&mut response)
            .await
            .expect("HTTP 响应必须可读取");
        assert!(response.ends_with(b"\r\n\r\nok"));
        assert!(matches!(
            inspectionTask
                .await
                .expect("分类任务必须结束")
                .expect("透明 HTTP 接管必须成功"),
            TcpTunnelDisposition::Handled(SessionApplicationProtocol::Http)
        ));
        upstreamTask.await.expect("HTTP 上游任务必须结束");
        let page = recording
            .pageView(None, 10, None)
            .await
            .expect("HTTP 事务页必须可读取");
        assert_eq!(page.total, 1);
        assert_eq!(page.transactions[0].protocol, TransactionProtocol::Http);
        assert_eq!(page.transactions[0].host, "delayed.fixture.invalid");
        assert_eq!(page.transactions[0].method, "GET");
    })
    .await
    .expect("延迟透明 HTTP 回归测试超时");
}

/// 验证延迟到达的完整 ClientHello 仍被识别为 TLS，并把 SNI 写回透明连接的逻辑目标。
///
/// 运行上下文：默认没有 SSL 解密规则，因此分类结果应保留原始字节流但明确声明 TLS，而不是退化为 TCP。
/// 失败语义：超时过短、SNI 解析失败或域名仍被 IP 覆盖都会使协议或目标域名断言失败。
#[tokio::test]
async fn delayedTransparentClientHelloUsesTlsAndLogicalHost() {
    timeout(TEST_TIMEOUT, async {
        let inspector = createTransparentInspector().await;
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("TLS 上游监听必须成功");
        let upstreamAddress = upstream.local_addr().expect("TLS 上游地址必须可读");
        let remoteConnection = TcpStream::connect(upstreamAddress);
        let acceptedConnection = upstream.accept();
        let (remoteResult, acceptedResult) = tokio::join!(remoteConnection, acceptedConnection);
        let remoteStream = remoteResult.expect("TLS 预连接必须建立");
        let _upstreamStream = acceptedResult.expect("TLS 上游必须接收预连接").0;
        let (mut applicationClient, proxyClient) = createTcpPair().await;
        let tunnel = TcpTunnel {
            clientAddress: proxyClient.peer_addr().expect("客户端地址必须可读"),
            clientStream: proxyClient,
            remoteStream,
            clientProcessName: Some("fixtureClient.exe".to_owned()),
            clientProcessId: Some(42_424),
            targetHost: upstreamAddress.ip().to_string(),
            connectHost: upstreamAddress.ip().to_string(),
            routePinned: true,
            targetPort: 443,
            cancellation: CancellationToken::new(),
            accountLease: None,
        };
        let inspectionTask = tokio::spawn(async move { inspector.intercept(tunnel).await });
        tokio::time::sleep(Duration::from_millis(250)).await;
        applicationClient
            .write_all(&buildClientHello("delayed-tls.fixture.invalid"))
            .await
            .expect("延迟 ClientHello 必须可写入");
        let disposition = inspectionTask
            .await
            .expect("TLS 分类任务必须结束")
            .expect("TLS 分类必须成功");
        let TcpTunnelDisposition::Raw {
            tunnel,
            applicationProtocol,
        } = disposition
        else {
            panic!("未配置解密规则的 TLS 必须保留原始中继");
        };
        assert_eq!(applicationProtocol, SessionApplicationProtocol::Tls);
        assert_eq!(tunnel.targetHost, "delayed-tls.fixture.invalid");
    })
    .await
    .expect("延迟透明 TLS 回归测试超时");
}
