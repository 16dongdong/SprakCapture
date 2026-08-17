#![allow(non_snake_case)]

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use capture_core::{
    BeginTransaction, BodyStorageKind, BodyWrite, CaptureError, HeaderField, MessageSide,
    RecordingConfiguration, RecordingLimits, RecordingLimitsUpdate, RecordingSession,
    RecordingSettingsUpdate, RecordingState, TransactionCompletion, TransactionError,
    TransactionProgressUpdate, TransactionProtocol, TransactionStatus, TransactionUpdate,
    TransactionUserUpdate, currentTimeMilliseconds,
};
use location_core::{LocationPattern, ResolvedLocation};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio::time::timeout;

/// 创建每个测试独占的 spill 根目录和小型录制预算。
fn testConfiguration(temporaryDirectory: &TempDir) -> RecordingConfiguration {
    RecordingConfiguration {
        limits: RecordingLimits {
            maxTransactions: 8,
            maxBodyBytes: 32,
            maxTotalBodyBytes: 128,
        },
        ignoreLocations: Vec::new(),
        recordTunnelMetadata: true,
        memoryBodyThreshold: 8,
        metadataMemoryBudgetBytes: 64 * 1024 * 1024,
        spillDirectory: temporaryDirectory.path().to_path_buf(),
    }
}

/// 创建已解析 HTTP 目标，测试只覆盖录制层而不重复 URL 解析职责。
fn location(host: &str, path: &str) -> ResolvedLocation {
    ResolvedLocation {
        protocol: "http".to_owned(),
        host: host.to_owned(),
        port: 80,
        path: path.to_owned(),
        query: String::new(),
        display: format!("http://{host}{path}"),
    }
}

/// 创建新事务输入；所有字段使用稳定值，便于断言仅关注目标行为。
fn transaction(host: &str, path: &str) -> BeginTransaction {
    BeginTransaction {
        protocol: TransactionProtocol::Http,
        method: "GET".to_owned(),
        location: location(host, path),
        clientAddress: "127.0.0.1:50000".to_owned(),
        clientProcessName: None,
        clientProcessId: None,
        contentType: String::new(),
        startAtMilliseconds: currentTimeMilliseconds(),
    }
}

/// 开始一个必须被录制的事务；None 表示测试前置状态配置错误。
async fn begin(session: &RecordingSession, path: &str) -> String {
    session
        .beginTransaction(transaction("example.com", path))
        .await
        .expect("创建事务")
        .expect("事务应进入录制会话")
}

/// 将测试事务迁移到 complete，使其可参与数量、正文和元数据预算淘汰。
async fn complete(session: &RecordingSession, transactionId: &str) {
    session
        .commit(
            transactionId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: currentTimeMilliseconds(),
                contentType: String::new(),
            },
        )
        .await
        .expect("完成测试事务");
}

/// 写入可进入强实体索引的单字节 206 响应；任一步失败都说明索引前置契约被破坏。
async fn completeRange(session: &RecordingSession, transactionId: &str, entityTag: &str) {
    completeRangeWithHeaders(
        session,
        transactionId,
        vec![
            HeaderField {
                name: "Content-Range".to_owned(),
                value: "bytes 0-0/1".to_owned(),
            },
            HeaderField {
                name: "ETag".to_owned(),
                value: entityTag.to_owned(),
            },
        ],
    )
    .await;
}

