#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use capture_core::{
    MessageSide, RecordingConfiguration, RecordingSession, TransactionProtocol, TransactionStatus,
};
use proxy_backend::socksTransactionProjection::SocksTransactionProjector;
use socks5_core::{
    SessionApplicationProtocol, SessionSnapshot, SessionState, TrafficDirection,
    registry::{SessionRegistry, SessionUpdate},
};

/// 构造具有确定时间与端点的会话快照，测试只覆盖投影语义而不依赖真实网络调度。
fn sessionSnapshot(command: &str, targetAddress: &str) -> SessionSnapshot {
    SessionSnapshot {
        sessionId: format!("session-{command}"),
        clientAddress: "127.0.0.1:50000".to_owned(),
        username: String::new(),
        command: command.to_owned(),
        targetAddress: targetAddress.to_owned(),
        state: SessionState::Connecting,
        bytesUp: 0,
        bytesDown: 0,
        createdAtMilliseconds: 1_000,
        updatedAtMilliseconds: 1_100,
        closedAtMilliseconds: 0,
        errorMessage: String::new(),
        applicationProtocol: match command {
            "udpAssociate" => SessionApplicationProtocol::Udp,
            _ => SessionApplicationProtocol::Tcp,
        },
        captureGeneration: 0,
        capturedBytesUp: Default::default(),
        capturedBytesDown: Default::default(),
        capturedPackets: Default::default(),
    }
}

/// 创建隔离录制会话；RecordingSession 自身负责为每个实例分配唯一 spill 子目录。
async fn recordingSession() -> RecordingSession {
    RecordingSession::new(RecordingConfiguration::default())
        .await
        .expect("创建 SOCKS 投影测试录制会话")
}

/// 验证 CONNECT 的主机、字节、阶段时间和成功终态完整进入统一事务模型。
#[tokio::test]
async fn connectSessionProjectsToCompletedTransaction() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    let mut session = sessionSnapshot("connect", "example.com:443");
    projector
        .project(&session)
        .await
        .expect("创建 CONNECT 事务");

    session.state = SessionState::Relaying;
    session.bytesUp = 128;
    session.bytesDown = 256;
    session.capturedBytesUp = b"request-prefix".to_vec().into();
    session.capturedBytesDown = b"response-prefix".to_vec().into();
    session.updatedAtMilliseconds = 1_200;
    projector
        .project(&session)
        .await
        .expect("同步 CONNECT 流量");

    session.state = SessionState::Closed;
    session.closedAtMilliseconds = 1_300;
    session.updatedAtMilliseconds = 1_300;
    projector
        .project(&session)
        .await
        .expect("完成 CONNECT 事务");

    let page = recording
        .pageView(None, 10, None)
        .await
        .expect("读取 CONNECT 事务");
    assert_eq!(page.total, 1);
    let transaction = &page.transactions[0];
    assert_eq!(transaction.protocol, TransactionProtocol::Socks);
    assert_eq!(transaction.method, "CONNECT");
    assert_eq!(transaction.host, "example.com");
    assert_eq!(transaction.port, 443);
    assert_eq!(transaction.urlDisplay, "tcp://example.com:443");
    assert!(transaction.path.is_empty());
    assert_eq!(transaction.status, TransactionStatus::Complete);
    assert_eq!(transaction.statusCode, Some(0));
    assert_eq!(transaction.sizes.requestBodyBytes, 128);
    assert_eq!(transaction.sizes.responseBodyBytes, 256);
    assert_eq!(transaction.timings.connectEndAtMilliseconds, Some(1_200));
    assert_eq!(transaction.timings.endAtMilliseconds, Some(1_300));
    let requestBody = recording
        .getBody(&transaction.transactionId, MessageSide::Request)
        .await
        .expect("读取 SOCKS 请求流前缀");
    let responseBody = recording
        .getBody(&transaction.transactionId, MessageSide::Response)
        .await
        .expect("读取 SOCKS 响应流前缀");
    assert_eq!(requestBody.bytes, b"request-prefix");
    assert_eq!(requestBody.meta.originalBytes, 128);
    assert!(requestBody.meta.truncated);
    assert_eq!(responseBody.bytes, b"response-prefix");
    assert_eq!(responseBody.meta.originalBytes, 256);
    assert_eq!(responseBody.meta.contentType, "application/octet-stream");
    assert_eq!(responseBody.meta.encoding, "binary");
}

