#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use capture_core::*;
use location_core::ResolvedLocation;
use tempfile::TempDir;

const minimumTransactionMetadataBytes: usize = 64 * 1024;

/// 返回测试事务的 pending 预算；额外空间覆盖记录结构与两份索引标识，不依赖私有容器布局。
fn testMetadataBudget(transactionSlots: usize, headerBudgetBytes: usize) -> usize {
    minimumTransactionMetadataBytes
        .saturating_add(2 * 1024)
        .saturating_mul(transactionSlots.max(1))
        .saturating_add(headerBudgetBytes)
}

/// 创建使用极小内存阈值的独占会话，确保测试正文稳定进入 spill 路径。
async fn spillSession(maxTransactions: usize) -> (TempDir, RecordingSession) {
    let temporaryDirectory = tempfile::tempdir().expect("创建临时目录");
    let session = RecordingSession::new(RecordingConfiguration {
        limits: RecordingLimits {
            maxTransactions,
            maxBodyBytes: 4_096,
            maxTotalBodyBytes: 16_384,
        },
        ignoreLocations: Vec::new(),
        recordingRules: RecordingRuleConfiguration::default(),
        recordTunnelMetadata: true,
        memoryBodyThreshold: 1,
        metadataMemoryBudgetBytes: testMetadataBudget(maxTransactions, 0),
        spillDirectory: temporaryDirectory.path().to_path_buf(),
    })
    .await
    .expect("创建 spill 会话");
    (temporaryDirectory, session)
}

/// 创建带精确摘要槽位和少量头预算的会话，用于验证元数据上界且避免测试分配大内存。
async fn metadataBudgetSession(
    transactionSlots: usize,
    headerBudgetBytes: usize,
) -> (TempDir, RecordingSession) {
    let temporaryDirectory = tempfile::tempdir().expect("创建元数据预算临时目录");
    let session = RecordingSession::new(RecordingConfiguration {
        limits: RecordingLimits {
            maxTransactions: transactionSlots,
            maxBodyBytes: 4_096,
            maxTotalBodyBytes: 16_384,
        },
        ignoreLocations: Vec::new(),
        recordingRules: RecordingRuleConfiguration::default(),
        recordTunnelMetadata: true,
        memoryBodyThreshold: 1,
        metadataMemoryBudgetBytes: testMetadataBudget(transactionSlots, headerBudgetBytes),
        spillDirectory: temporaryDirectory.path().to_path_buf(),
    })
    .await
    .expect("创建元数据预算会话");
    (temporaryDirectory, session)
}

/// 构造固定协议字段的测试事务，避免各清理用例复制与目标无关的样板。
fn testTransaction(path: &str) -> BeginTransaction {
    BeginTransaction {
        protocol: TransactionProtocol::Http,
        method: "GET".to_owned(),
        location: ResolvedLocation {
            protocol: "http".to_owned(),
            host: "example.test".to_owned(),
            port: 80,
            path: path.to_owned(),
            query: String::new(),
            display: format!("http://example.test{path}"),
        },
        clientAddress: "127.0.0.1:50000".to_owned(),
        clientProcessName: None,
        clientProcessId: None,
        contentType: String::new(),
        startAtMilliseconds: capture_core::currentTimeMilliseconds(),
    }
}

/// 创建必定录制的事务，固定字段使测试只关注清理和取消语义。
async fn beginTestTransaction(session: &RecordingSession, path: &str) -> String {
    session
        .beginTransaction(testTransaction(path))
        .await
        .expect("创建测试事务")
        .expect("测试事务应进入录制")
}

/// 验证请求工具可在事务仍为 pending 时原子替换方法与完整目标，列表和详情不得继续展示客户端原始地址。
#[tokio::test]
async fn pendingTransactionUsesFinalRequestIdentity() {
    let (_temporaryDirectory, session) = spillSession(4).await;
    let transactionId = beginTestTransaction(&session, "/before").await;

    session
        .update(
            &transactionId,
            TransactionUpdate {
                method: Some("POST".to_owned()),
                location: Some(ResolvedLocation {
                    protocol: "https".to_owned(),
                    host: "mapped.example".to_owned(),
                    port: 8443,
                    path: "/after".to_owned(),
                    query: "mode=final".to_owned(),
                    display: "https://mapped.example:8443/after?mode=final".to_owned(),
                }),
                ..TransactionUpdate::default()
            },
        )
        .await
        .expect("最终请求身份必须写入");

    let summary = session
        .listMetadata()
        .await
        .expect("事务摘要必须可读")
        .into_iter()
        .find(|summary| summary.transactionId == transactionId)
        .expect("目标事务必须存在");
    assert_eq!(summary.method, "POST");
    assert_eq!(summary.host, "mapped.example");
    assert_eq!(summary.port, 8443);
    assert_eq!(summary.path, "/after");
    assert_eq!(summary.query, "mode=final");
    assert_eq!(
        summary.urlDisplay,
        "https://mapped.example:8443/after?mode=final"
    );
}