/// 用指定响应头完成单字节 206 事务，供重复实体字段等协议边界测试复用。
///
/// 运行上下文：调用方负责构造待验证的原始响应头；正文固定为完整单字节，保证索引拒绝只能
/// 归因于响应头契约。任一步失败都会终止测试，不把录制写入错误误判为索引行为。
async fn completeRangeWithHeaders(
    session: &RecordingSession,
    transactionId: &str,
    responseHeaders: Vec<HeaderField>,
) {
    session
        .storeHeaders(transactionId, MessageSide::Response, responseHeaders)
        .await
        .expect("写入实体响应头");
    session
        .storeBody(
            transactionId,
            MessageSide::Response,
            BodyWrite {
                bytes: vec![0x2a],
                originalBytes: 1,
                contentType: "audio/mp4".to_owned(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("写入实体响应正文");
    session
        .commit(
            transactionId,
            TransactionCompletion {
                statusCode: 206,
                endAtMilliseconds: currentTimeMilliseconds(),
                contentType: "audio/mp4".to_owned(),
            },
        )
        .await
        .expect("提交实体响应");
}

/// 验证重复 ETag 或 Content-Range 不会进入跨事务媒体实体索引。
///
/// 即使重复字段之一语法正确，也不能采用首值继续拼接；否则代理和源站对冲突字段的解释差异
/// 会制造坏媒体。本测试分别覆盖冲突 ETag 与冲突区间，并确认查询结果为空。
#[tokio::test]
async fn responseEntityIndexRejectsDuplicateIdentityHeaders() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");

    let duplicateEntityTagId = begin(&session, "/duplicate-etag").await;
    completeRangeWithHeaders(
        &session,
        &duplicateEntityTagId,
        vec![
            HeaderField {
                name: "Content-Range".to_owned(),
                value: "bytes 0-0/1".to_owned(),
            },
            HeaderField {
                name: "ETag".to_owned(),
                value: "\"generation-a\"".to_owned(),
            },
            HeaderField {
                name: "etag".to_owned(),
                value: "\"generation-b\"".to_owned(),
            },
        ],
    )
    .await;
    assert!(
        session
            .findResponseRangeCandidates(
                "http://example.com/duplicate-etag",
                "\"generation-a\"",
                1,
                "identity",
            )
            .await
            .expect("查询重复 ETag 实体")
            .is_empty()
    );

    let duplicateRangeId = begin(&session, "/duplicate-range").await;
    completeRangeWithHeaders(
        &session,
        &duplicateRangeId,
        vec![
            HeaderField {
                name: "Content-Range".to_owned(),
                value: "bytes 0-0/1".to_owned(),
            },
            HeaderField {
                name: "content-range".to_owned(),
                value: "bytes 0-0/2".to_owned(),
            },
            HeaderField {
                name: "ETag".to_owned(),
                value: "\"generation-c\"".to_owned(),
            },
        ],
    )
    .await;
    assert!(
        session
            .findResponseRangeCandidates(
                "http://example.com/duplicate-range",
                "\"generation-c\"",
                1,
                "identity",
            )
            .await
            .expect("查询重复 Content-Range 实体")
            .is_empty()
    );
}

/// 验证媒体候选索引严格使用完整 URL，协议不同的同主机路径不得进入同一实体集合。
#[tokio::test]
async fn responseCandidatesNeverNormalizeHttpAndHttpsSchemes() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let httpTransactionId = begin(&session, "/same-resource").await;
    let mut httpsTransaction = transaction("example.com", "/same-resource");
    httpsTransaction.location.protocol = "https".to_owned();
    httpsTransaction.location.port = 443;
    httpsTransaction.location.display = "https://example.com/same-resource".to_owned();
    let httpsTransactionId = session
        .beginTransaction(httpsTransaction)
        .await
        .expect("创建 HTTPS 事务")
        .expect("HTTPS 事务应进入录制会话");
    completeRange(&session, &httpTransactionId, "\"entity-generation\"").await;
    completeRange(&session, &httpsTransactionId, "\"entity-generation\"").await;

    let httpCandidates = session
        .findResponseRangeCandidates(
            "http://example.com/same-resource",
            "\"entity-generation\"",
            1,
            "identity",
        )
        .await
        .expect("查询 HTTP 媒体候选");
    assert_eq!(httpCandidates.len(), 1);
    assert_eq!(httpCandidates[0].transactionId, httpTransactionId);
    let httpsCandidates = session
        .findResponseRangeCandidates(
            "https://example.com/same-resource",
            "\"entity-generation\"",
            1,
            "identity",
        )
        .await
        .expect("查询 HTTPS 媒体候选");
    assert_eq!(httpsCandidates.len(), 1);
    assert_eq!(httpsCandidates[0].transactionId, httpsTransactionId);
}

/// 验证一万个同实体重复 Range 通过二级索引查询，最终只需为一个规划分段建立租约。
///
/// 查询必须在固定时限内完成；随后 clear 先于租约线性化时返回 NotFound，已经建立的单租约
/// 仍可读完。该用例同时证明索引清理不会留下可查询的幽灵事务。
#[tokio::test]
async fn responseEntityIndexScalesAndLeasesOnlyFinalSegment() {
    let candidateCount = 10_000_usize;
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxTransactions = candidateCount;
    configuration.limits.maxTotalBodyBytes = candidateCount * 2;
    configuration.metadataMemoryBudgetBytes = 256 * 1024 * 1024;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建大规模索引会话");
    for _ in 0..candidateCount {
        let transactionId = begin(&session, "/indexed-repeat").await;
        completeRange(&session, &transactionId, "\"shared-generation\"").await;
    }

    let candidates = timeout(
        Duration::from_secs(1),
        session.findResponseRangeCandidates(
            "http://example.com/indexed-repeat",
            "\"shared-generation\"",
            1,
            "identity",
        ),
    )
    .await
    .expect("二级索引查询不得退化为全事务阻塞")
    .expect("查询重复实体候选");
    assert_eq!(candidates.len(), candidateCount);
    let finalTransactionId = candidates
        .last()
        .expect("必须存在最终候选")
        .transactionId
        .clone();
    let leases = session
        .getBodyReadLeases(
            std::slice::from_ref(&finalTransactionId),
            MessageSide::Response,
        )
        .await
        .expect("只为最终规划事务建立租约");
    assert_eq!(leases.len(), 1);
    let stableLease = leases.into_iter().next().expect("最终租约");

    session.clearSession().await.expect("清空大规模索引会话");
    assert!(
        session
            .findResponseRangeCandidates(
                "http://example.com/indexed-repeat",
                "\"shared-generation\"",
                1,
                "identity",
            )
            .await
            .expect("clear 后查询索引")
            .is_empty()
    );
    assert!(matches!(
        session
            .getBodyReadLeases(&[finalTransactionId], MessageSide::Response)
            .await,
        Err(CaptureError::TransactionNotFound)
    ));
    assert_eq!(
        stableLease.readRange(0, 1).await.expect("读取既有租约"),
        [0x2a]
    );
}

/// 验证唯一实体索引的 URL、ETag、编码与节点容量纳入权威元数据预算并随 FIFO 同步回收。
#[tokio::test]
async fn responseEntityIndexNeverExceedsMetadataBudget() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxTransactions = 1_000;
    configuration.limits.maxTotalBodyBytes = 2_000;
    configuration.metadataMemoryBudgetBytes = 64 * 1024;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建索引预算会话");
    for index in 0..400 {
        let path = format!("/budgeted-entity-{index:04}");
        let entityTag = format!("\"budget-generation-{index:04}\"");
        let transactionId = begin(&session, &path).await;
        completeRange(&session, &transactionId, &entityTag).await;
        let snapshot = session.snapshot().await.expect("读取索引预算快照");
        assert!(
            snapshot.totalMetadataBytes <= snapshot.metadataMemoryBudgetBytes,
            "二级索引拥有的字符串和树节点必须进入权威预算"
        );
    }
    let snapshot = session.snapshot().await.expect("读取最终索引预算");
    assert!(
        snapshot.droppedCount > 0,
        "紧预算必须通过 FIFO 回收索引实体"
    );
    assert!(
        session
            .findResponseRangeCandidates(
                "http://example.com/budgeted-entity-0000",
                "\"budget-generation-0000\"",
                1,
                "identity",
            )
            .await
            .expect("查询已回收索引实体")
            .is_empty(),
        "事务淘汰必须同步删除二级索引成员"
    );
}