/// 验证未解密 TLS 长连接尚未关闭时首批双向片段已经进入详情接口，防止 HTTPS 原始流有字节却显示零包。
#[tokio::test]
async fn activeRawTlsSessionPublishesPacketsBeforeClose() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    let registry = SessionRegistry::withCaptureBudget(8, 64 * 1024);
    let sessionId = registry.create("127.0.0.1:50000".to_owned());
    assert!(registry.update(
        &sessionId,
        SessionUpdate {
            username: None,
            command: Some("connect".to_owned()),
            targetAddress: Some("example.com:443".to_owned()),
            applicationProtocol: Some(SessionApplicationProtocol::Tls),
            state: SessionState::Relaying,
        },
    ));
    let initialSnapshot = registry.snapshots().pop().expect("活动会话快照");
    projector
        .project(&initialSnapshot)
        .await
        .expect("创建活动原始流事务");

    registry.addTraffic(&sessionId, TrafficDirection::Up, b"request");
    let requestSnapshot = registry.snapshots().pop().expect("请求片段快照");
    projector
        .project(&requestSnapshot)
        .await
        .expect("活动请求片段应立即可见");
    registry.addTraffic(&sessionId, TrafficDirection::Down, b"response");
    let responseSnapshot = registry.snapshots().pop().expect("响应片段快照");
    projector
        .project(&responseSnapshot)
        .await
        .expect("活动响应片段应立即可见");

    let transaction = recording
        .listMetadata()
        .await
        .expect("读取活动事务")
        .pop()
        .expect("活动事务");
    assert_eq!(transaction.status, TransactionStatus::Pending);
    assert_eq!(transaction.urlDisplay, "https://example.com:443");
    let detail = recording
        .getTransactionDetail(&transaction.transactionId)
        .await
        .expect("读取活动事务详情");
    assert_eq!(detail.requestPackets.len(), 1);
    assert_eq!(detail.responsePackets.len(), 1);
    assert_eq!(detail.requestPackets[0].sequence, 1);
    assert_eq!(detail.responsePackets[0].sequence, 1);
    assert_eq!(detail.requestBody.expect("请求正文").storedBytes, 7);
    assert_eq!(detail.responseBody.expect("响应正文").storedBytes, 8);
}

/// 验证排队事件共享的正文镜像继续增长时，投影只提交事件自己的字节水位和片段范围。
/// 该竞争曾让新片段越过已写正文并返回 captureInvalidBodyLength，最终事务无法进入完成态。
#[tokio::test]
async fn queuedSnapshotIgnoresTrafficAppendedAfterItsEventWatermark() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    let registry = SessionRegistry::withCaptureBudget(8, 64 * 1024);
    let sessionId = registry.create("127.0.0.1:50000".to_owned());
    assert!(registry.update(
        &sessionId,
        SessionUpdate {
            username: None,
            command: Some("connect".to_owned()),
            targetAddress: Some("example.com:443".to_owned()),
            applicationProtocol: Some(SessionApplicationProtocol::Tls),
            state: SessionState::Relaying,
        },
    ));
    projector
        .project(&registry.snapshots().pop().expect("初始活动会话"))
        .await
        .expect("创建活动事务");

    registry.addTraffic(&sessionId, TrafficDirection::Up, b"first");
    let queuedSnapshot = registry.snapshots().pop().expect("排队事件快照");
    registry.addTraffic(&sessionId, TrafficDirection::Up, b"second");
    projector
        .project(&queuedSnapshot)
        .await
        .expect("共享镜像增长不得破坏旧事件投影");

    let transaction = recording
        .listMetadata()
        .await
        .expect("读取活动事务")
        .pop()
        .expect("活动事务");
    let detail = recording
        .getTransactionDetail(&transaction.transactionId)
        .await
        .expect("读取活动事务详情");
    assert_eq!(detail.requestBody.expect("请求正文").storedBytes, 5);
    assert_eq!(detail.requestPackets.len(), 1);
    assert_eq!(detail.requestPackets[0].storedBytes, 5);
}

