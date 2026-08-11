use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    config::{capturedStreamPrefixLimit, maximumTotalCapturedStreamBytes},
    model::{
        CaptureGeneration, CapturedBytes, CapturedBytesBudget, CapturedPacketList, ServiceMetrics,
        SessionApplicationProtocol, SessionEvent, SessionSnapshot, SessionState, TrafficDirection,
        currentTimeMilliseconds,
    },
};

/// 在网络任务之间共享会话快照并发布有界状态事件。
#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    sessions: RwLock<HashMap<String, SessionSnapshot>>,
    metrics: RwLock<ServiceMetrics>,
    historyLimit: usize,
    captureGeneration: CaptureGeneration,
    capturedBytesBudget: Arc<CapturedBytesBudget>,
    eventSender: broadcast::Sender<SessionEvent>,
}

/// 聚合一次会话元数据更新，避免调用方以多个可选参数制造字段顺序错误。
pub struct SessionUpdate {
    pub username: Option<String>,
    pub command: Option<String>,
    pub targetAddress: Option<String>,
    pub applicationProtocol: Option<SessionApplicationProtocol>,
    pub state: SessionState,
}

impl SessionRegistry {
    /// 创建指定关闭历史上限的注册表；事件慢订阅者由 broadcast 明确报告丢帧。
    pub fn new(historyLimit: usize) -> Self {
        Self::withCaptureBudget(historyLimit, maximumTotalCapturedStreamBytes)
    }

    /// 创建具有可注入镜像预算的注册表；生产路径不设上限，显式小值只供边界测试。
    pub fn withCaptureBudget(historyLimit: usize, maximumCapturedBytes: usize) -> Self {
        let (eventSender, _) = broadcast::channel(1_024);
        Self {
            inner: Arc::new(RegistryInner {
                sessions: RwLock::new(HashMap::new()),
                metrics: RwLock::new(ServiceMetrics::default()),
                historyLimit,
                captureGeneration: CaptureGeneration::new(),
                capturedBytesBudget: Arc::new(CapturedBytesBudget::new(maximumCapturedBytes)),
                eventSender,
            }),
        }
    }