/// 验证流式 spool 跨越内存阈值后仍原子绑定双向完整正文，且摘要不产生截断标记。
#[tokio::test]
async fn streamingSpoolsPersistCompleteBidirectionalBodies() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().to_path_buf(),
        memoryBodyThreshold: 8,
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建录制会话");
    let transactionId = begin(&session, "/streaming-spool").await;
    let requestBytes = (0..(2 * 1024 * 1024 + 17))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let responseBytes = (0..(3 * 1024 * 1024 + 29))
        .map(|index| (index % 239) as u8)
        .collect::<Vec<_>>();
    let mut requestSpool = session
        .createBodySpool(&transactionId, MessageSide::Request)
        .await
        .expect("创建请求 spool");
    let mut responseSpool = session
        .createBodySpool(&transactionId, MessageSide::Response)
        .await
        .expect("创建响应 spool");
    for chunk in requestBytes.chunks(65_537) {
        requestSpool.append(chunk).await.expect("追加请求正文");
    }
    for chunk in responseBytes.chunks(32_771) {
        responseSpool.append(chunk).await.expect("追加响应正文");
    }
    session
        .storeBodySpools(
            &transactionId,
            requestSpool,
            responseSpool,
            "application/octet-stream",
            "binary",
        )
        .await
        .expect("原子绑定双向 spool");
    complete(&session, &transactionId).await;

    let storedRequest = session
        .getBody(&transactionId, MessageSide::Request)
        .await
        .expect("读取请求正文");
    let storedResponse = session
        .getBody(&transactionId, MessageSide::Response)
        .await
        .expect("读取响应正文");
    let responseChunkOffset = 1024 * 1024 - 3;
    let responseChunk = session
        .getBodyChunk(
            &transactionId,
            MessageSide::Response,
            responseChunkOffset,
            17,
        )
        .await
        .expect("按偏移读取 spill 响应分块");
    assert_eq!(
        responseChunk.bytes,
        responseBytes[responseChunkOffset..responseChunkOffset + 17],
        "惰性媒体流只能读取请求区间，且跨文件偏移不得错位"
    );
    let endChunk = session
        .getBodyChunk(
            &transactionId,
            MessageSide::Response,
            responseBytes.len(),
            17,
        )
        .await
        .expect("读取 spill 正文末端空块");
    assert!(endChunk.bytes.is_empty());
    assert_eq!(storedRequest.bytes, requestBytes);
    assert_eq!(storedResponse.bytes, responseBytes);
    assert!(!storedRequest.meta.truncated);
    assert!(!storedResponse.meta.truncated);
    assert_eq!(
        session
            .getBodyStorageKind(&transactionId, MessageSide::Request)
            .await
            .expect("读取请求正文介质"),
        BodyStorageKind::Spill
    );
    let summary = session
        .getTransaction(&transactionId)
        .await
        .expect("读取事务摘要");
    assert!(!summary.flags.bodyTruncated);
    assert_eq!(summary.sizes.requestBodyBytes, requestBytes.len() as u64);
    assert_eq!(summary.sizes.responseBodyBytes, responseBytes.len() as u64);
}

/// 验证其它事务提交正文时不会把仍在写入的透明流 spool 当成孤儿文件删除。
///
/// 运行上下文：多个长连接会并发录制，其中一条完成时会触发孤儿清理；活动 spool 必须继续可写并最终完整提交。
/// 失败语义：活动文件若进入孤儿集合，本测试会在后续追加或原子提交处直接暴露 I/O 错误。
#[tokio::test]
async fn concurrentBodyCleanupPreservesActiveStreamingSpools() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().to_path_buf(),
        memoryBodyThreshold: 1,
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建录制会话");
    let streamingTransactionId = begin(&session, "/active-stream").await;
    let mut requestSpool = session
        .createBodySpool(&streamingTransactionId, MessageSide::Request)
        .await
        .expect("创建活动请求 spool");
    let mut responseSpool = session
        .createBodySpool(&streamingTransactionId, MessageSide::Response)
        .await
        .expect("创建活动响应 spool");
    requestSpool
        .append(b"request-prefix")
        .await
        .expect("写入活动请求前缀");
    responseSpool
        .append(b"response-prefix")
        .await
        .expect("写入活动响应前缀");
    assert_eq!(
        session
            .snapshot()
            .await
            .expect("读取活动 spool 快照")
            .pendingCleanupCount,
        0,
        "仍由写入任务持有的文件不属于孤儿清理项"
    );

    let completedTransactionId = begin(&session, "/completed-body").await;
    session
        .storeBody(
            &completedTransactionId,
            MessageSide::Response,
            BodyWrite {
                bytes: b"completed-body".to_vec(),
                originalBytes: 14,
                contentType: "application/octet-stream".to_owned(),
                encoding: "binary".to_owned(),
            },
        )
        .await
        .expect("提交并发事务正文");

    requestSpool
        .append(b"-suffix")
        .await
        .expect("孤儿清理后继续写入活动请求");
    responseSpool
        .append(b"-suffix")
        .await
        .expect("孤儿清理后继续写入活动响应");
    session
        .storeBodySpools(
            &streamingTransactionId,
            requestSpool,
            responseSpool,
            "application/octet-stream",
            "binary",
        )
        .await
        .expect("提交活动双向 spool");

    assert_eq!(
        session
            .getBody(&streamingTransactionId, MessageSide::Request)
            .await
            .expect("读取活动请求正文")
            .bytes,
        b"request-prefix-suffix"
    );
    assert_eq!(
        session
            .getBody(&streamingTransactionId, MessageSide::Response)
            .await
            .expect("读取活动响应正文")
            .bytes,
        b"response-prefix-suffix"
    );
}

/// 验证录制多字段更新在任一规则无效时保持全部旧值，防止控制 API 暴露部分成功。
#[tokio::test]
async fn settingsUpdateValidatesBeforeAtomicCommit() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let original = session.snapshot().await.expect("读取原始快照");
    let result = session
        .updateSettings(RecordingSettingsUpdate {
            state: Some(RecordingState::Paused),
            limits: Some(RecordingLimitsUpdate {
                maxTransactions: Some(0),
                maxBodyBytes: Some(16),
                maxTotalBodyBytes: Some(32),
            }),
            ignoreLocations: Some(vec![LocationPattern {
                protocol: "http".to_owned(),
                host: "*.example.com".to_owned(),
                port: String::new(),
                path: String::new(),
                query: None,
            }]),
            recordTunnelMetadata: Some(false),
        })
        .await;
    assert!(matches!(result, Err(CaptureError::InvalidLimits)));
    let current = session.snapshot().await.expect("读取失败后的快照");
    assert_eq!(current.state, original.state);
    assert_eq!(current.limits, original.limits);
    assert_eq!(current.ignoreLocations, original.ignoreLocations);
    assert_eq!(current.recordTunnelMetadata, original.recordTunnelMetadata);
}

