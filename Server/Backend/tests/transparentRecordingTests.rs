#![allow(non_snake_case)]

use std::{io, time::Duration};

use capture_core::{
    MessageSide, RecordingConfiguration, RecordingSession, TransactionProtocol, TransactionStatus,
};
use plugin_host::{
    PacketFilterAction, PacketFilterConfiguration, PacketFilterDirection, PacketFilterRule,
    PacketFilterTransport, PluginHost,
};
use proxy_backend::transparentRecording::TransparentRecording;
use socks5_core::{SessionApplicationProtocol, interception::TcpTunnel};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 创建录制器和两端真实 TCP 套接字；测试客户端与测试服务端分别位于透明中继的外侧。
///
/// 运行上下文：每个测试使用独占临时目录和监听端口，不依赖 WinDivert 或系统代理。
/// 参数：`protocol` 决定事务显示为 tcp 或 https；失败时直接终止测试并报告具体监听边界。
async fn transparentFixture(
    protocol: SessionApplicationProtocol,
) -> (
    tempfile::TempDir,
    RecordingSession,
    TransparentRecording,
    TcpTunnel,
    TcpStream,
    TcpStream,
) {
    let temporaryDirectory = tempfile::tempdir().expect("创建录制临时目录");
    let recording = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().to_path_buf(),
        memoryBodyThreshold: 8,
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建录制会话");
    let recorder = TransparentRecording::new(recording.clone());

    let clientListener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定客户端夹具");
    let clientAddress = clientListener.local_addr().expect("读取客户端夹具地址");
    let testClient = TcpStream::connect(clientAddress)
        .await
        .expect("连接客户端夹具");
    let (proxyClient, observedClientAddress) =
        clientListener.accept().await.expect("接受客户端夹具");

    let serverListener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定服务端夹具");
    let serverAddress = serverListener.local_addr().expect("读取服务端夹具地址");
    let proxyRemote = TcpStream::connect(serverAddress)
        .await
        .expect("连接服务端夹具");
    let (testServer, _) = serverListener.accept().await.expect("接受服务端夹具");
    let targetHost = match protocol {
        SessionApplicationProtocol::Tls => "tls.fixture.local",
        _ => "tcp.fixture.local",
    };
    let tunnel = TcpTunnel {
        clientStream: proxyClient,
        remoteStream: proxyRemote,
        clientAddress: observedClientAddress,
        clientProcessName: Some("fixtureClient.exe".to_owned()),
        clientProcessId: Some(42_424),
        targetHost: targetHost.to_owned(),
        connectHost: serverAddress.ip().to_string(),
        routePinned: true,
        targetPort: serverAddress.port(),
        cancellation: CancellationToken::new(),
        accountLease: None,
    };
    (
        temporaryDirectory,
        recording,
        recorder,
        tunnel,
        testClient,
        testServer,
    )
}

