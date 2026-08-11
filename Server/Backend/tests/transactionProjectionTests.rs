#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use capture_core::{
    TransactionFlags, TransactionProtocol, TransactionSizes, TransactionStatus, TransactionSummary,
    TransactionTimings,
};
use proxy_backend::transactionProjection::{TransactionPageSource, buildTransactionPage};

/// 构造自由文本均超过公开边界的事务摘要，用于验证投影而非录制层的响应限额。
fn oversizedSummary(sequence: u64) -> TransactionSummary {
    let text = "x".repeat(2_200);
    TransactionSummary {
        transactionId: format!("transaction-{sequence}"),
        recordingSessionId: "recording-session".to_owned(),
        sequence,
        protocol: TransactionProtocol::Http,
        method: text.clone(),
        host: text.clone(),
        port: 80,
        path: text.clone(),
        query: text.clone(),
        urlDisplay: text.clone(),
        status: TransactionStatus::Complete,
        statusCode: Some(200),
        clientAddress: text.clone(),
        clientProcessName: Some(text.clone()),
        clientProcessId: Some(1),
        contentType: text.clone(),
        timings: TransactionTimings::default(),
        sizes: TransactionSizes::default(),
        flags: TransactionFlags::default(),
        error: None,
        notes: text.clone(),
        tags: vec![text.clone(); 12],
        appliedTools: vec![text; 12],
    }
}

/// 验证列表、快照和事件共用的投影在极端元数据下仍保持完整 JSON 响应低于 4MiB。
#[test]
fn transactionCollectionStaysWithinWireBudget() {
    let transactions = (0..1_000).map(oversizedSummary).collect();
    let page = buildTransactionPage(TransactionPageSource {
        revision: 9,
        recordingSessionId: "recording-session".to_owned(),
        collectionToken: "recording-session:9".to_owned(),
        total: 1_000,
        transactions,
        offset: 0,
        limit: 1_000,
        preferLatest: false,
    });
    let encoded = serde_json::to_vec(&page).expect("序列化有界事务页");
    assert!(encoded.len() <= 4 * 1024 * 1024);
    assert!(page.items.len() < 1_000);
    assert!(page.itemsTruncated);
    assert!(page.hasMore);
    assert_eq!(page.nextOffset, Some(page.items.len()));
}

/// 验证预算缩短页面后按 nextOffset 继续读取，前后两页仍严格相邻且不会遗漏或重叠。
#[test]
fn nextOffsetUsesActualReturnedRange() {
    let firstPage = buildTransactionPage(TransactionPageSource {
        revision: 9,
        recordingSessionId: "recording-session".to_owned(),
        collectionToken: "recording-session:9".to_owned(),
        total: 1_000,
        transactions: (0..1_000).map(oversizedSummary).collect(),
        offset: 0,
        limit: 1_000,
        preferLatest: false,
    });
    let nextOffset = firstPage.nextOffset.expect("预算缩短后仍有下一页");
    let secondPage = buildTransactionPage(TransactionPageSource {
        revision: 9,
        recordingSessionId: "recording-session".to_owned(),
        collectionToken: "recording-session:9".to_owned(),
        total: 1_000,
        transactions: (nextOffset..1_000)
            .take(1_000)
            .map(|sequence| oversizedSummary(sequence as u64))
            .collect(),
        offset: nextOffset,
        limit: 1_000,
        preferLatest: false,
    });

    assert_eq!(
        firstPage.items.last().map(|item| item.sequence),
        Some(nextOffset as u64 - 1)
    );
    assert_eq!(
        secondPage.items.first().map(|item| item.sequence),
        Some(nextOffset as u64)
    );
}