/// 写入大于内存阈值的响应正文，并返回公开元信息供调用方继续断言。
async fn storeTestSpill(session: &RecordingSession, transactionId: &str) -> BodyHandleMeta {
    session
        .storeBody(
            transactionId,
            MessageSide::Response,
            BodyWrite {
                bytes: b"spill-body".to_vec(),
                originalBytes: 10,
                contentType: "application/octet-stream".to_owned(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("写入测试 spill")
}

/// 递归统计会话临时根目录中的正文文件，覆盖 pending 与最终文件名。
fn countBodyFiles(temporaryDirectory: &TempDir) -> usize {
    std::fs::read_dir(temporaryDirectory.path())
        .expect("读取 spill 根目录")
        .flat_map(|entry| {
            let path = entry.expect("读取 spill 根目录项").path();
            if path.is_dir() {
                std::fs::read_dir(path)
                    .expect("读取会话 spill 目录")
                    .map(|nested| nested.expect("读取正文目录项").path())
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

/// 验证超过内存阈值的正文可通过公开读取接口取回，清空会话后对应 spill 文件被同步回收。
#[tokio::test]
async fn spillBodyCanBeReadAndCleared() {
    let (temporaryDirectory, session) = spillSession(4).await;
    let transactionId = beginTestTransaction(&session, "/spill-read").await;
    session
        .storeBody(
            &transactionId,
            MessageSide::Response,
            BodyWrite {
                bytes: b"persisted-spill".to_vec(),
                originalBytes: 15,
                contentType: "application/octet-stream".to_owned(),
                encoding: "identity".to_owned(),
            },
        )
        .await
        .expect("写入 spill 正文");
    assert_eq!(countBodyFiles(&temporaryDirectory), 1);
    assert_eq!(
        session
            .getBody(&transactionId, MessageSide::Response)
            .await
            .expect("读取 spill 正文")
            .bytes,
        b"persisted-spill"
    );
    session.clearSession().await.expect("清空录制会话");
    assert_eq!(countBodyFiles(&temporaryDirectory), 0);
    assert_eq!(
        session
            .snapshot()
            .await
            .expect("读取清空后快照")
            .transactionCount,
        0
    );
}

/// 验证限额缩小被拒绝，已结束事务的 spill 正文仍然完整可读。
#[tokio::test]
async fn limitReductionCannotEvictTerminalSpillBody() {
    let (temporaryDirectory, session) = spillSession(2).await;
    let firstId = beginTestTransaction(&session, "/first").await;
    storeTestSpill(&session, &firstId).await;
    session
        .commit(
            &firstId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: capture_core::currentTimeMilliseconds(),
                contentType: String::new(),
            },
        )
        .await
        .expect("完成首条事务");
    let secondId = beginTestTransaction(&session, "/second").await;
    let result = session
        .setLimits(RecordingLimits {
            maxTransactions: 1,
            maxBodyBytes: 4_096,
            maxTotalBodyBytes: 16_384,
        })
        .await;
    assert!(matches!(result, Err(CaptureError::InvalidLimits)));
    assert!(session.getTransaction(&firstId).await.is_ok());
    assert!(session.getTransaction(&secondId).await.is_ok());
    let snapshot = session.snapshot().await.expect("读取拒绝后的快照");
    assert_eq!(snapshot.transactionCount, 2);
    assert_eq!(snapshot.pendingCleanupCount, 0);
    assert_eq!(countBodyFiles(&temporaryDirectory), 1);
}

/// 验证限额修改失败后活动事务继续正常终结，不会在终态转换时延迟删除。
#[tokio::test]
async fn rejectedLimitChangeDoesNotDeleteOnTerminalTransition() {
    let (temporaryDirectory, session) = spillSession(2).await;
    let firstId = beginTestTransaction(&session, "/first").await;
    let secondId = beginTestTransaction(&session, "/second").await;
    for transactionId in [&firstId, &secondId] {
        storeTestSpill(&session, transactionId).await;
    }
    let result = session
        .setLimits(RecordingLimits {
            maxTransactions: 1,
            maxBodyBytes: 4_096,
            maxTotalBodyBytes: 16_384,
        })
        .await;
    assert!(matches!(result, Err(CaptureError::InvalidLimits)));
    let activeSnapshot = session.snapshot().await.expect("读取活动期快照");
    assert_eq!(activeSnapshot.transactionCount, 2);
    session
        .commit(
            &firstId,
            TransactionCompletion {
                statusCode: 204,
                endAtMilliseconds: capture_core::currentTimeMilliseconds(),
                contentType: String::new(),
            },
        )
        .await
        .expect("终态结束点收敛");
    assert!(session.getTransaction(&firstId).await.is_ok());
    let pendingCleanupSnapshot = session.snapshot().await.expect("读取终态快照");
    assert_eq!(pendingCleanupSnapshot.transactionCount, 2);
    assert_eq!(pendingCleanupSnapshot.totalBodyBytes, 20);
    assert_eq!(pendingCleanupSnapshot.pendingCleanupCount, 0);
    assert!(session.getTransaction(&secondId).await.is_ok());
    assert!(
        pendingCleanupSnapshot.totalMetadataBytes
            <= pendingCleanupSnapshot.metadataMemoryBudgetBytes
    );
    session
        .cleanupPendingBodies()
        .await
        .expect("回收终态 tombstone");
    assert_eq!(countBodyFiles(&temporaryDirectory), 2);
}

/// 验证 FIFO 淘汰会在新事务可见前完成旧 spill 的回收，调用方只会获得完整的新事务标识。
#[tokio::test]
async fn fifoCleanupCompletesBeforeNewTransaction() {
    let (temporaryDirectory, session) = spillSession(1).await;
    let firstId = beginTestTransaction(&session, "/first").await;
    storeTestSpill(&session, &firstId).await;
    session
        .commit(
            &firstId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: capture_core::currentTimeMilliseconds(),
                contentType: String::new(),
            },
        )
        .await
        .expect("完成首条事务");
    let secondId = session
        .beginTransaction(testTransaction("/second"))
        .await
        .expect("执行 FIFO 淘汰")
        .expect("新事务必须完整创建");
    assert!(matches!(
        session.getTransaction(&firstId).await,
        Err(CaptureError::TransactionNotFound)
    ));
    assert!(session.getTransaction(&secondId).await.is_ok());
    let snapshot = session.snapshot().await.expect("读取 FIFO 快照");
    assert_eq!(snapshot.transactionCount, 1);
    assert_eq!(snapshot.pendingCleanupCount, 0);
    assert_eq!(countBodyFiles(&temporaryDirectory), 0);
}

/// 验证清空会话时会同时删除多侧 spill 正文，并将公开集合和目录状态收敛为空。
#[tokio::test]
async fn clearSessionRemovesAllSpillBodies() {
    let (temporaryDirectory, session) = spillSession(4).await;
    let transactionId = beginTestTransaction(&session, "/clear").await;
    for side in [MessageSide::Request, MessageSide::Response] {
        session
            .storeBody(
                &transactionId,
                side,
                BodyWrite {
                    bytes: b"spill-body".to_vec(),
                    originalBytes: 10,
                    contentType: "application/octet-stream".to_owned(),
                    encoding: "identity".to_owned(),
                },
            )
            .await
            .expect("写入待清空正文");
    }
    assert_eq!(countBodyFiles(&temporaryDirectory), 2);
    session.clearSession().await.expect("清空会话正文");
    let snapshot = session.snapshot().await.expect("读取清空后快照");
    assert_eq!(snapshot.transactionCount, 0);
    assert_eq!(snapshot.pendingCleanupCount, 0);
    assert_eq!(snapshot.totalMetadataBytes, 0);
    assert_eq!(countBodyFiles(&temporaryDirectory), 0);
}

/// 验证活动事务不会被元数据压力淘汰，头按剩余预算裁剪且终态事务随后可被优先回收。
#[tokio::test]
async fn metadataBudgetTruncatesHeadersWithoutEvictingActiveTransactions() {
    let (_temporaryDirectory, session) = metadataBudgetSession(2, 256).await;
    let firstId = beginTestTransaction(&session, "/first-active").await;
    let secondId = beginTestTransaction(&session, "/second-active").await;
    let baselineMetadataBytes = session
        .snapshot()
        .await
        .expect("读取头写入前的元数据计数")
        .totalMetadataBytes;
    session
        .storeHeaders(
            &secondId,
            MessageSide::Request,
            vec![HeaderField {
                name: "x-large".to_owned(),
                value: "v".repeat(256 * 1024),
            }],
        )
        .await
        .expect("按预算保存头");
    let snapshot = session.snapshot().await.expect("读取元数据预算快照");
    assert!(snapshot.totalMetadataBytes <= snapshot.metadataMemoryBudgetBytes);
    assert!(session.getTransaction(&firstId).await.is_ok());
    let second = session
        .getTransactionDetail(&secondId)
        .await
        .expect("读取被裁剪头");
    assert!(second.transaction.flags.headersTruncated);
    assert!(!second.requestHeaders.is_empty());
    session
        .storeHeaders(&secondId, MessageSide::Request, Vec::new())
        .await
        .expect("替换为空头集合");
    assert_eq!(
        session
            .snapshot()
            .await
            .expect("读取头替换后的精确计数")
            .totalMetadataBytes,
        baselineMetadataBytes
    );

    assert!(
        session
            .beginTransaction(testTransaction("/skipped-active"))
            .await
            .expect("预算不足应跳过录制")
            .is_none()
    );
    assert!(session.getTransaction(&firstId).await.is_ok());
    session
        .commit(
            &firstId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: capture_core::currentTimeMilliseconds(),
                contentType: String::new(),
            },
        )
        .await
        .expect("完成最旧事务");
    let admittedId = session
        .beginTransaction(testTransaction("/admitted-after-terminal"))
        .await
        .expect("终态淘汰后接纳事务")
        .expect("新事务应被录制");
    assert!(matches!(
        session.getTransaction(&firstId).await,
        Err(CaptureError::TransactionNotFound)
    ));
    assert!(session.getTransaction(&secondId).await.is_ok());
    assert!(session.getTransaction(&admittedId).await.is_ok());
    session.clearSession().await.expect("清空元数据预算会话");
    assert_eq!(
        session
            .snapshot()
            .await
            .expect("读取清空后的元数据计数")
            .totalMetadataBytes,
        0
    );
}

/// 验证并发头写入在同一全局预算内线性化，所有活动事务仍可继续访问。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrentHeaderWritesKeepMetadataAccountingBounded() {
    let (_temporaryDirectory, session) = metadataBudgetSession(4, 1_024).await;
    let mut transactionIds = Vec::new();
    for index in 0..4 {
        transactionIds.push(beginTestTransaction(&session, &format!("/{index}")).await);
    }
    let writers = transactionIds
        .iter()
        .cloned()
        .map(|transactionId| {
            let writingSession = session.clone();
            tokio::spawn(async move {
                writingSession
                    .storeHeaders(
                        &transactionId,
                        MessageSide::Response,
                        vec![HeaderField {
                            name: "x-concurrent".to_owned(),
                            value: "v".repeat(128 * 1024),
                        }],
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer
            .await
            .expect("并发头任务不应 panic")
            .expect("并发头写入");
    }
    let snapshot = session.snapshot().await.expect("读取并发预算快照");
    assert!(snapshot.totalMetadataBytes <= snapshot.metadataMemoryBudgetBytes);
    let mut truncatedCount = 0;
    for transactionId in transactionIds {
        let transaction = session
            .getTransaction(&transactionId)
            .await
            .expect("活动事务不得被淘汰");
        truncatedCount += usize::from(transaction.flags.headersTruncated);
    }
    assert!(truncatedCount > 0, "受限测试预算必须命中至少一次头裁剪");
}

/// 验证默认完整录制策略在连续一万条事务后仍不淘汰任何记录。
#[tokio::test]
async fn defaultRecordingRetainsTransactionsBeyondLegacyLimit() {
    let temporaryDirectory = tempfile::tempdir().expect("创建默认预算临时目录");
    let session = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().to_path_buf(),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建默认预算会话");
    // 旧版在第 10001 条事务时会 FIFO 淘汰；回归必须跨过该边界才能证明录制集合完整。
    let verificationCount = 10_001;
    assert!(RecordingLimits::default().maxTransactions > verificationCount);
    for index in 0..verificationCount {
        let transactionId = beginTestTransaction(&session, &format!("/{index}")).await;
        session
            .commit(
                &transactionId,
                TransactionCompletion {
                    statusCode: 204,
                    endAtMilliseconds: capture_core::currentTimeMilliseconds(),
                    contentType: String::new(),
                },
            )
            .await
            .expect("提交默认预算事务");
    }
    let snapshot = session.snapshot().await.expect("读取默认预算快照");
    assert_eq!(snapshot.transactionCount, verificationCount);
    assert_eq!(snapshot.droppedCount, 0);
    assert!(snapshot.totalMetadataBytes <= snapshot.metadataMemoryBudgetBytes);
}

/// 验证 URI、客户端、用户标注和错误参数进入持久集合后保持完整内容。
#[tokio::test]
async fn freeTextFieldsStayWithinPerTransactionReservation() {
    let temporaryDirectory = tempfile::tempdir().expect("创建完整字段临时目录");
    let session = RecordingSession::new(RecordingConfiguration {
        spillDirectory: temporaryDirectory.path().to_path_buf(),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建完整字段会话");
    let largeText = "界".repeat(16 * 1024);
    let mut input = testTransaction("/bounded");
    input.method = largeText.clone();
    input.location.path = format!("/{largeText}");
    input.location.query = largeText.clone();
    input.location.display = largeText.clone();
    input.clientAddress = largeText.clone();
    input.clientProcessName = Some(largeText.clone());
    input.contentType = largeText.clone();
    let transactionId = session
        .beginTransaction(input)
        .await
        .expect("创建有界事务")
        .expect("事务应被录制");
    session
        .updateUserFields(
            &transactionId,
            TransactionUserUpdate {
                notes: Some(largeText.clone()),
                tags: Some(vec![largeText.clone(); 64]),
                appliedTools: Some(vec![largeText.clone(); 64]),
            },
        )
        .await
        .expect("写入有界用户字段");
    for side in [MessageSide::Request, MessageSide::Response] {
        session
            .storeBody(
                &transactionId,
                side,
                BodyWrite {
                    bytes: b"x".to_vec(),
                    originalBytes: 1,
                    contentType: largeText.clone(),
                    encoding: largeText.clone(),
                },
            )
            .await
            .expect("写入有界正文引用元数据");
    }
    session
        .fail(
            &transactionId,
            TransactionError {
                code: largeText.clone(),
                messageKey: largeText.clone(),
                params: (0..16)
                    .map(|index| (format!("{largeText}{index}"), largeText.clone()))
                    .collect(),
            },
            capture_core::currentTimeMilliseconds(),
        )
        .await
        .expect("写入有界错误");
    let transaction = session
        .getTransaction(&transactionId)
        .await
        .expect("读取有界摘要");
    assert_eq!(transaction.method, largeText);
    assert_eq!(transaction.path, format!("/{largeText}"));
    assert_eq!(transaction.query, largeText);
    assert_eq!(transaction.urlDisplay, largeText);
    assert_eq!(transaction.clientAddress, largeText);
    assert_eq!(
        transaction.clientProcessName.as_deref(),
        Some(largeText.as_str())
    );
    assert_eq!(transaction.contentType, largeText);
    assert_eq!(transaction.notes, largeText);
    assert_eq!(transaction.tags, vec![largeText.clone(); 64]);
    assert_eq!(transaction.appliedTools, vec![largeText.clone(); 64]);
    let error = transaction.error.as_ref().expect("终态错误");
    assert_eq!(error.code, largeText);
    assert_eq!(error.messageKey, largeText);
    assert_eq!(error.params.len(), 16);
    let snapshot = session.snapshot().await.expect("读取有界摘要快照");
    assert!(
        snapshot.totalMetadataBytes <= snapshot.metadataMemoryBudgetBytes,
        "完整摘要写入后记账不得超过 JavaScript 精确计数边界"
    );
}