/// 验证超过旧内存正文阈值的 Raw TCP 双向字节逐块落盘，并生成覆盖完整正文的连续方向索引。
#[tokio::test]
async fn recordsCompleteLargeRawTcpBodiesWithoutTruncation() {
    let (_temporaryDirectory, recording, recorder, tunnel, mut client, mut server) =
        transparentFixture(SessionApplicationProtocol::Tcp).await;
    let requestBytes = (0..(3 * 1024 * 1024 + 31))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let responseBytes = (0..(2 * 1024 * 1024 + 47))
        .map(|index| (index % 239) as u8)
        .collect::<Vec<_>>();
    let expectedRequest = requestBytes.clone();
    let expectedResponse = responseBytes.clone();
    let relay = tokio::spawn({
        let recorder = recorder.clone();
        async move {
            recorder
                .relay(tunnel, SessionApplicationProtocol::Tcp)
                .await
        }
    });
    let clientTask = tokio::spawn(async move {
        client
            .write_all(&requestBytes)
            .await
            .expect("客户端写入请求");
        client.shutdown().await.expect("客户端结束请求方向");
        let mut received = Vec::new();
        client
            .read_to_end(&mut received)
            .await
            .expect("客户端读取响应");
        received
    });
    let serverTask = tokio::spawn(async move {
        let mut received = Vec::new();
        server
            .read_to_end(&mut received)
            .await
            .expect("服务端读取请求");
        server
            .write_all(&responseBytes)
            .await
            .expect("服务端写入响应");
        server.shutdown().await.expect("服务端结束响应方向");
        received
    });
    let (relayResult, clientReceived, serverReceived) = timeout(TEST_TIMEOUT, async {
        (
            relay.await.expect("中继任务不得 panic"),
            clientTask.await.expect("客户端任务不得 panic"),
            serverTask.await.expect("服务端任务不得 panic"),
        )
    })
    .await
    .expect("大正文中继必须在测试时限内结束");
    relayResult.expect("Raw TCP 中继必须成功");
    assert_eq!(clientReceived, expectedResponse);
    assert_eq!(serverReceived, expectedRequest);

    let summaries = recording.listMetadata().await.expect("读取录制摘要");
    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.protocol, TransactionProtocol::Tunnel);
    assert_eq!(summary.status, TransactionStatus::Complete);
    assert_eq!(
        summary.clientProcessName.as_deref(),
        Some("fixtureClient.exe")
    );
    assert_eq!(summary.clientProcessId, Some(42_424));
    assert!(summary.urlDisplay.starts_with("tcp://tcp.fixture.local:"));
    assert!(!summary.flags.bodyTruncated);
    let storedRequest = recording
        .getBody(&summary.transactionId, MessageSide::Request)
        .await
        .expect("读取完整请求正文");
    let storedResponse = recording
        .getBody(&summary.transactionId, MessageSide::Response)
        .await
        .expect("读取完整响应正文");
    assert_eq!(storedRequest.bytes, expectedRequest);
    assert_eq!(storedResponse.bytes, expectedResponse);
    assert!(!storedRequest.meta.truncated);
    assert!(!storedResponse.meta.truncated);
    let detail = recording
        .getTransactionDetail(&summary.transactionId)
        .await
        .expect("读取原始流片段索引");
    assert!(!detail.requestPackets.is_empty());
    assert!(!detail.responsePackets.is_empty());
    assert_eq!(
        detail
            .requestPackets
            .iter()
            .map(|packet| packet.storedBytes)
            .sum::<usize>(),
        expectedRequest.len()
    );
    assert_eq!(
        detail
            .responsePackets
            .iter()
            .map(|packet| packet.storedBytes)
            .sum::<usize>(),
        expectedResponse.len()
    );
    let mut expectedOffset = 0;
    for (index, packet) in detail.requestPackets.iter().enumerate() {
        assert_eq!(packet.sequence, index as u64 + 1);
        assert_eq!(packet.storedOffsetBytes, expectedOffset);
        assert!(!packet.truncated);
        expectedOffset += packet.storedBytes;
    }
    recorder.shutdown().await;
}

/// 验证 WinDivert Raw TCP 使用与 SOCKS5 相同的最终写线入口；服务端和录制正文都只能看到修改后的字节。
#[tokio::test]
async fn transparentTcpUsesSharedPacketFilterDataPlane() {
    let (_temporaryDirectory, recording, recorder, tunnel, mut client, mut server) =
        transparentFixture(SessionApplicationProtocol::Tcp).await;
    let pluginHost = PluginHost::disabled();
    pluginHost
        .packetFilters()
        .replaceConfiguration(PacketFilterConfiguration {
            enabled: true,
            rules: vec![PacketFilterRule {
                id: "transparentTcpRewrite".to_owned(),
                name: "透明 TCP 修改".to_owned(),
                enabled: true,
                transport: PacketFilterTransport::Tcp,
                direction: PacketFilterDirection::Up,
                host: "tcp.fixture.local".to_owned(),
                port: Some(tunnel.targetPort),
                minimumLength: None,
                maximumLength: None,
                pattern: "70 6C 61 69 6E 2D 62 65 66 6F 72 65".to_owned(),
                replacement: "70 6C 61 69 6E 2D 61 66 74 65 72 21".to_owned(),
                action: PacketFilterAction::Modify,
                replaceAll: true,
                continueMatching: false,
            }],
        })
        .expect("透明 TCP 封包滤镜配置应有效");
    let relay = tokio::spawn(async move {
        recorder
            .relayWithDataPlane(tunnel, SessionApplicationProtocol::Tcp, pluginHost)
            .await
    });
    client
        .write_all(b"plain-before")
        .await
        .expect("客户端写入透明 TCP 测试正文");
    client.shutdown().await.expect("客户端结束上行");
    let mut serverReceived = Vec::new();
    server
        .read_to_end(&mut serverReceived)
        .await
        .expect("服务端读取透明 TCP 正文");
    server.shutdown().await.expect("服务端结束下行");
    timeout(TEST_TIMEOUT, relay)
        .await
        .expect("透明 TCP 中继超时")
        .expect("透明 TCP 中继任务异常")
        .expect("透明 TCP 中继失败");
    assert_eq!(serverReceived, b"plain-after!");
    let transactions = recording
        .listMetadata()
        .await
        .expect("读取透明 TCP 事务列表");
    let transactionId = &transactions
        .first()
        .expect("应生成透明 TCP 事务")
        .transactionId;
    let body = recording
        .getBody(transactionId, MessageSide::Request)
        .await
        .expect("读取透明 TCP 录制正文");
    assert_eq!(body.bytes, b"plain-after!");
}