    /// 订阅后续会话事件；调用方必须先订阅再取 snapshot，使快照读取窗口内的事件进入队列而不会丢失。
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.inner.eventSender.subscribe()
    }

    /// 返回与当前注册表共享的录制代际句柄；投影任务只读取，不直接推进。
    pub fn captureGeneration(&self) -> CaptureGeneration {
        self.inner.captureGeneration.clone()
    }

    /// 创建协商中会话并返回全生命周期唯一 ID。
    pub fn create(&self, clientAddress: String) -> String {
        let now = currentTimeMilliseconds();
        let mut sessions = self.inner.sessions.write();
        let snapshot = SessionSnapshot {
            sessionId: Uuid::new_v4().simple().to_string(),
            clientAddress,
            username: String::new(),
            command: String::new(),
            targetAddress: String::new(),
            state: SessionState::Negotiating,
            bytesUp: 0,
            bytesDown: 0,
            createdAtMilliseconds: now,
            updatedAtMilliseconds: now,
            closedAtMilliseconds: 0,
            errorMessage: String::new(),
            applicationProtocol: SessionApplicationProtocol::Undetermined,
            // 与 sessions 写锁共同确定代际，避免 create 和 clear 跨越边界。
            captureGeneration: self.inner.captureGeneration.current(),
            capturedBytesUp: CapturedBytes::withBudget(self.inner.capturedBytesBudget.clone()),
            capturedBytesDown: CapturedBytes::withBudget(self.inner.capturedBytesBudget.clone()),
            capturedPackets: CapturedPacketList::new(),
        };
        let sessionId = snapshot.sessionId.clone();
        sessions.insert(sessionId.clone(), snapshot.clone());
        drop(sessions);
        {
            let mut metrics = self.inner.metrics.write();
            metrics.acceptedConnections += 1;
            metrics.activeConnections += 1;
        }
        self.publish("sessionCreated", snapshot);
        sessionId
    }

    /// 更新身份、命令、目标和状态；不存在的会话返回 false。
    pub fn update(&self, sessionId: &str, update: SessionUpdate) -> bool {
        let snapshot = {
            let mut sessions = self.inner.sessions.write();
            let Some(snapshot) = sessions.get_mut(sessionId) else {
                return false;
            };
            if let Some(username) = update.username {
                snapshot.username = username;
            }
            if let Some(command) = update.command {
                snapshot.command = command;
            }
            if let Some(targetAddress) = update.targetAddress {
                snapshot.targetAddress = targetAddress;
            }
            if let Some(applicationProtocol) = update.applicationProtocol {
                snapshot.applicationProtocol = applicationProtocol;
            }
            snapshot.state = update.state;
            snapshot.updatedAtMilliseconds = currentTimeMilliseconds();
            snapshot.clone()
        };
        self.publish("sessionUpdated", snapshot);
        true
    }

    /// 累计已成功转发的载荷并保存完整正文与分片索引；空载荷不发布无意义事件。
    pub fn addTraffic(&self, sessionId: &str, direction: TrafficDirection, payload: &[u8]) {
        self.addModifiedTraffic(sessionId, direction, payload, payload);
    }

    /// 记录插件处理前后的真实字节并提取差异；正文始终保存最终写线值，差异只作为包级可视化元数据。
    ///
    /// 运行上下文：SOCKS TCP/UDP 写入对端成功后调用。`originalPayload` 是读取原文，`payload` 是最终写线值。
    /// 失败语义：会话已结束或最终正文为空时忽略；该方法不影响已经完成的网络写入。
    pub fn addModifiedTraffic(
        &self,
        sessionId: &str,
        direction: TrafficDirection,
        originalPayload: &[u8],
        payload: &[u8],
    ) {
        if payload.is_empty() {
            return;
        }
        let modifications = plugin_host::deriveWireByteModifications(originalPayload, payload);
        let byteCount = payload.len() as u64;
        let capturedAtMilliseconds = currentTimeMilliseconds();
        let snapshot = {
            let mut sessions = self.inner.sessions.write();
            let Some(snapshot) = sessions.get_mut(sessionId) else {
                return;
            };
            match direction {
                TrafficDirection::Up => {
                    snapshot.bytesUp = snapshot.bytesUp.saturating_add(byteCount);
                    let storedOffsetBytes = snapshot.capturedBytesUp.len();
                    let storedBytes = snapshot
                        .capturedBytesUp
                        .append(payload, capturedStreamPrefixLimit);
                    snapshot.capturedPackets.append(
                        direction,
                        capturedAtMilliseconds,
                        storedOffsetBytes,
                        storedBytes,
                        byteCount,
                        modifications.clone(),
                    );
                }
                TrafficDirection::Down => {
                    snapshot.bytesDown = snapshot.bytesDown.saturating_add(byteCount);
                    let storedOffsetBytes = snapshot.capturedBytesDown.len();
                    let storedBytes = snapshot
                        .capturedBytesDown
                        .append(payload, capturedStreamPrefixLimit);
                    snapshot.capturedPackets.append(
                        direction,
                        capturedAtMilliseconds,
                        storedOffsetBytes,
                        storedBytes,
                        byteCount,
                        modifications,
                    );
                }
            }
            snapshot.updatedAtMilliseconds = capturedAtMilliseconds;
            snapshot.clone()
        };
        let mut metrics = self.inner.metrics.write();
        match direction {
            TrafficDirection::Up => metrics.bytesUp += byteCount,
            TrafficDirection::Down => metrics.bytesDown += byteCount,
        }
        drop(metrics);
        self.publish("sessionTraffic", snapshot);
    }

    /// 结束会话并保存诊断；空错误表示正常关闭，非空错误表示失败。
    pub fn close(&self, sessionId: &str, errorMessage: String) {
        let snapshot = {
            let mut sessions = self.inner.sessions.write();
            let Some(snapshot) = sessions.get_mut(sessionId) else {
                return;
            };
            let now = currentTimeMilliseconds();
            snapshot.state = if errorMessage.is_empty() {
                SessionState::Closed
            } else {
                SessionState::Failed
            };
            snapshot.errorMessage = errorMessage;
            snapshot.closedAtMilliseconds = now;
            snapshot.updatedAtMilliseconds = now;
            snapshot.clone()
        };
        {
            let mut metrics = self.inner.metrics.write();
            metrics.activeConnections = metrics.activeConnections.saturating_sub(1);
            if !snapshot.errorMessage.is_empty() {
                metrics.failedConnections += 1;
            }
        }
        self.publish("sessionClosed", snapshot);
        self.pruneClosed();
    }

    /// 以同一诊断关闭所有尚未结束的会话；仅用于任务被强制中止后修复最终状态与活动计数。
    pub fn closeActive(&self, errorMessage: &str) {
        let activeSessionIds: Vec<String> = self
            .inner
            .sessions
            .read()
            .iter()
            .filter(|(_, snapshot)| {
                !matches!(snapshot.state, SessionState::Closed | SessionState::Failed)
            })
            .map(|(sessionId, _)| sessionId.clone())
            .collect();
        for sessionId in activeSessionIds {
            self.close(&sessionId, errorMessage.to_owned());
        }
    }

    /// 返回创建时间倒序的不可变会话集合。
    pub fn snapshots(&self) -> Vec<SessionSnapshot> {
        let mut snapshots: Vec<_> = self.inner.sessions.read().values().cloned().collect();
        snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.createdAtMilliseconds));
        snapshots
    }

    /// 返回服务累计指标副本，控制面读取不会持有注册表内部锁。
    pub fn metrics(&self) -> ServiceMetrics {
        self.inner.metrics.read().clone()
    }

    /// 累计成功转发的 UDP 包；字节数仍由 addTraffic 单独记录。
    pub fn recordUdpPacket(&self, direction: TrafficDirection) {
        let mut metrics = self.inner.metrics.write();
        match direction {
            TrafficDirection::Up => metrics.udpPacketsUp += 1,
            TrafficDirection::Down => metrics.udpPacketsDown += 1,
        }
    }

    /// 累计被协议、来源或资源边界拒绝的 UDP 数据报。
    pub fn recordDroppedUdpPacket(&self) {
        self.inner.metrics.write().droppedUdpPackets += 1;
    }

    /// 删除所有已结束记录并返回删除 ID；活动会话继续承担流量记账。
    pub fn clearClosed(&self) -> Vec<String> {
        let mut sessions = self.inner.sessions.write();
        let removedIds: Vec<String> = sessions
            .iter()
            .filter(|(_, snapshot)| {
                matches!(snapshot.state, SessionState::Closed | SessionState::Failed)
            })
            .map(|(sessionId, _)| sessionId.clone())
            .collect();
        for sessionId in &removedIds {
            sessions.remove(sessionId);
        }
        removedIds
    }

    /// 释放全部活动与历史会话的原始流镜像；清空录制后既有会话继续转发但不再追写已删除事务。
    pub fn clearCapturedBytes(&self) {
        let mut sessions = self.inner.sessions.write();
        self.inner.captureGeneration.advance();
        for snapshot in sessions.values_mut() {
            snapshot.capturedBytesUp = CapturedBytes::default();
            snapshot.capturedBytesDown = CapturedBytes::default();
            snapshot.capturedPackets = CapturedPacketList::default();
        }
    }

    /// 从注册表历史中释放已完成投影的原始流；调用前必须确认录制器已经接管正文，
    /// 否则广播丢帧后的权威快照将失去恢复正文所需的字节。
    pub fn releaseCapturedBytes(&self, sessionId: &str) {
        let mut sessions = self.inner.sessions.write();
        let Some(snapshot) = sessions.get_mut(sessionId) else {
            return;
        };
        snapshot.capturedBytesUp = CapturedBytes::default();
        snapshot.capturedBytesDown = CapturedBytes::default();
        snapshot.capturedPackets = CapturedPacketList::default();
    }

    /// 按关闭时间删除最旧记录，活动会话永不参与上限裁剪。
    fn pruneClosed(&self) {
        let mut sessions = self.inner.sessions.write();
        let mut closed: Vec<(String, u64)> = sessions
            .iter()
            .filter(|(_, snapshot)| {
                matches!(snapshot.state, SessionState::Closed | SessionState::Failed)
            })
            .map(|(sessionId, snapshot)| (sessionId.clone(), snapshot.closedAtMilliseconds))
            .collect();
        if closed.len() <= self.inner.historyLimit {
            return;
        }
        closed.sort_by_key(|(_, closedAt)| *closedAt);
        let removeCount = closed.len() - self.inner.historyLimit;
        for (sessionId, _) in closed.into_iter().take(removeCount) {
            sessions.remove(&sessionId);
        }
    }

    /// 发布会话事件；没有订阅者不是错误，网络数据面不得依赖界面在线。
    fn publish(&self, eventType: &str, snapshot: SessionSnapshot) {
        let _ = self.inner.eventSender.send(SessionEvent {
            eventType: eventType.to_owned(),
            snapshot,
        });
    }
}