/// 验证失败连接保留 SOCKS 专用机器码，且重复终态事件不会创建第二条事务。
#[tokio::test]
async fn failedSessionProjectsOnceWithStableError() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    let mut session = sessionSnapshot("connect", "unreachable.example:80");
    projector.project(&session).await.expect("创建失败候选事务");

    session.state = SessionState::Failed;
    session.errorMessage = "连接被拒绝".to_owned();
    session.closedAtMilliseconds = 1_250;
    session.updatedAtMilliseconds = 1_250;
    projector.project(&session).await.expect("提交失败事务");
    projector.project(&session).await.expect("忽略重复终态事件");

    let page = recording
        .pageView(None, 10, None)
        .await
        .expect("读取失败事务");
    assert_eq!(page.total, 1);
    let transaction = &page.transactions[0];
    assert_eq!(transaction.status, TransactionStatus::Failed);
    assert_eq!(
        transaction.error.as_ref().map(|error| error.code.as_str()),
        Some("socksSessionFailed")
    );
}

/// 验证目标已经解析、但在首段协议分类前失败的连接仍生成一条可见失败事务。
/// 这覆盖连接超时、分类读取错误等过去只留在会话诊断、完全不进入事务树的路径。
#[tokio::test]
async fn failedUnclassifiedSessionRemainsVisible() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    let mut session = sessionSnapshot("connect", "unclassified.example:443");
    session.applicationProtocol = SessionApplicationProtocol::Undetermined;
    session.state = SessionState::Failed;
    session.errorMessage = "连接阶段失败".to_owned();
    session.closedAtMilliseconds = 1_250;
    session.updatedAtMilliseconds = 1_250;

    projector
        .project(&session)
        .await
        .expect("未分类失败必须进入事务树");

    let page = recording
        .pageView(None, 10, None)
        .await
        .expect("读取未分类失败事务");
    assert_eq!(page.total, 1);
    assert_eq!(page.transactions[0].status, TransactionStatus::Failed);
    assert_eq!(
        page.transactions[0].urlDisplay,
        "tcp://unclassified.example:443"
    );
    assert_eq!(
        page.transactions[0]
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("socksSessionFailed")
    );
}

/// 验证一个 UDP ASSOCIATE 生命周期只生成一条摘要，数据报数量不会放大事务数。
#[tokio::test]
async fn udpAssociationProjectsAsSingleTransaction() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    let mut session = sessionSnapshot("udpAssociate", "127.0.0.1:53000");
    session.state = SessionState::UdpAssociating;
    projector
        .project(&session)
        .await
        .expect("创建 UDP 关联事务");

    session.bytesUp = 52;
    session.bytesDown = 64;
    session.updatedAtMilliseconds = 1_150;
    projector.project(&session).await.expect("同步 UDP 数据报");
    session.state = SessionState::Closed;
    session.closedAtMilliseconds = 1_200;
    session.updatedAtMilliseconds = 1_200;
    projector
        .project(&session)
        .await
        .expect("完成 UDP 关联事务");

    let page = recording
        .pageView(None, 10, None)
        .await
        .expect("读取 UDP 关联事务");
    assert_eq!(page.total, 1);
    assert_eq!(page.transactions[0].method, "UDP ASSOCIATE");
    assert_eq!(page.transactions[0].urlDisplay, "udp://127.0.0.1:53000");
    assert_eq!(page.transactions[0].sizes.requestBodyBytes, 52);
    assert_eq!(page.transactions[0].sizes.responseBodyBytes, 64);
}

