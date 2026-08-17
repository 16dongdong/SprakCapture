#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    collections::HashMap,
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use plugin_host::PluginHost;
use socks5_core::{
    AddressOverride, AuthenticationMode, FusedProxyDependencies, FusedProxyOptions, Socks5Config,
    address::{TargetAddress, TargetHost, encodeTargetAddress, readTargetAddress},
    interception::PortProtocolHandler,
    protocol::{decodeUdpPacket, encodeUdpPacket},
    startFusedProxyServer, startSocks5Server, startSocks5ServerWithInterceptionAndResolver,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const testTimeout: Duration = Duration::from_secs(3);
const mappedTestDomain: &str = "mapped.fixture.test";

/// 将固定测试域名映射到本机，验证 SOCKS5 域名覆盖不会依赖宿主机 DNS。
struct LocalAddressOverride;

impl AddressOverride for LocalAddressOverride {
    /// 仅映射测试域名；其他域名继续使用服务端系统 DNS。
    fn resolveIp(&self, host: &str) -> Option<IpAddr> {
        host.eq_ignore_ascii_case(mappedTestDomain)
            .then_some(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }
}

/// 启动单连接 TCP 回显器并返回地址与任务句柄。
async fn startTcpEcho() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("TCP 回显监听");
    let address = listener.local_addr().expect("TCP 回显地址");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("TCP 回显接受");
        let mut buffer = [0_u8; 1_024];
        while let Ok(byteCount) = stream.read(&mut buffer).await {
            if byteCount == 0 {
                break;
            }
            if stream.write_all(&buffer[..byteCount]).await.is_err() {
                break;
            }
        }
    });
    (address, task)
}

/// 启动 UDP 回显器并返回地址与任务句柄。
async fn startUdpEcho() -> (SocketAddr, JoinHandle<()>) {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("UDP 回显监听");
    let address = socket.local_addr().expect("UDP 回显地址");
    let task = tokio::spawn(async move {
        let mut buffer = [0_u8; 65_507];
        if let Ok((byteCount, peer)) = socket.recv_from(&mut buffer).await {
            let _ = socket.send_to(&buffer[..byteCount], peer).await;
        }
    });
    (address, task)
}

/// 启动 IPv6 UDP 回显器；绑定失败会直接暴露当前平台的双栈支持问题。
async fn startIpv6UdpEcho() -> (SocketAddr, JoinHandle<()>) {
    let socket = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
        .await
        .expect("IPv6 UDP 回显监听");
    let address = socket.local_addr().expect("IPv6 UDP 回显地址");
    let task = tokio::spawn(async move {
        let mut buffer = [0_u8; 65_507];
        if let Ok((byteCount, peer)) = socket.recv_from(&mut buffer).await {
            let _ = socket.send_to(&buffer[..byteCount], peer).await;
        }
    });
    (address, task)
}

/// 完成 NO_AUTH 协商。
async fn negotiateNoAuth(stream: &mut TcpStream) {
    stream
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .expect("写入无认证方法");
    let mut reply = [0_u8; 2];
    stream.read_exact(&mut reply).await.expect("读取方法响应");
    assert_eq!(reply, [0x05, 0x00]);
}

/// 写入使用 IPv4 或 IPv6 字面地址的 SOCKS5 请求；编码器与生产 UDP/TCP 协议路径完全一致。
async fn writeRequest(stream: &mut TcpStream, command: u8, target: SocketAddr) {
    let mut request = vec![0x05, command, 0x00];
    encodeTargetAddress(
        &TargetAddress {
            host: TargetHost::Ip(target.ip()),
            port: target.port(),
        },
        &mut request,
    )
    .expect("编码 SOCKS5 字面地址请求");
    stream.write_all(&request).await.expect("写入 SOCKS5 请求");
}

/// 写入使用域名目标的 SOCKS5 请求，确保测试覆盖服务端解析而非客户端预解析。
async fn writeDomainRequest(stream: &mut TcpStream, command: u8, domain: &str, port: u16) {
    let domainBytes = domain.as_bytes();
    let domainLength = u8::try_from(domainBytes.len()).expect("测试域名长度必须合法");
    let mut request = vec![0x05, command, 0x00, 0x03, domainLength];
    request.extend_from_slice(domainBytes);
    request.extend_from_slice(&port.to_be_bytes());
    stream
        .write_all(&request)
        .await
        .expect("写入 SOCKS5 域名请求");
}