/// 验证 watch 合并多个快速变化时仍保留最新序号，控制层可据此重新读取最终权威快照。
#[tokio::test]
async fn changeSubscriptionRetainsLatestRevision() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let mut changes = session.subscribeChanges();
    session.pauseRecording().await.expect("暂停录制");
    session.startRecording().await.expect("恢复录制");
    let _transactionId = begin(&session, "/latest").await;
    timeout(Duration::from_secs(1), changes.changed())
        .await
        .expect("等待变化超时")
        .expect("变化发送端提前关闭");
    assert_eq!(*changes.borrow_and_update(), 3);
    assert_eq!(
        session
            .snapshot()
            .await
            .expect("读取最终快照")
            .transactionCount,
        1
    );
}

/// 验证并发事务写入期间有界分页视图始终满足统计数量、总数和明确页长约束。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recordingPageViewKeepsCountAndMetadataLinearized() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxTransactions = 256;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let writingSession = session.clone();
    let writer = tokio::spawn(async move {
        for index in 0..128 {
            writingSession
                .beginTransaction(transaction("example.com", &format!("/{index}")))
                .await
                .expect("并发创建事务");
        }
    });
    for _ in 0..128 {
        let view = session
            .pageView(None, 8, None)
            .await
            .expect("读取有界分页视图");
        assert_eq!(view.recording.transactionCount, view.total);
        assert!(view.transactions.len() <= 8);
    }
    writer.await.expect("写入任务不应 panic");
    let finalView = session
        .pageView(None, 8, None)
        .await
        .expect("读取最终分页视图");
    assert_eq!(finalView.recording.transactionCount, 128);
    assert_eq!(finalView.total, 128);
    assert_eq!(finalView.transactions.len(), 8);
}

/// 验证尾部追加保持分页 offset 稳定，而 FIFO 淘汰与 clear 会使旧代际失效。
#[tokio::test]
async fn collectionTokenAllowsTailAppendAndRejectsDestructiveChanges() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxTransactions = 2;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let emptyToken = session
        .pageView(Some(0), 1, None)
        .await
        .expect("读取空集合")
        .collectionToken;
    let firstId = begin(&session, "/first").await;
    let appendedPage = session
        .pageView(Some(0), 1, Some(&emptyToken))
        .await
        .expect("尾部追加不得使既有 offset 失效");
    assert_eq!(appendedPage.collectionToken, emptyToken);
    assert_eq!(appendedPage.transactions[0].transactionId, firstId);
    session
        .commit(
            &firstId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: currentTimeMilliseconds(),
                contentType: String::new(),
            },
        )
        .await
        .expect("完成首个事务");
    let firstPage = session
        .pageView(Some(0), 1, None)
        .await
        .expect("读取首个集合");
    let firstToken = firstPage.collectionToken;
    let secondId = begin(&session, "/second").await;
    let stableFirstPage = session
        .pageView(Some(0), 1, Some(&firstToken))
        .await
        .expect("第二次尾部追加仍应保留首个 offset");
    assert_eq!(stableFirstPage.transactions[0].transactionId, firstId);
    session
        .commit(
            &secondId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: currentTimeMilliseconds(),
                contentType: String::new(),
            },
        )
        .await
        .expect("完成第二个事务");
    let thirdId = begin(&session, "/third").await;
    assert!(matches!(
        session.pageView(Some(0), 1, Some(&firstToken)).await,
        Err(CaptureError::CollectionChanged)
    ));
    assert!(matches!(
        session.getTransaction(&firstId).await,
        Err(CaptureError::TransactionNotFound)
    ));
    assert!(session.getTransaction(&secondId).await.is_ok());
    assert!(session.getTransaction(&thirdId).await.is_ok());

    let secondPage = session
        .pageView(Some(0), 1, None)
        .await
        .expect("读取 FIFO 后集合");
    let secondToken = secondPage.collectionToken;
    session.clearSession().await.expect("清空集合");
    assert!(matches!(
        session.pageView(Some(0), 1, Some(&secondToken)).await,
        Err(CaptureError::CollectionChanged)
    ));
}

/// 验证高频尾部写入不会打断使用旧代际顺序读取的历史页；容量足够时每个既有 offset 始终指向同一事务。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paginationTokenRemainsUsableDuringConcurrentTailAppends() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxTransactions = 512;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    for index in 0..64 {
        begin(&session, &format!("/existing/{index}")).await;
    }
    let firstPage = session
        .pageView(Some(0), 16, None)
        .await
        .expect("读取首个历史页");
    let collectionToken = firstPage.collectionToken.clone();
    let expectedIds = firstPage
        .transactions
        .iter()
        .map(|transaction| transaction.transactionId.clone())
        .collect::<Vec<_>>();
    let writingSession = session.clone();
    let writer = tokio::spawn(async move {
        for index in 0..256 {
            begin(&writingSession, &format!("/incoming/{index}")).await;
        }
    });
    for _ in 0..128 {
        let page = session
            .pageView(Some(0), 16, Some(&collectionToken))
            .await
            .expect("尾部并发写入期间旧分页代际必须持续可用");
        let actualIds = page
            .transactions
            .iter()
            .map(|transaction| transaction.transactionId.clone())
            .collect::<Vec<_>>();
        assert_eq!(actualIds, expectedIds);
        tokio::task::yield_now().await;
    }
    writer.await.expect("并发写入任务不应 panic");
}

/// 构造不依赖具体语言的结构化事务错误。
fn transactionFailure() -> TransactionError {
    TransactionError {
        code: "upstreamUnavailable".to_owned(),
        messageKey: "error.httpProxy.upstreamUnavailable".to_owned(),
        params: BTreeMap::from([("host".to_owned(), "example.com".to_owned())]),
    }
}

