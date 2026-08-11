#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::time::Duration;

use tokio::time::timeout;

use socks5_core::model::TrafficDirection;
use socks5_core::registry::SessionRegistry;

const legacyCapturedStreamLimit: usize = 8 * 1024 * 1024;
const allocatedChunkBudget: usize = 64 * 1024;

/// 验证 SOCKS 写线正文保存修改后字节，同时单包索引保留 WPE 修改前后的精确差异。
#[test]
fn modifiedTrafficKeepsFinalBytesAndDifference() {
    let registry = SessionRegistry::new(8);
    let sessionId = registry.create("127.0.0.1:10000".to_owned());
    registry.addModifiedTraffic(
        &sessionId,
        TrafficDirection::Up,
        &[0x03, 0x06, 0x02, 0x01],
        &[0x03, 0x06, 0x02, 0x00],
    );

    let snapshot = registry.snapshots().pop().expect("应保留活动会话");
    assert_eq!(
        snapshot.capturedBytesUp.toVec(),
        vec![0x03, 0x06, 0x02, 0x00]
    );
    let packets = snapshot.capturedPackets.forDirection(TrafficDirection::Up);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].modifications.len(), 1);
    assert_eq!(packets[0].modifications[0].offsetBytes, 3);
    assert_eq!(packets[0].modifications[0].originalBytes, vec![0x01]);
    assert_eq!(packets[0].modifications[0].modifiedBytes, vec![0x00]);
}

/// 验证先订阅再读取基线时，落入快照窗口的创建事件仍保留在订阅队列中。
#[tokio::test]
async fn subscriptionBeforeSnapshotDoesNotLoseWindowEvent() {
    let registry = SessionRegistry::new(8);
    let mut events = registry.subscribe();
    let sessionId = registry.create("127.0.0.1:10000".to_owned());
    let snapshot = registry.snapshots();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].sessionId, sessionId);

    let event = timeout(Duration::from_millis(100), events.recv())
        .await
        .expect("快照窗口事件未进入订阅队列")
        .expect("会话事件通道意外关闭");
    assert_eq!(event.snapshot.sessionId, sessionId);
    assert_eq!(event.eventType, "sessionCreated");
}

/// 验证终态事件和权威历史都保留投影所需正文，确认接管后才释放注册表镜像。
#[tokio::test]
async fn terminalCaptureStaysReplayableUntilProjectionAcknowledges() {
    let registry = SessionRegistry::withCaptureBudget(8, 16);
    let mut events = registry.subscribe();
    let sessionId = registry.create("127.0.0.1:10000".to_owned());
    registry.addTraffic(&sessionId, TrafficDirection::Up, b"payload");
    registry.close(&sessionId, String::new());

    let terminalEvent = loop {
        let event = timeout(Duration::from_millis(100), events.recv())
            .await
            .expect("终态事件未进入订阅队列")
            .expect("会话事件通道意外关闭");
        if event.eventType == "sessionClosed" {
            break event;
        }
    };
    assert_eq!(terminalEvent.snapshot.capturedBytesUp.toVec(), b"payload");
    let capturedPackets = terminalEvent
        .snapshot
        .capturedPackets
        .forDirection(TrafficDirection::Up);
    assert_eq!(capturedPackets.len(), 1);
    assert_eq!(capturedPackets[0].storedOffsetBytes, 0);
    assert_eq!(capturedPackets[0].storedBytes, b"payload".len());
    assert_eq!(capturedPackets[0].originalBytes, b"payload".len() as u64);
    let historySnapshot = registry.snapshots().pop().expect("历史快照");
    assert_eq!(historySnapshot.capturedBytesUp.toVec(), b"payload");

    registry.releaseCapturedBytes(&sessionId);
    let releasedSnapshot = registry.snapshots().pop().expect("释放后的历史快照");
    assert!(releasedSnapshot.capturedBytesUp.is_empty());
    assert!(
        releasedSnapshot
            .capturedPackets
            .forDirection(TrafficDirection::Up)
            .is_empty()
    );
}