/// 编码使用域名目标的 UDP ASSOCIATE 数据报，避免测试客户端提前解析域名。
fn encodeDomainUdpPacket(domain: &str, port: u16, payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x00, 0x00, 0x00];
    encodeTargetAddress(
        &TargetAddress {
            host: TargetHost::Domain(domain.to_owned()),
            port,
        },
        &mut packet,
    )
    .expect("编码 SOCKS5 UDP 域名地址");
    packet.extend_from_slice(payload);
    packet
}

/// 读取成功响应并返回绑定端点。
async fn readSuccessfulReply(stream: &mut TcpStream) -> SocketAddr {
    let version = stream.read_u8().await.expect("读取响应版本");
    let replyCode = stream.read_u8().await.expect("读取响应码");
    let reserved = stream.read_u8().await.expect("读取保留字节");
    assert_eq!((version, replyCode, reserved), (0x05, 0x00, 0x00));
    let address = readTargetAddress(stream).await.expect("读取绑定地址");
    let TargetAddress {
        host: socks5_core::address::TargetHost::Ip(ip),
        port,
    } = address
    else {
        panic!("绑定响应必须返回 IP 地址");
    };
    SocketAddr::new(ip, port)
}

/// 返回适合回环集成测试的零端口配置。
fn testConfig() -> Socks5Config {
    Socks5Config {
        listenPort: 0,
        connectTimeoutMilliseconds: 2_000,
        bindTimeoutMilliseconds: 2_000,
        idleTimeoutMilliseconds: 2_000,
        shutdownTimeoutMilliseconds: 2_000,
        readTimeoutMilliseconds: 2_000,
        ..Socks5Config::default()
    }
}

/// 记录融合监听是否在首字节分类前交由透明连接处理器接管，并验证停机回调确实执行。
struct ClaimingProtocolHandler {
    observedByte: Arc<AtomicUsize>,
    shutdownCalled: Arc<AtomicBool>,
}

impl PortProtocolHandler for ClaimingProtocolHandler {
    /// 测试夹具声明所有连接均为透明连接，使 `0x05` 不得误入 SOCKS5 协商。
    fn claimsConnection(&self, _stream: &TcpStream, _clientAddress: SocketAddr) -> bool {
        true
    }

    /// 读取未被窥视消费的首字节并记录；读取失败时以哨兵值暴露测试失败。
    fn serve(
        &self,
        mut stream: TcpStream,
        _clientAddress: SocketAddr,
        _cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let observedByte = self.observedByte.clone();
        Box::pin(async move {
            let byte = stream.read_u8().await.map_or(usize::MAX, usize::from);
            observedByte.store(byte, Ordering::SeqCst);
        })
    }

    /// 记录兼容的优雅停止入口；强制停止测试会确保该入口不参与生命周期。
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
        let shutdownCalled = self.shutdownCalled.clone();
        Box::pin(async move {
            shutdownCalled.store(true, Ordering::SeqCst);
            Ok(())
        })
    }

    /// 记录强制停止回调；真实 HTTP 实现会在同一边界中止任务追踪器并析构套接字。
    fn abortAndWait(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let shutdownCalled = self.shutdownCalled.clone();
        Box::pin(async move {
            shutdownCalled.store(true, Ordering::SeqCst);
        })
    }
}