/// 验证未解密 TLS 仍作为可展开原始流录制，但展示地址使用已确认的 HTTPS 而不是 SOCKS5 入口协议。
#[tokio::test]
async fn rawTlsSessionUsesHttpsDisplayWithoutDuplicateHttpTransaction() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    let mut session = sessionSnapshot("connect", "secure.example:443");
    session.applicationProtocol = SessionApplicationProtocol::Tls;
    projector
        .project(&session)
        .await
        .expect("创建原始 TLS 事务");
    session.state = SessionState::Closed;
    session.closedAtMilliseconds = 1_200;
    session.updatedAtMilliseconds = 1_200;
    projector
        .project(&session)
        .await
        .expect("完成原始 TLS 事务");

    let page = recording
        .pageView(None, 10, None)
        .await
        .expect("读取原始 TLS 事务");
    assert_eq!(page.total, 1);
    assert_eq!(page.transactions[0].protocol, TransactionProtocol::Socks);
    assert_eq!(
        page.transactions[0].urlDisplay,
        "https://secure.example:443"
    );
}

/// 验证已由 HTTP/HTTPS 处理器录制的 CONNECT 会话不会再生成重复的原始 SOCKS5 流事务。
#[tokio::test]
async fn classifiedHttpSessionSkipsRawStreamProjection() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    for (suffix, protocol, port) in [
        ("http", SessionApplicationProtocol::Http, 80),
        ("https", SessionApplicationProtocol::Https, 443),
    ] {
        let mut session = sessionSnapshot("connect", &format!("example.com:{port}"));
        session.sessionId = format!("session-{suffix}");
        session.applicationProtocol = protocol;
        session.state = SessionState::Relaying;
        session.bytesUp = 128;
        session.bytesDown = 256;
        projector
            .project(&session)
            .await
            .expect("已解码会话不得生成原始流事务");
        session.state = SessionState::Closed;
        session.closedAtMilliseconds = 1_200;
        projector
            .project(&session)
            .await
            .expect("已解码会话终态不得生成原始流事务");
    }
    let page = recording
        .pageView(None, 10, None)
        .await
        .expect("事务页必须可读取");
    assert_eq!(page.total, 0);
}

/// 验证 clear 后迟到的旧代际事件不会重建已删除事务，新代际会话仍能正常录制。
#[tokio::test]
async fn clearGenerationRejectsQueuedOldSessionsWithoutBlockingNewSessions() {
    let recording = recordingSession().await;
    let mut projector = SocksTransactionProjector::new(recording.clone(), 16);
    let mut oldSession = sessionSnapshot("connect", "old.example:443");
    projector
        .project(&oldSession)
        .await
        .expect("创建清空前事务");

    recording.clearSession().await.expect("清空录制会话");
    projector.advanceCaptureGeneration(1);
    oldSession.state = SessionState::Closed;
    oldSession.closedAtMilliseconds = 1_200;
    oldSession.updatedAtMilliseconds = 1_200;
    projector
        .project(&oldSession)
        .await
        .expect("拒绝清空前迟到终态");

    let mut newSession = sessionSnapshot("connect", "new.example:443");
    newSession.sessionId = "session-new-generation".to_owned();
    newSession.captureGeneration = 1;
    projector
        .project(&newSession)
        .await
        .expect("创建清空后事务");
    newSession.state = SessionState::Closed;
    newSession.closedAtMilliseconds = 1_300;
    newSession.updatedAtMilliseconds = 1_300;
    projector
        .project(&newSession)
        .await
        .expect("完成清空后事务");

    let page = recording
        .pageView(None, 10, None)
        .await
        .expect("读取清空后事务");
    assert_eq!(page.total, 1);
    assert_eq!(page.transactions[0].host, "new.example");
}