/// 验证投影确认后历史快照不再长期持有原始字节。
#[test]
fn closedHistoryReleasesCaptureAfterProjectionAcknowledges() {
    let registry = SessionRegistry::withCaptureBudget(8, 10);
    let sessionId = registry.create("127.0.0.1:10000".to_owned());
    registry.addTraffic(&sessionId, TrafficDirection::Up, b"12345678");
    registry.close(&sessionId, String::new());
    assert_eq!(
        registry
            .snapshots()
            .pop()
            .expect("确认前历史快照")
            .capturedBytesUp
            .len(),
        8
    );

    registry.releaseCapturedBytes(&sessionId);
    let historySnapshot = registry.snapshots().pop().expect("确认后历史快照");
    assert!(historySnapshot.capturedBytesUp.is_empty());
}

/// 验证生产注册表越过旧 8 MiB 前缀边界后仍保留完整正文，且原始字节不进入会话 JSON。
#[test]
fn trafficCaptureKeepsCompleteBodyBeyondLegacyLimit() {
    let registry = SessionRegistry::new(8);
    let sessionId = registry.create("127.0.0.1:10000".to_owned());
    let payload = vec![0x5a; legacyCapturedStreamLimit + 17];
    registry.addTraffic(&sessionId, TrafficDirection::Up, &payload);

    let snapshot = registry.snapshots().pop().expect("会话快照");
    assert_eq!(snapshot.bytesUp, payload.len() as u64);
    assert_eq!(snapshot.capturedBytesUp.len(), payload.len());
    assert!(
        snapshot
            .capturedBytesUp
            .toVec()
            .iter()
            .all(|byte| *byte == 0x5a)
    );
    let debugOutput = format!("{snapshot:?}");
    assert!(!debugOutput.contains("90, 90"));

    let serialized = serde_json::to_value(snapshot).expect("序列化公开会话快照");
    assert!(serialized.get("capturedBytesUp").is_none());
    assert!(serialized.get("capturedBytesDown").is_none());
}

/// 验证请求与响应拥有独立序号空间；交错转发时两侧首包都必须显示为各自的第 1 包。
#[test]
fn packetSequenceIncrementsIndependentlyPerDirection() {
    let registry = SessionRegistry::new(8);
    let sessionId = registry.create("127.0.0.1:10000".to_owned());
    registry.addTraffic(&sessionId, TrafficDirection::Up, b"request-1");
    registry.addTraffic(&sessionId, TrafficDirection::Down, b"response-1");
    registry.addTraffic(&sessionId, TrafficDirection::Up, b"request-2");
    registry.addTraffic(&sessionId, TrafficDirection::Down, b"response-2");

    let snapshot = registry.snapshots().pop().expect("交错流量快照");
    let requestSequences = snapshot
        .capturedPackets
        .forDirection(TrafficDirection::Up)
        .into_iter()
        .map(|packet| packet.sequence)
        .collect::<Vec<_>>();
    let responseSequences = snapshot
        .capturedPackets
        .forDirection(TrafficDirection::Down)
        .into_iter()
        .map(|packet| packet.sequence)
        .collect::<Vec<_>>();
    assert_eq!(requestSequences, [1, 2]);
    assert_eq!(responseSequences, [1, 2]);
}

/// 验证高频原始流越过旧 2048 分片边界后仍保留每个偏移和序号，避免正文完整但包视图丢失。
#[test]
fn packetIndexKeepsEveryFragmentBeyondLegacyLimit() {
    let registry = SessionRegistry::new(8);
    let sessionId = registry.create("127.0.0.1:10000".to_owned());
    for byte in 0..2_050_u16 {
        registry.addTraffic(&sessionId, TrafficDirection::Up, &[(byte % 256) as u8]);
    }

    let snapshot = registry.snapshots().pop().expect("高频会话快照");
    let packets = snapshot.capturedPackets.forDirection(TrafficDirection::Up);
    assert_eq!(snapshot.capturedBytesUp.len(), 2_050);
    assert_eq!(packets.len(), 2_050);
    assert_eq!(packets.first().expect("首分片").sequence, 1);
    assert_eq!(packets.last().expect("末分片").sequence, 2_050);
    assert_eq!(packets.last().expect("末分片").storedOffsetBytes, 2_049);
}