/// 透明连接身份必须优先于载荷首字节，且融合监听停止时必须调用处理器排空入口。
#[tokio::test]
async fn prioritizesClaimedConnectionAndShutsDownProtocolHandler() {
    let observedByte = Arc::new(AtomicUsize::new(usize::MAX));
    let shutdownCalled = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(ClaimingProtocolHandler {
        observedByte: observedByte.clone(),
        shutdownCalled: shutdownCalled.clone(),
    });
    let server = startFusedProxyServer(
        testConfig(),
        FusedProxyDependencies {
            pluginHost: PluginHost::disabled(),
            tunnelInterceptor: None,
            addressOverride: None,
            protocolHandler: Some(handler),
            outboundConnector: None,
        },
        FusedProxyOptions::default(),
    )
    .await
    .expect("融合监听必须启动");
    let mut client = TcpStream::connect(server.boundAddress())
        .await
        .expect("测试客户端必须连接");
    client.write_u8(0x05).await.expect("测试首字节必须写入");
    timeout(testTimeout, async {
        while observedByte.load(Ordering::SeqCst) == usize::MAX {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("透明处理器必须收到首字节");
    assert_eq!(observedByte.load(Ordering::SeqCst), 0x05);
    drop(client);
    assert!(server.stop().await.errorMessage.is_none());
    assert!(shutdownCalled.load(Ordering::SeqCst));
}

/// 仅认领内部回环监听器连接，公开端口继续执行 SOCKS5 协商。
struct InternalCaptureHandler {
    observedByte: Arc<AtomicUsize>,
    servedConnections: Arc<AtomicUsize>,
}

impl PortProtocolHandler for InternalCaptureHandler {
    /// 模拟公开客户端恰好命中透明流表四元组；独立入口边界必须阻止公开连接采用该结果。
    fn claimsConnection(&self, _stream: &TcpStream, _clientAddress: SocketAddr) -> bool {
        true
    }

    /// 读取内部入口的首字节，证明两个监听器共享同一协议处理器。
    fn serve(
        &self,
        mut stream: TcpStream,
        _clientAddress: SocketAddr,
        _cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let observedByte = self.observedByte.clone();
        let servedConnections = self.servedConnections.clone();
        Box::pin(async move {
            let byte = stream.read_u8().await.map_or(usize::MAX, usize::from);
            observedByte.store(byte, Ordering::SeqCst);
            servedConnections.fetch_add(1, Ordering::SeqCst);
        })
    }
}

/// 公开 HTTP/SOCKS 端口与内部透明捕获端口必须隔离，并由一次 stop 同时关闭和排空。
#[tokio::test]
async fn isolatesInternalCaptureListenerAndStopsBothEntrypoints() {
    let observedByte = Arc::new(AtomicUsize::new(usize::MAX));
    let servedConnections = Arc::new(AtomicUsize::new(0));
    let server = startFusedProxyServer(
        testConfig(),
        FusedProxyDependencies {
            pluginHost: PluginHost::disabled(),
            tunnelInterceptor: None,
            addressOverride: None,
            protocolHandler: Some(Arc::new(InternalCaptureHandler {
                observedByte: observedByte.clone(),
                servedConnections: servedConnections.clone(),
            })),
            outboundConnector: None,
        },
        FusedProxyOptions {
            enableInternalCaptureListener: true,
            accountServiceConfig: None,
        },
    )
    .await
    .expect("双入口融合监听必须启动");
    let publicAddress = server.boundAddress();
    let captureProxyAddress = server
        .internalCaptureAddress()
        .expect("内部捕获监听必须存在");
    let captureAddresses = server.internalCaptureAddresses().to_vec();
    assert_eq!(captureAddresses.len(), 2);
    assert!(captureProxyAddress.ip().is_unspecified());
    assert_ne!(publicAddress.port(), captureProxyAddress.port());
    assert!(
        captureAddresses
            .iter()
            .all(|address| address.ip().is_loopback())
    );
    assert!(captureAddresses.iter().any(SocketAddr::is_ipv4));
    assert!(captureAddresses.iter().any(SocketAddr::is_ipv6));
    assert!(
        captureAddresses
            .iter()
            .all(|address| address.port() == captureProxyAddress.port())
    );
    let mut publicClient = TcpStream::connect(publicAddress)
        .await
        .expect("公开 SOCKS5 端口必须连接");
    negotiateNoAuth(&mut publicClient).await;
    drop(publicClient);

    for (index, captureAddress) in captureAddresses.iter().copied().enumerate() {
        let mut captureClient = TcpStream::connect(captureAddress)
            .await
            .expect("双栈内部捕获端口必须连接");
        captureClient
            .write_u8(0x7f)
            .await
            .expect("内部测试字节必须写入");
        timeout(testTimeout, async {
            while servedConnections.load(Ordering::SeqCst) <= index {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("双栈内部处理器必须收到连接");
        assert_eq!(observedByte.load(Ordering::SeqCst), 0x7f);
    }

    assert!(server.stop().await.errorMessage.is_none());
    assert!(TcpStream::connect(publicAddress).await.is_err());
    for captureAddress in captureAddresses {
        assert!(TcpStream::connect(captureAddress).await.is_err());
    }
}

/// 拒绝未登记透明连接，并记录协议处理器是否被错误调用。
struct RejectingInternalHandler {
    serveCalled: Arc<AtomicBool>,
}

impl PortProtocolHandler for RejectingInternalHandler {
    /// 模拟内部连接尚未写入透明流表。
    fn claimsConnection(&self, _stream: &TcpStream, _clientAddress: SocketAddr) -> bool {
        false
    }

    /// 未命中连接不得进入该入口；调用即记录回归。
    fn serve(
        &self,
        _stream: TcpStream,
        _clientAddress: SocketAddr,
        _cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let serveCalled = self.serveCalled.clone();
        Box::pin(async move {
            serveCalled.store(true, Ordering::SeqCst);
        })
    }
}

/// 内部端口未命中流表时必须直接关闭；禁用内部入口时不得额外绑定端点。
#[tokio::test]
async fn rejectsUnclaimedInternalConnectionsAndOmitsDisabledListener() {
    let serveCalled = Arc::new(AtomicBool::new(false));
    let server = startFusedProxyServer(
        testConfig(),
        FusedProxyDependencies {
            pluginHost: PluginHost::disabled(),
            tunnelInterceptor: None,
            addressOverride: None,
            protocolHandler: Some(Arc::new(RejectingInternalHandler {
                serveCalled: serveCalled.clone(),
            })),
            outboundConnector: None,
        },
        FusedProxyOptions {
            enableInternalCaptureListener: true,
            accountServiceConfig: None,
        },
    )
    .await
    .expect("内部监听必须启动");
    let internalAddress = *server
        .internalCaptureAddresses()
        .first()
        .expect("启用内部监听时必须返回真实端点");
    let mut client = TcpStream::connect(internalAddress)
        .await
        .expect("内部测试连接必须建立");
    client
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .expect("SOCKS 探针必须写入");
    let mut byte = [0_u8; 1];
    let readResult = timeout(testTimeout, client.read(&mut byte))
        .await
        .expect("未认领内部连接必须及时关闭");
    let connectionClosed = match readResult {
        Ok(0) => true,
        Err(error) => matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
        ),
        Ok(_) => false,
    };
    assert!(connectionClosed);
    assert!(!serveCalled.load(Ordering::SeqCst));
    assert!(server.stop().await.errorMessage.is_none());

    let disabledServer = startFusedProxyServer(
        testConfig(),
        FusedProxyDependencies {
            pluginHost: PluginHost::disabled(),
            tunnelInterceptor: None,
            addressOverride: None,
            protocolHandler: Some(Arc::new(RejectingInternalHandler { serveCalled })),
            outboundConnector: None,
        },
        FusedProxyOptions::default(),
    )
    .await
    .expect("禁用内部入口时公开监听必须启动");
    assert!(disabledServer.internalCaptureAddress().is_none());
    assert!(disabledServer.stop().await.errorMessage.is_none());
}

/// 模拟拒绝完成优雅排空的协议处理器，并记录融合监听是否执行强制中止。
struct BlockingShutdownHandler {
    abortCalled: Arc<AtomicBool>,
    trackedTask: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl PortProtocolHandler for BlockingShutdownHandler {
    /// 测试不建立连接，因此该入口只满足协议处理器契约。
    fn serve(
        &self,
        _stream: TcpStream,
        _clientAddress: SocketAddr,
        _cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }

    /// 永久等待以稳定触发统一停机预算，验证监听器不会把超时误报为成功。
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
        Box::pin(std::future::pending())
    }

    /// 中止内部任务并等待其析构；返回时设置的析构标志可证明停止结果没有早于任务资源释放。
    fn abortAndWait(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let abortCalled = self.abortCalled.clone();
        let trackedTask = self
            .trackedTask
            .lock()
            .expect("测试任务句柄锁不得中毒")
            .take();
        Box::pin(async move {
            abortCalled.store(true, Ordering::SeqCst);
            if let Some(task) = trackedTask {
                task.abort();
                let _ = task.await;
            }
        })
    }
}

/// 在测试任务 future 完成析构时发布状态，避免只验证已经发送中止请求。
struct TaskDropMarker(Arc<AtomicBool>);

impl Drop for TaskDropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// 停止必须跳过永久挂起的优雅排空入口并直接强制中止，禁止长连接被误报为服务故障。
#[tokio::test]
async fn forceStopBypassesProtocolDrainAndAbortsTasks() {
    let abortCalled = Arc::new(AtomicBool::new(false));
    let taskDropped = Arc::new(AtomicBool::new(false));
    let dropMarker = TaskDropMarker(taskDropped.clone());
    let trackedTask = tokio::spawn(async move {
        let _dropMarker = dropMarker;
        std::future::pending::<()>().await;
    });
    let server = startFusedProxyServer(
        testConfig(),
        FusedProxyDependencies {
            pluginHost: PluginHost::disabled(),
            tunnelInterceptor: None,
            addressOverride: None,
            protocolHandler: Some(Arc::new(BlockingShutdownHandler {
                abortCalled: abortCalled.clone(),
                trackedTask: Arc::new(Mutex::new(Some(trackedTask))),
            })),
            outboundConnector: None,
        },
        FusedProxyOptions::default(),
    )
    .await
    .expect("融合监听必须启动");

    let outcome = timeout(testTimeout, server.stop())
        .await
        .expect("停止结果必须受测试时限约束");
    assert!(outcome.errorMessage.is_none());
    assert!(abortCalled.load(Ordering::SeqCst));
    assert!(taskDropped.load(Ordering::SeqCst));
}

/// 活动客户端保持套接字不关闭时，stop 仍须在固定短边界内断开连接并立即释放监听端口。
#[tokio::test]
async fn forceStopClosesStalledClientAndReleasesPortImmediately() {
    let server = startSocks5Server(testConfig())
        .await
        .expect("强制停止测试服务必须启动");
    let address = server.boundAddress();
    let mut client = TcpStream::connect(address)
        .await
        .expect("悬挂客户端必须连接");
    negotiateNoAuth(&mut client).await;

    let startedAt = Instant::now();
    let outcome = timeout(Duration::from_millis(500), server.stop())
        .await
        .expect("强制停止不得等待客户端读写超时");
    assert!(outcome.errorMessage.is_none());
    assert!(startedAt.elapsed() < Duration::from_millis(500));

    let mut closedByte = [0_u8; 1];
    let closeResult = timeout(Duration::from_millis(500), client.read(&mut closedByte))
        .await
        .expect("客户端必须立即观察到代理断开");
    let connectionClosed = match closeResult {
        Ok(0) => true,
        Err(error) => matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
        ),
        Ok(_) => false,
    };
    assert!(connectionClosed);
    let rebound = TcpListener::bind(address)
        .await
        .expect("停止返回后监听端口必须立即可重绑");
    drop(rebound);
}

/// 未发送首字节的融合连接必须受读取超时约束，避免占满连接额度并阻塞停机。
#[tokio::test]
async fn closesUnclassifiedConnectionAfterPeekTimeout() {
    let mut config = testConfig();
    config.readTimeoutMilliseconds = 50;
    let handler = Arc::new(ClaimingProtocolHandler {
        observedByte: Arc::new(AtomicUsize::new(usize::MAX)),
        shutdownCalled: Arc::new(AtomicBool::new(false)),
    });
    /// 仅关闭提前认领行为，用于验证普通未分类连接的首字节等待上限。
    struct NonClaimingHandler(Arc<ClaimingProtocolHandler>);
    impl PortProtocolHandler for NonClaimingHandler {
        /// 此夹具仅触发首字节等待，不提前接管连接。
        fn serve(
            &self,
            stream: TcpStream,
            clientAddress: SocketAddr,
            cancellation: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            self.0.serve(stream, clientAddress, cancellation)
        }

        /// 委托停机记录，确保超时连接退出后仍执行处理器生命周期回调。
        fn shutdown(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
            self.0.shutdown()
        }
    }
    let server = startFusedProxyServer(
        config,
        FusedProxyDependencies {
            pluginHost: PluginHost::disabled(),
            tunnelInterceptor: None,
            addressOverride: None,
            protocolHandler: Some(Arc::new(NonClaimingHandler(handler))),
            outboundConnector: None,
        },
        FusedProxyOptions::default(),
    )
    .await
    .expect("融合监听必须启动");
    let mut client = TcpStream::connect(server.boundAddress())
        .await
        .expect("测试客户端必须连接");
    let mut byte = [0_u8; 1];
    let readBytes = timeout(Duration::from_millis(500), client.read(&mut byte))
        .await
        .expect("首字节等待必须在配置超时内结束")
        .expect("关闭连接读取必须成功");
    assert_eq!(readBytes, 0);
    assert!(server.stop().await.errorMessage.is_none());
}

/// 验证 CONNECT 响应、双向转发和会话流量快照。
#[tokio::test]
async fn connectRelaysTcpTraffic() {
    timeout(testTimeout, async {
        let (echoAddress, echoTask) = startTcpEcho().await;
        let server = startSocks5Server(testConfig())
            .await
            .expect("启动 SOCKS5 服务");
        let mut client = TcpStream::connect(server.boundAddress())
            .await
            .expect("连接 SOCKS5");
        negotiateNoAuth(&mut client).await;
        writeRequest(&mut client, 0x01, echoAddress).await;
        let _ = readSuccessfulReply(&mut client).await;
        client.write_all(b"connect-echo").await.expect("写入代理");
        let mut response = [0_u8; 12];
        client.read_exact(&mut response).await.expect("读取代理");
        assert_eq!(&response, b"connect-echo");
        let snapshot = server.snapshot();
        assert_eq!(snapshot.metrics.bytesUp, 12);
        assert_eq!(snapshot.metrics.bytesDown, 12);
        drop(client);
        assert!(server.stop().await.errorMessage.is_none());
        echoTask.abort();
    })
    .await
    .expect("CONNECT 集成测试超时");
}

/// 验证 DNS 覆盖同时作用于 SOCKS5 CONNECT，且会话仍记录原始域名而不是映射 IP。
#[tokio::test]
async fn connectUsesInjectedDomainOverride() {
    timeout(testTimeout, async {
        let (echoAddress, echoTask) = startTcpEcho().await;
        let server = startSocks5ServerWithInterceptionAndResolver(
            testConfig(),
            PluginHost::disabled(),
            None,
            Some(Arc::new(LocalAddressOverride)),
        )
        .await
        .expect("启动带 DNS 覆盖的 SOCKS5 服务");
        let mut client = TcpStream::connect(server.boundAddress())
            .await
            .expect("连接 SOCKS5");
        negotiateNoAuth(&mut client).await;
        writeDomainRequest(&mut client, 0x01, mappedTestDomain, echoAddress.port()).await;
        let _ = readSuccessfulReply(&mut client).await;
        client.write_all(b"dns-connect").await.expect("写入代理");
        let mut response = [0_u8; 11];
        client.read_exact(&mut response).await.expect("读取代理");
        assert_eq!(&response, b"dns-connect");
        assert_eq!(
            server.snapshot().sessions[0].targetAddress,
            format!("{mappedTestDomain}:{}", echoAddress.port())
        );
        drop(client);
        assert!(server.stop().await.errorMessage.is_none());
        echoTask.abort();
    })
    .await
    .expect("SOCKS5 CONNECT DNS 覆盖测试超时");
}

/// 验证密码模式真实服务只接受 RFC1929，并在成功认证后允许 CONNECT。
#[tokio::test]
async fn passwordAuthenticationAllowsConnect() {
    timeout(testTimeout, async {
        let (echoAddress, echoTask) = startTcpEcho().await;
        let mut configuration = testConfig();
        configuration.authenticationMode = AuthenticationMode::UsernamePassword;
        configuration.users = HashMap::from([("alice".to_owned(), "secret".to_owned())]);
        let server = startSocks5Server(configuration)
            .await
            .expect("启动密码 SOCKS5");
        let mut client = TcpStream::connect(server.boundAddress())
            .await
            .expect("连接密码 SOCKS5");
        client
            .write_all(&[0x05, 0x01, 0x02])
            .await
            .expect("写入密码方法");
        let mut methodReply = [0_u8; 2];
        client
            .read_exact(&mut methodReply)
            .await
            .expect("读取密码方法");
        assert_eq!(methodReply, [0x05, 0x02]);
        client
            .write_all(&[
                0x01, 5, b'a', b'l', b'i', b'c', b'e', 6, b's', b'e', b'c', b'r', b'e', b't',
            ])
            .await
            .expect("写入密码认证");
        let mut authenticationReply = [0_u8; 2];
        client
            .read_exact(&mut authenticationReply)
            .await
            .expect("读取密码认证");
        assert_eq!(authenticationReply, [0x01, 0x00]);
        writeRequest(&mut client, 0x01, echoAddress).await;
        let _ = readSuccessfulReply(&mut client).await;
        client.write_all(b"auth").await.expect("写入认证代理");
        let mut response = [0_u8; 4];
        client
            .read_exact(&mut response)
            .await
            .expect("读取认证代理");
        assert_eq!(&response, b"auth");
        let sessions = server.snapshot().sessions;
        assert_eq!(sessions[0].username, "alice");
        drop(client);
        assert!(server.stop().await.errorMessage.is_none());
        echoTask.abort();
    })
    .await
    .expect("密码认证集成测试超时");
}

/// 验证 BIND 使用私有临时监听器发送两阶段响应并转发双方数据。
#[tokio::test]
async fn bindUsesTwoStageReplyAndRelays() {
    timeout(testTimeout, async {
        let server = startSocks5Server(testConfig())
            .await
            .expect("启动 SOCKS5 服务");
        let mut control = TcpStream::connect(server.boundAddress())
            .await
            .expect("连接 SOCKS5");
        negotiateNoAuth(&mut control).await;
        writeRequest(
            &mut control,
            0x02,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        )
        .await;
        let bindAddress = readSuccessfulReply(&mut control).await;
        let mut remote = TcpStream::connect(bindAddress).await.expect("连接 BIND");
        let secondAddress = readSuccessfulReply(&mut control).await;
        assert_eq!(secondAddress, remote.local_addr().expect("远端本地地址"));
        control.write_all(b"up").await.expect("写入上行");
        let mut up = [0_u8; 2];
        remote.read_exact(&mut up).await.expect("读取上行");
        assert_eq!(&up, b"up");
        remote.write_all(b"down").await.expect("写入下行");
        let mut down = [0_u8; 4];
        control.read_exact(&mut down).await.expect("读取下行");
        assert_eq!(&down, b"down");
        drop(remote);
        drop(control);
        assert!(server.stop().await.errorMessage.is_none());
    })
    .await
    .expect("BIND 集成测试超时");
}

/// 验证 UDP ASSOCIATE 的来源绑定、数据报解析和远端响应封装。
#[tokio::test]
async fn udpAssociateRelaysDatagram() {
    timeout(testTimeout, async {
        let (echoAddress, echoTask) = startUdpEcho().await;
        let server = startSocks5Server(testConfig())
            .await
            .expect("启动 SOCKS5 服务");
        let mut control = TcpStream::connect(server.boundAddress())
            .await
            .expect("连接 SOCKS5");
        negotiateNoAuth(&mut control).await;
        let udpClient = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("绑定 UDP 客户端");
        writeRequest(
            &mut control,
            0x03,
            udpClient.local_addr().expect("UDP 客户端地址"),
        )
        .await;
        let relayAddress = readSuccessfulReply(&mut control).await;
        let packet = encodeUdpPacket(echoAddress, b"udp-echo").expect("编码 UDP 请求");
        udpClient
            .send_to(&packet, relayAddress)
            .await
            .expect("发送 UDP 请求");
        let mut response = [0_u8; 1_024];
        let (byteCount, _) = udpClient
            .recv_from(&mut response)
            .await
            .expect("读取 UDP 响应");
        let decoded = decodeUdpPacket(&response[..byteCount]).expect("解码 UDP 响应");
        assert_eq!(decoded.payload, b"udp-echo");
        let snapshot = server.snapshot();
        assert_eq!(snapshot.metrics.udpPacketsUp, 1);
        assert_eq!(snapshot.metrics.udpPacketsDown, 1);
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].targetAddress, echoAddress.to_string());
        drop(control);
        assert!(server.stop().await.errorMessage.is_none());
        echoTask.abort();
    })
    .await
    .expect("UDP ASSOCIATE 集成测试超时");
}

/// 验证 IPv6 控制连接、UDP 中继端点和远端目标均保持 IPv6，正文不会因地址编码被改写。
#[tokio::test]
async fn udpAssociateRelaysIpv6Datagram() {
    timeout(testTimeout, async {
        let (echoAddress, echoTask) = startIpv6UdpEcho().await;
        let mut config = testConfig();
        config.listenHost = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let server = startSocks5Server(config)
            .await
            .expect("启动 IPv6 SOCKS5 服务");
        let mut control = TcpStream::connect(server.boundAddress())
            .await
            .expect("连接 IPv6 SOCKS5");
        negotiateNoAuth(&mut control).await;
        let udpClient = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("绑定 IPv6 UDP 客户端");
        writeRequest(
            &mut control,
            0x03,
            udpClient.local_addr().expect("IPv6 UDP 客户端地址"),
        )
        .await;
        let relayAddress = readSuccessfulReply(&mut control).await;
        assert!(relayAddress.is_ipv6());
        let packet = encodeUdpPacket(echoAddress, b"ipv6-udp-echo").expect("编码 IPv6 UDP 请求");
        udpClient
            .send_to(&packet, relayAddress)
            .await
            .expect("发送 IPv6 UDP 请求");
        let mut response = [0_u8; 1_024];
        let (byteCount, _) = udpClient
            .recv_from(&mut response)
            .await
            .expect("读取 IPv6 UDP 响应");
        let decoded = decodeUdpPacket(&response[..byteCount]).expect("解码 IPv6 UDP 响应");
        assert_eq!(decoded.destination.toString(), echoAddress.to_string());
        assert_eq!(decoded.payload, b"ipv6-udp-echo");
        let snapshot = server.snapshot();
        assert_eq!(snapshot.metrics.udpPacketsUp, 1);
        assert_eq!(snapshot.metrics.udpPacketsDown, 1);
        drop(control);
        assert!(server.stop().await.errorMessage.is_none());
        echoTask.abort();
    })
    .await
    .expect("IPv6 UDP ASSOCIATE 集成测试超时");
}

/// 验证 DNS 覆盖同样作用于 SOCKS5 UDP 数据报，避免 TCP 与 UDP 规则语义分裂。
#[tokio::test]
async fn udpAssociateUsesInjectedDomainOverride() {
    timeout(testTimeout, async {
        let (echoAddress, echoTask) = startUdpEcho().await;
        let server = startSocks5ServerWithInterceptionAndResolver(
            testConfig(),
            PluginHost::disabled(),
            None,
            Some(Arc::new(LocalAddressOverride)),
        )
        .await
        .expect("启动带 DNS 覆盖的 SOCKS5 服务");
        let mut control = TcpStream::connect(server.boundAddress())
            .await
            .expect("连接 SOCKS5");
        negotiateNoAuth(&mut control).await;
        let udpClient = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("绑定 UDP 客户端");
        writeRequest(
            &mut control,
            0x03,
            udpClient.local_addr().expect("UDP 客户端地址"),
        )
        .await;
        let relayAddress = readSuccessfulReply(&mut control).await;
        let packet = encodeDomainUdpPacket(mappedTestDomain, echoAddress.port(), b"dns-udp");
        udpClient
            .send_to(&packet, relayAddress)
            .await
            .expect("发送域名 UDP 请求");
        let mut response = [0_u8; 1_024];
        let (byteCount, _) = udpClient
            .recv_from(&mut response)
            .await
            .expect("读取域名 UDP 响应");
        let decoded = decodeUdpPacket(&response[..byteCount]).expect("解码 UDP 响应");
        assert_eq!(decoded.payload, b"dns-udp");
        let snapshot = server.snapshot();
        assert_eq!(snapshot.metrics.udpPacketsUp, 1);
        assert_eq!(snapshot.metrics.udpPacketsDown, 1);
        assert_eq!(
            snapshot.sessions[0].targetAddress,
            format!("{mappedTestDomain}:{}", echoAddress.port())
        );
        drop(control);
        assert!(server.stop().await.errorMessage.is_none());
        echoTask.abort();
    })
    .await
    .expect("SOCKS5 UDP DNS 覆盖测试超时");
}