/// 统计临时根目录内所有正文文件，验证 clear 与替换真实释放磁盘资源。
fn countBodyFiles(temporaryDirectory: &TempDir) -> usize {
    std::fs::read_dir(temporaryDirectory.path())
        .expect("读取临时根目录")
        .flat_map(|entry| {
            let path = entry.expect("读取会话目录项").path();
            if path.is_dir() {
                std::fs::read_dir(path)
                    .expect("读取会话 spill 目录")
                    .map(|nested| nested.expect("读取正文文件").path())
                    .collect::<Vec<_>>()
            } else {
                vec![path]
            }
        })
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "body")
        })
        .count()
}

/// 验证暂停只阻止新事务，恢复后继续使用同一 RecordingSession。
#[tokio::test]
async fn pauseAndResumeControlOnlyNewTransactions() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    session.pauseRecording().await.expect("暂停录制");
    assert_eq!(
        session.snapshot().await.expect("读取快照").state,
        RecordingState::Paused
    );
    assert!(
        session
            .beginTransaction(transaction("example.com", "/paused"))
            .await
            .expect("暂停状态应正常判定")
            .is_none()
    );
    session.startRecording().await.expect("恢复录制");
    assert!(
        session
            .beginTransaction(transaction("example.com", "/recording"))
            .await
            .expect("录制状态应正常创建")
            .is_some()
    );
}

/// 验证暂停后已存在的 pending 事务仍能写正文并完成。
#[tokio::test]
async fn pendingTransactionCompletesWhileRecordingPaused() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/pending").await;
    session.pauseRecording().await.expect("暂停录制");
    session
        .storeBody(
            &transactionId,
            MessageSide::Response,
            BodyWrite {
                bytes: b"response".to_vec(),
                originalBytes: 8,
                contentType: "text/plain".to_owned(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("写入 pending 正文");
    session
        .commit(
            &transactionId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: currentTimeMilliseconds(),
                contentType: "text/plain".to_owned(),
            },
        )
        .await
        .expect("完成 pending 事务");
    assert_eq!(
        session
            .getTransaction(&transactionId)
            .await
            .expect("读取事务")
            .status,
        TransactionStatus::Complete
    );
}

/// 验证忽略列表复用 Location 通配语义，未命中目标仍被录制。
#[tokio::test]
async fn ignoreLocationsSuppressOnlyMatchingTargets() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.ignoreLocations = vec![LocationPattern {
        host: "*.ignored.example".to_owned(),
        ..LocationPattern::default()
    }];
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    assert!(
        !session
            .shouldRecord(&location("cdn.ignored.example", "/asset"))
            .await
            .expect("匹配忽略规则")
    );
    assert!(
        session
            .beginTransaction(transaction("cdn.ignored.example", "/asset"))
            .await
            .expect("忽略事务")
            .is_none()
    );
    assert!(
        session
            .beginTransaction(transaction("allowed.example", "/asset"))
            .await
            .expect("创建允许事务")
            .is_some()
    );
}

/// 验证事务数量超限时 FIFO 删除最旧事务并累计 droppedCount。
#[tokio::test]
async fn transactionLimitEvictsOldestMetadata() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxTransactions = 2;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let first = begin(&session, "/first").await;
    let second = begin(&session, "/second").await;
    complete(&session, &first).await;
    let third = begin(&session, "/third").await;
    assert!(matches!(
        session.getTransaction(&first).await,
        Err(CaptureError::TransactionNotFound)
    ));
    let metadata = session.listMetadata().await.expect("列出事务");
    assert_eq!(
        metadata
            .iter()
            .map(|summary| summary.transactionId.as_str())
            .collect::<Vec<_>>(),
        vec![second.as_str(), third.as_str()]
    );
    assert_eq!(session.snapshot().await.expect("读取快照").droppedCount, 1);
}