/// 验证所有会话和方向共享严格预算，投影确认释放后新会话可以复用全部额度。
#[test]
fn captureBudgetIsSharedAndProjectionReleaseReturnsBytes() {
    let registry = SessionRegistry::withCaptureBudget(8, 10);
    let firstSessionId = registry.create("127.0.0.1:10000".to_owned());
    registry.addTraffic(&firstSessionId, TrafficDirection::Up, b"12345678");
    registry.addTraffic(&firstSessionId, TrafficDirection::Down, b"abcdefgh");
    let activeSnapshot = registry.snapshots().pop().expect("活动快照");
    assert_eq!(activeSnapshot.capturedBytesUp.len(), 8);
    assert!(activeSnapshot.capturedBytesDown.is_empty());
    drop(activeSnapshot);

    registry.close(&firstSessionId, String::new());
    let closedSnapshot = registry.snapshots().pop().expect("终态快照");
    assert_eq!(closedSnapshot.capturedBytesUp.len(), 8);
    assert!(closedSnapshot.capturedBytesDown.is_empty());
    drop(closedSnapshot);

    let secondSessionId = registry.create("127.0.0.1:10001".to_owned());
    registry.addTraffic(&secondSessionId, TrafficDirection::Up, b"0123456789");
    let blockedSnapshot = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.sessionId == secondSessionId)
        .expect("预算占用期间的第二条活动快照");
    assert!(blockedSnapshot.capturedBytesUp.is_empty());
    drop(blockedSnapshot);

    registry.releaseCapturedBytes(&firstSessionId);
    registry.addTraffic(&secondSessionId, TrafficDirection::Up, b"0123456789");
    let secondSnapshot = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.sessionId == secondSessionId)
        .expect("第二条活动快照");
    assert_eq!(secondSnapshot.capturedBytesUp.len(), 10);

    registry.clearCapturedBytes();
    registry.addTraffic(
        &secondSessionId,
        TrafficDirection::Down,
        b"capture-must-stay-disabled",
    );
    let clearedSnapshot = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.sessionId == secondSessionId)
        .expect("清理后的活动快照");
    assert!(clearedSnapshot.capturedBytesUp.is_empty());
    assert!(clearedSnapshot.capturedBytesDown.is_empty());
}

/// 验证非整块首段不会触发 Vec 倍增；共享预算按已分配定长块容量严格封顶。
#[test]
fn captureBudgetTracksAllocatedChunkCapacity() {
    let registry = SessionRegistry::withCaptureBudget(8, allocatedChunkBudget);
    let firstSessionId = registry.create("127.0.0.1:10000".to_owned());
    registry.addTraffic(&firstSessionId, TrafficDirection::Up, &vec![0x31; 60_000]);
    registry.addTraffic(
        &firstSessionId,
        TrafficDirection::Up,
        &vec![0x32; allocatedChunkBudget - 60_000],
    );
    let firstSnapshot = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.sessionId == firstSessionId)
        .expect("首条容量边界快照");
    assert_eq!(firstSnapshot.capturedBytesUp.len(), allocatedChunkBudget);
    drop(firstSnapshot);

    let secondSessionId = registry.create("127.0.0.1:10001".to_owned());
    registry.addTraffic(&secondSessionId, TrafficDirection::Up, b"blocked");
    let blockedSnapshot = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.sessionId == secondSessionId)
        .expect("预算耗尽后的快照");
    assert!(blockedSnapshot.capturedBytesUp.is_empty());

    registry.releaseCapturedBytes(&firstSessionId);
    registry.addTraffic(&secondSessionId, TrafficDirection::Up, b"accepted");
    let reusedSnapshot = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.sessionId == secondSessionId)
        .expect("预算复用后的快照");
    assert_eq!(reusedSnapshot.capturedBytesUp.toVec(), b"accepted");
}