/// 验证服务取消立即关闭 RawTls 套接字，同时后台 spool 仍提交已确认字节并形成 cancelled 终态。
#[tokio::test]
async fn cancellationFinalizesRawTlsWithExactObservedBodies() {
    let (_temporaryDirectory, recording, recorder, tunnel, mut client, mut server) =
        transparentFixture(SessionApplicationProtocol::Tls).await;
    let cancellation = tunnel.cancellation.clone();
    let requestBytes = vec![0x16; 512 * 1024 + 13];
    let responseBytes = vec![0x17; 384 * 1024 + 19];
    let relay = tokio::spawn({
        let recorder = recorder.clone();
        async move {
            recorder
                .relay(tunnel, SessionApplicationProtocol::Tls)
                .await
        }
    });
    client
        .write_all(&requestBytes)
        .await
        .expect("客户端写入 TLS 原始字节");
    let mut serverReceived = vec![0_u8; requestBytes.len()];
    server
        .read_exact(&mut serverReceived)
        .await
        .expect("服务端读取 TLS 原始字节");
    server
        .write_all(&responseBytes)
        .await
        .expect("服务端写入 TLS 响应字节");
    let mut clientReceived = vec![0_u8; responseBytes.len()];
    client
        .read_exact(&mut clientReceived)
        .await
        .expect("客户端读取 TLS 响应字节");
    cancellation.cancel();
    let relayError = timeout(TEST_TIMEOUT, relay)
        .await
        .expect("取消后的中继必须及时结束")
        .expect("中继任务不得 panic")
        .expect_err("取消必须结束连接");
    assert_eq!(relayError.kind(), io::ErrorKind::Interrupted);
    assert_eq!(serverReceived, requestBytes);
    assert_eq!(clientReceived, responseBytes);

    let summaries = recording.listMetadata().await.expect("读取取消事务摘要");
    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.status, TransactionStatus::Cancelled);
    assert!(summary.urlDisplay.starts_with("https://tls.fixture.local:"));
    assert_eq!(
        recording
            .getBody(&summary.transactionId, MessageSide::Request)
            .await
            .expect("读取取消请求正文")
            .bytes,
        requestBytes
    );
    assert_eq!(
        recording
            .getBody(&summary.transactionId, MessageSide::Response)
            .await
            .expect("读取取消响应正文")
            .bytes,
        responseBytes
    );
    recorder.abortAndWait().await;
}