/// 验证正文单体限额、线上原始大小、截断标记和按需头体读取保持一致。
#[tokio::test]
async fn bodyLimitTruncatesStoredBytesWithoutChangingWireSize() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxBodyBytes = 4;
    configuration.memoryBodyThreshold = 16;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/body").await;
    let headers = vec![
        HeaderField {
            name: "Set-Cookie".to_owned(),
            value: "a=1".to_owned(),
        },
        HeaderField {
            name: "Set-Cookie".to_owned(),
            value: "b=2".to_owned(),
        },
    ];
    session
        .storeHeaders(&transactionId, MessageSide::Response, headers.clone())
        .await
        .expect("存储响应头");
    let meta = session
        .storeBody(
            &transactionId,
            MessageSide::Response,
            BodyWrite {
                bytes: b"0123456789".to_vec(),
                originalBytes: 10,
                contentType: "text/plain".to_owned(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("存储响应体");
    assert_eq!(meta.storedBytes, 4);
    assert!(meta.truncated);
    assert_eq!(
        session
            .getHeaders(&transactionId, MessageSide::Response)
            .await
            .expect("读取响应头"),
        headers
    );
    let body = session
        .getBody(&transactionId, MessageSide::Response)
        .await
        .expect("读取响应体");
    assert_eq!(body.bytes, b"0123");
    let summary = session
        .getTransaction(&transactionId)
        .await
        .expect("读取事务");
    assert_eq!(summary.sizes.responseBodyBytes, 10);
    assert!(summary.flags.bodyTruncated);
}

/// 验证事务列表 JSON 没有头、正文引用或正文原始字节字段。
#[tokio::test]
async fn metadataSerializationNeverLeaksHeadersOrBodies() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/metadata").await;
    session
        .storeHeaders(
            &transactionId,
            MessageSide::Request,
            vec![HeaderField {
                name: "Authorization".to_owned(),
                value: "secret".to_owned(),
            }],
        )
        .await
        .expect("存储请求头");
    session
        .storeBody(
            &transactionId,
            MessageSide::Request,
            BodyWrite {
                bytes: b"secret-body".to_vec(),
                originalBytes: 11,
                contentType: "text/plain".to_owned(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("存储请求体");
    let serialized =
        serde_json::to_string(&session.listMetadata().await.expect("列出元数据")).expect("序列化");
    assert!(!serialized.contains("Authorization"));
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("bodyRef"));
    assert!(!serialized.contains("bytes"));
}

/// 验证大正文写入 spill，clear 返回时文件与事务都已不可见。
#[tokio::test]
async fn spillBodyIsDeletedByClearSession() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.memoryBodyThreshold = 2;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/spill").await;
    session
        .storeBody(
            &transactionId,
            MessageSide::Response,
            BodyWrite {
                bytes: b"spill-content".to_vec(),
                originalBytes: 13,
                contentType: "application/octet-stream".to_owned(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("写入 spill");
    assert_eq!(
        session
            .getBodyStorageKind(&transactionId, MessageSide::Response)
            .await
            .expect("读取存储介质"),
        BodyStorageKind::Spill
    );
    assert_eq!(countBodyFiles(&temporaryDirectory), 1);
    session.clearSession().await.expect("清空录制会话");
    assert_eq!(countBodyFiles(&temporaryDirectory), 0);
    assert!(matches!(
        session.getTransaction(&transactionId).await,
        Err(CaptureError::TransactionNotFound)
    ));
}

/// 验证稳定正文租约在 FIFO 淘汰和 clear 删除 spill 路径后仍能逐字节读完声明正文。
///
/// 运行上下文：读任务先消费首字节并暂停，主任务随后触发事务数量淘汰和全量清空，确认
/// 事务与文件路径已经不可见后再恢复读取。失败语义要求任一短读、NotFound 或字节错位直接失败。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bodyReadLeaseSurvivesFifoEvictionAndClear() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.memoryBodyThreshold = 1;
    configuration.limits.maxTransactions = 1;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/leased-spill").await;
    let expectedBytes = b"stable-spill-body".to_vec();
    session
        .storeBody(
            &transactionId,
            MessageSide::Response,
            BodyWrite {
                bytes: expectedBytes.clone(),
                originalBytes: expectedBytes.len() as u64,
                contentType: "application/octet-stream".to_owned(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("写入租约 spill");
    complete(&session, &transactionId).await;
    let lease = session
        .getBodyReadLease(&transactionId, MessageSide::Response)
        .await
        .expect("建立稳定正文租约");
    assert_eq!(lease.storageKind(), BodyStorageKind::Spill);

    let firstByteRead = Arc::new(Notify::new());
    let resumeRead = Arc::new(Notify::new());
    let readerExpectedBytes = expectedBytes.clone();
    let readerFirstByteRead = Arc::clone(&firstByteRead);
    let readerResumeRead = Arc::clone(&resumeRead);
    let reader = tokio::spawn(async move {
        let mut actualBytes = lease.readRange(0, 1).await.expect("读取租约首字节");
        readerFirstByteRead.notify_one();
        readerResumeRead.notified().await;
        for offset in 1..readerExpectedBytes.len() {
            actualBytes.extend(
                lease
                    .readRange(offset, 1)
                    .await
                    .expect("淘汰与清空后逐字节读取租约"),
            );
        }
        actualBytes
    });
    firstByteRead.notified().await;

    let replacementId = begin(&session, "/replacement").await;
    assert!(matches!(
        session.getTransaction(&transactionId).await,
        Err(CaptureError::TransactionNotFound)
    ));
    session.clearSession().await.expect("持有租约时清空会话");
    assert!(matches!(
        session.getTransaction(&replacementId).await,
        Err(CaptureError::TransactionNotFound)
    ));
    assert_eq!(countBodyFiles(&temporaryDirectory), 0);

    resumeRead.notify_one();
    let actualBytes = reader.await.expect("租约读任务不应 panic");
    assert_eq!(actualBytes.len(), expectedBytes.len());
    assert_eq!(actualBytes, expectedBytes);
}

/// 验证替换 spill 正文后旧文件被回收且总预算只统计新正文。
#[tokio::test]
async fn replacingSpillBodyReclaimsPreviousFileAndBudget() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.memoryBodyThreshold = 1;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/replace").await;
    for bytes in [b"first-body".as_slice(), b"new".as_slice()] {
        session
            .storeBody(
                &transactionId,
                MessageSide::Response,
                BodyWrite {
                    bytes: bytes.to_vec(),
                    originalBytes: bytes.len() as u64,
                    contentType: String::new(),
                    encoding: "identity".to_owned(),
                },
            )
            .await
            .expect("替换 spill 正文");
        assert_eq!(countBodyFiles(&temporaryDirectory), 1);
    }
    assert_eq!(
        session
            .snapshot()
            .await
            .expect("读取正文预算")
            .totalBodyBytes,
        3
    );
    assert_eq!(
        session
            .getBody(&transactionId, MessageSide::Response)
            .await
            .expect("读取新正文")
            .bytes,
        b"new"
    );
}

/// 验证总正文预算按 FIFO 淘汰完整事务，而不是保留失去正文的孤立元数据。
#[tokio::test]
async fn totalBodyBudgetEvictsOldestWholeTransaction() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxTotalBodyBytes = 6;
    configuration.limits.maxBodyBytes = 6;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let first = begin(&session, "/first").await;
    session
        .storeBody(
            &first,
            MessageSide::Response,
            BodyWrite {
                bytes: b"1111".to_vec(),
                originalBytes: 4,
                contentType: String::new(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("写入首个正文");
    complete(&session, &first).await;
    let second = begin(&session, "/second").await;
    session
        .storeBody(
            &second,
            MessageSide::Response,
            BodyWrite {
                bytes: b"2222".to_vec(),
                originalBytes: 4,
                contentType: String::new(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("写入第二个正文");
    assert!(matches!(
        session.getTransaction(&first).await,
        Err(CaptureError::TransactionNotFound)
    ));
    let snapshot = session.snapshot().await.expect("读取快照");
    assert_eq!(snapshot.transactionCount, 1);
    assert_eq!(snapshot.totalBodyBytes, 4);
    assert_eq!(snapshot.droppedCount, 1);
}

/// 验证单事务正文超过全局预算时只存可用前缀，不错误淘汰当前事务。
#[tokio::test]
async fn singleBodyIsTruncatedToTotalBudget() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxBodyBytes = 16;
    configuration.limits.maxTotalBodyBytes = 3;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/limited").await;
    let meta = session
        .storeBody(
            &transactionId,
            MessageSide::Request,
            BodyWrite {
                bytes: b"abcdef".to_vec(),
                originalBytes: 6,
                contentType: String::new(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("存储受总预算限制的正文");
    assert_eq!(meta.storedBytes, 3);
    assert_eq!(
        session
            .getBody(&transactionId, MessageSide::Request)
            .await
            .expect("读取正文")
            .bytes,
        b"abc"
    );
}

/// 验证运行期限额修改被拒绝，已经录制的事务保持完整且不会触发 FIFO 删除。
#[tokio::test]
async fn changingLimitsCannotDeleteExistingTransactions() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let first = begin(&session, "/one").await;
    let second = begin(&session, "/two").await;
    let _third = begin(&session, "/three").await;
    complete(&session, &first).await;
    complete(&session, &second).await;
    let result = session
        .setLimits(RecordingLimits {
            maxTransactions: 1,
            maxBodyBytes: 32,
            maxTotalBodyBytes: 128,
        })
        .await;
    assert!(matches!(result, Err(CaptureError::InvalidLimits)));
    let metadata = session.listMetadata().await.expect("列出事务");
    assert_eq!(metadata.len(), 3);
    assert_eq!(session.snapshot().await.expect("读取快照").droppedCount, 0);
}

/// 验证多个代理任务并发建事务时 sequence 唯一、连续且无状态丢失。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrentWritersReceiveUniqueSequences() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.limits.maxTransactions = 128;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let tasks = (0..64)
        .map(|index| {
            let session = session.clone();
            tokio::spawn(async move {
                session
                    .beginTransaction(transaction("example.com", &format!("/{index}")))
                    .await
                    .expect("并发创建事务")
                    .expect("并发事务应被录制")
            })
        })
        .collect::<Vec<_>>();
    for task in tasks {
        task.await.expect("并发任务不应 panic");
    }
    let metadata = session.listMetadata().await.expect("列出并发事务");
    assert_eq!(metadata.len(), 64);
    assert_eq!(
        metadata
            .iter()
            .map(|summary| summary.sequence)
            .collect::<Vec<_>>(),
        (1..=64).collect::<Vec<_>>()
    );
}

/// 验证请求与响应阶段并发更新不同进度字段时不会发生读改写覆盖。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrentProgressUpdatesMergeWithoutLostFields() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/progress").await;
    let requestSession = session.clone();
    let requestId = transactionId.clone();
    let requestTask = tokio::spawn(async move {
        requestSession
            .updateProgress(
                &requestId,
                TransactionProgressUpdate {
                    requestHeaderBytes: Some(111),
                    requestSentAtMilliseconds: Some(222),
                    ..TransactionProgressUpdate::default()
                },
            )
            .await
    });
    let responseSession = session.clone();
    let responseId = transactionId.clone();
    let responseTask = tokio::spawn(async move {
        responseSession
            .updateProgress(
                &responseId,
                TransactionProgressUpdate {
                    responseHeaderBytes: Some(333),
                    responseStartAtMilliseconds: Some(444),
                    ..TransactionProgressUpdate::default()
                },
            )
            .await
    });
    requestTask
        .await
        .expect("请求进度任务不应 panic")
        .expect("更新请求进度");
    responseTask
        .await
        .expect("响应进度任务不应 panic")
        .expect("更新响应进度");
    let summary = session
        .getTransaction(&transactionId)
        .await
        .expect("读取合并进度");
    assert_eq!(summary.sizes.requestHeaderBytes, 111);
    assert_eq!(summary.sizes.responseHeaderBytes, 333);
    assert_eq!(summary.timings.requestSentAtMilliseconds, Some(222));
    assert_eq!(summary.timings.responseStartAtMilliseconds, Some(444));
}

/// 验证终态冻结协议观测字段和传输进度，但备注、标签与工具记录仍可由用户维护。
#[tokio::test]
async fn finalStateRejectsProtocolUpdatesButAllowsUserFields() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/final-fields").await;
    session
        .commit(
            &transactionId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: currentTimeMilliseconds(),
                contentType: "text/plain".to_owned(),
            },
        )
        .await
        .expect("完成事务");
    assert!(matches!(
        session
            .update(
                &transactionId,
                TransactionUpdate {
                    statusCode: Some(201),
                    ..TransactionUpdate::default()
                }
            )
            .await,
        Err(CaptureError::TransactionFinished)
    ));
    assert!(matches!(
        session
            .updateProgress(
                &transactionId,
                TransactionProgressUpdate {
                    responseBodyBytes: Some(99),
                    ..TransactionProgressUpdate::default()
                }
            )
            .await,
        Err(CaptureError::TransactionFinished)
    ));
    session
        .updateUserFields(
            &transactionId,
            TransactionUserUpdate {
                notes: Some("已复核".to_owned()),
                tags: Some(vec!["重点".to_owned()]),
                appliedTools: Some(vec!["重放器".to_owned()]),
            },
        )
        .await
        .expect("终态更新用户字段");
    let summary = session
        .getTransaction(&transactionId)
        .await
        .expect("读取终态事务");
    assert_eq!(summary.statusCode, Some(200));
    assert_eq!(summary.sizes.responseBodyBytes, 0);
    assert_eq!(summary.notes, "已复核");
    assert_eq!(summary.tags, ["重点"]);
    assert_eq!(summary.appliedTools, ["重放器"]);
}

/// 验证写 spill、clear 和 close 在同一会话串行完成且不会死锁或留下可写状态。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrentSpillClearAndCloseHaveLinearCompletion() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.memoryBodyThreshold = 1;
    configuration.limits.maxTransactions = 64;
    configuration.limits.maxTotalBodyBytes = 1_024;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let mut transactionIds = Vec::new();
    for index in 0..16 {
        transactionIds.push(begin(&session, &format!("/{index}")).await);
    }
    let writers = transactionIds
        .into_iter()
        .map(|transactionId| {
            let session = session.clone();
            tokio::spawn(async move {
                session
                    .storeBody(
                        &transactionId,
                        MessageSide::Response,
                        BodyWrite {
                            bytes: vec![7; 16],
                            originalBytes: 16,
                            contentType: String::new(),
                            encoding: "identity".to_owned(),
                        },
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();
    let clearingSession = session.clone();
    let clearTask = tokio::spawn(async move { clearingSession.clearSession().await });
    timeout(Duration::from_secs(5), async {
        for writer in writers {
            let writeResult = writer.await.expect("正文写任务不应 panic");
            assert!(
                writeResult.is_ok()
                    || matches!(writeResult, Err(CaptureError::TransactionNotFound))
            );
        }
        clearTask
            .await
            .expect("清空任务不应 panic")
            .expect("并发清空");
        session.close().await.expect("清空后关闭");
    })
    .await
    .expect("写入、清空、关闭发生死锁");
    assert_eq!(countBodyFiles(&temporaryDirectory), 0);
    assert_eq!(
        std::fs::read_dir(temporaryDirectory.path())
            .expect("读取关闭后的 spill 根目录")
            .count(),
        0
    );
    assert!(matches!(
        session.snapshot().await,
        Err(CaptureError::SessionClosed)
    ));
}

/// 验证 fail 保存结构化错误且终态不能再次 commit。
#[tokio::test]
async fn failureUsesStructuredErrorAndFinalStateIsImmutable() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/failed").await;
    session
        .fail(
            &transactionId,
            transactionFailure(),
            currentTimeMilliseconds(),
        )
        .await
        .expect("标记事务失败");
    let summary = session
        .getTransaction(&transactionId)
        .await
        .expect("读取失败事务");
    assert_eq!(summary.status, TransactionStatus::Failed);
    assert_eq!(
        summary.error.expect("结构化错误").messageKey,
        "error.httpProxy.upstreamUnavailable"
    );
    assert!(matches!(
        session
            .commit(
                &transactionId,
                TransactionCompletion {
                    statusCode: 200,
                    endAtMilliseconds: currentTimeMilliseconds(),
                    contentType: String::new(),
                }
            )
            .await,
        Err(CaptureError::TransactionFinished)
    ));
    assert!(matches!(
        session
            .update(
                &transactionId,
                TransactionUpdate {
                    contentType: Some("application/json".to_owned()),
                    ..TransactionUpdate::default()
                }
            )
            .await,
        Err(CaptureError::TransactionFinished)
    ));
    session
        .updateUserFields(
            &transactionId,
            TransactionUserUpdate {
                notes: Some("失败记录已复核".to_owned()),
                ..TransactionUserUpdate::default()
            },
        )
        .await
        .expect("失败终态应允许用户备注");
    assert_eq!(
        session
            .getTransaction(&transactionId)
            .await
            .expect("读取失败终态备注")
            .notes,
        "失败记录已复核"
    );
}

/// 验证取消使用独立终态且与 complete/failed 一样冻结所有协议观测字段。
#[tokio::test]
async fn cancellationUsesDedicatedImmutableFinalState() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/cancelled").await;
    session
        .cancel(
            &transactionId,
            transactionFailure(),
            currentTimeMilliseconds(),
        )
        .await
        .expect("取消事务");
    let summary = session
        .getTransaction(&transactionId)
        .await
        .expect("读取取消事务");
    assert_eq!(summary.status, TransactionStatus::Cancelled);
    assert!(summary.timings.endAtMilliseconds.is_some());
    assert!(matches!(
        session
            .updateProgress(
                &transactionId,
                TransactionProgressUpdate {
                    responseBodyBytes: Some(1),
                    ..TransactionProgressUpdate::default()
                }
            )
            .await,
        Err(CaptureError::TransactionFinished)
    ));
    assert!(matches!(
        session
            .fail(
                &transactionId,
                transactionFailure(),
                currentTimeMilliseconds()
            )
            .await,
        Err(CaptureError::TransactionFinished)
    ));
}

/// 验证隧道元数据开关统一控制 HTTP CONNECT 与 SOCKS 会话投影，不影响既有事务。
#[tokio::test]
async fn disabledTunnelMetadataSkipsTunnelAndSocksTransactions() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut configuration = testConfiguration(&temporaryDirectory);
    configuration.recordTunnelMetadata = false;
    let session = RecordingSession::new(configuration)
        .await
        .expect("创建录制会话");
    let mut tunnel = transaction("example.com", "/");
    tunnel.protocol = TransactionProtocol::Tunnel;
    tunnel.method = "CONNECT".to_owned();
    assert!(
        session
            .beginTransaction(tunnel)
            .await
            .expect("应用隧道录制开关")
            .is_none()
    );
    let mut socks = transaction("example.com", "/");
    socks.protocol = TransactionProtocol::Socks;
    socks.method = "CONNECT".to_owned();
    assert!(
        session
            .beginTransaction(socks)
            .await
            .expect("应用 SOCKS 录制开关")
            .is_none()
    );
}

/// 验证无效资源预算和流式长度不一致返回稳定机器错误。
#[tokio::test]
async fn invalidConfigurationAndBodyLengthReturnStableCodes() {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let mut invalidConfiguration = testConfiguration(&temporaryDirectory);
    invalidConfiguration.limits.maxTransactions = 0;
    let error = match RecordingSession::new(invalidConfiguration).await {
        Ok(_) => panic!("零事务预算不应创建会话"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "captureInvalidLimits");

    let mut invalidMetadataBudget = testConfiguration(&temporaryDirectory);
    invalidMetadataBudget.metadataMemoryBudgetBytes = 1;
    let error = match RecordingSession::new(invalidMetadataBudget).await {
        Ok(_) => panic!("过小元数据预算不应创建会话"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "captureInvalidMetadataMemoryBudget");

    let session = RecordingSession::new(testConfiguration(&temporaryDirectory))
        .await
        .expect("创建录制会话");
    let transactionId = begin(&session, "/invalid-length").await;
    let error = session
        .storeBody(
            &transactionId,
            MessageSide::Request,
            BodyWrite {
                bytes: b"too-long".to_vec(),
                originalBytes: 1,
                contentType: String::new(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect_err("原始长度小于缓冲长度应失败");
    assert_eq!(error.messageKey(), "error.capture.invalidBodyLength");
}