/// 验证监听器强制中止外层套接字任务后，后台 spool 仍提交此前已确认的双向字节并形成取消终态。
#[tokio::test]
async fn forcedRelayAbortPersistsObservedBodiesBeforeShutdownReturns() {
    let (_temporaryDirectory, recording, recorder, tunnel, mut client, mut server) =
        transparentFixture(SessionApplicationProtocol::Tcp).await;
    let requestBytes = vec![0x31; 128 * 1024 + 7];
    let responseBytes = vec![0x32; 96 * 1024 + 11];
    let relay = tokio::spawn({
        let recorder = recorder.clone();
        async move {
            recorder
                .relay(tunnel, SessionApplicationProtocol::Tcp)
                .await
        }
    });
    client
        .write_all(&requestBytes)
        .await
        .expect("客户端写入强制停止前字节");
    let mut serverReceived = vec![0_u8; requestBytes.len()];
    server
        .read_exact(&mut serverReceived)
        .await
        .expect("服务端确认强制停止前请求字节");
    server
        .write_all(&responseBytes)
        .await
        .expect("服务端写入强制停止前响应字节");
    let mut clientReceived = vec![0_u8; responseBytes.len()];
    client
        .read_exact(&mut clientReceived)
        .await
        .expect("客户端确认强制停止前响应字节");

    relay.abort();
    let joinError = relay.await.expect_err("强制停止必须中止外层套接字任务");
    assert!(joinError.is_cancelled());
    timeout(TEST_TIMEOUT, recorder.abortAndWait())
        .await
        .expect("后台 spool 必须在停机时限内提交取消终态");

    let summaries = recording
        .listMetadata()
        .await
        .expect("读取强制停止事务摘要");
    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.status, TransactionStatus::Cancelled);
    assert_eq!(serverReceived, requestBytes);
    assert_eq!(clientReceived, responseBytes);
    assert_eq!(
        recording
            .getBody(&summary.transactionId, MessageSide::Request)
            .await
            .expect("读取强制停止请求正文")
            .bytes,
        requestBytes
    );
    assert_eq!(
        recording
            .getBody(&summary.transactionId, MessageSide::Response)
            .await
            .expect("读取强制停止响应正文")
            .bytes,
        responseBytes
    );
}

/// 验证录制会话不可用时透明中继仍逐字节传递双向负载，不把旁路录制故障伪装成网络坏包。
///
/// 运行上下文：先关闭录制会话，使事务初始化确定失败，再通过真实 TCP 套接字完成请求和响应。
/// 失败语义：任一方向缺字节、内容变化或 relay 返回错误都会直接使测试失败；关闭会话不应产生事务。
#[tokio::test]
async fn recordingInitializationFailureDoesNotInterruptTcpRelay() {
    let (_temporaryDirectory, recording, recorder, tunnel, mut client, mut server) =
        transparentFixture(SessionApplicationProtocol::Tcp).await;
    recording.close().await.expect("关闭录制会话");
    let requestBytes = (0..(512 * 1024 + 29))
        .map(|index| (index % 241) as u8)
        .collect::<Vec<_>>();
    let responseBytes = (0..(384 * 1024 + 37))
        .map(|index| (index % 229) as u8)
        .collect::<Vec<_>>();
    let expectedRequest = requestBytes.clone();
    let expectedResponse = responseBytes.clone();
    let relay = tokio::spawn(async move {
        recorder
            .relay(tunnel, SessionApplicationProtocol::Tcp)
            .await
    });
    let clientTask = tokio::spawn(async move {
        client
            .write_all(&requestBytes)
            .await
            .expect("客户端写入请求");
        client.shutdown().await.expect("客户端结束请求方向");
        let mut received = Vec::new();
        client
            .read_to_end(&mut received)
            .await
            .expect("客户端读取响应");
        received
    });
    let serverTask = tokio::spawn(async move {
        let mut received = Vec::new();
        server
            .read_to_end(&mut received)
            .await
            .expect("服务端读取请求");
        server
            .write_all(&responseBytes)
            .await
            .expect("服务端写入响应");
        server.shutdown().await.expect("服务端结束响应方向");
        received
    });

    let (relayResult, clientReceived, serverReceived) = timeout(TEST_TIMEOUT, async {
        (
            relay.await.expect("透明中继任务不得 panic"),
            clientTask.await.expect("客户端任务不得 panic"),
            serverTask.await.expect("服务端任务不得 panic"),
        )
    })
    .await
    .expect("录制关闭后的透明中继必须按时完成");
    relayResult.expect("录制故障不得中断透明中继");
    assert_eq!(serverReceived, expectedRequest);
    assert_eq!(clientReceived, expectedResponse);
    assert!(recording.listMetadata().await.is_err());
}
