//! 汇聚控制面的运行代际归档与事件投影任务。
//!
//! 这些函数只协调现有录制会话、SOCKS 会话快照和广播通道，不拥有网络套接字；拆分后主控制器只保留状态机与公开 API，
//! 避免单文件超过工程硬上限。投影异常按会话隔离，停止阶段则在固定期限后中止残留后台任务。

use super::*;

/// 将一个已结束数据面周期归档到控制层；每个 RunningServer 只能调用一次以避免指标重复累计。
pub(super) fn archiveRuntimeSnapshot(
    service: &mut ManagedService,
    mut snapshot: ServerSnapshot,
    historyLimit: usize,
) {
    let closedAtMilliseconds = currentTimeMilliseconds();
    let mut normalizedFailures = 0_u64;
    for session in &mut snapshot.sessions {
        if !matches!(session.state, SessionState::Closed | SessionState::Failed) {
            session.state = SessionState::Failed;
            session.updatedAtMilliseconds = closedAtMilliseconds;
            session.closedAtMilliseconds = closedAtMilliseconds;
            if session.errorMessage.is_empty() {
                session.errorMessage = "数据面结束时会话任务未发布最终状态".to_owned();
            }
            normalizedFailures += 1;
        }
        // stop 已先排空投影任务，正文由 capture-core 独立持有；归档只保留公开元数据，
        // 否则每次重启都会让旧实例预算连同原始字节跨代存活。
        session.capturedBytesUp = Default::default();
        session.capturedBytesDown = Default::default();
        session.capturedPackets = Default::default();
    }
    snapshot.metrics.activeConnections = 0;
    snapshot.metrics.failedConnections = snapshot
        .metrics
        .failedConnections
        .saturating_add(normalizedFailures);
    service.archivedMetrics = combineMetrics(&service.archivedMetrics, &snapshot.metrics);
    service.archivedSessions =
        combineSessions(&service.archivedSessions, &snapshot.sessions, historyLimit);
}

/// 把已终止 HTTP 服务周期的原子账本原子迁入历史；调用方必须先停止融合监听，确保计数不再变化。
///
/// 账本从当前槽位移除和写入历史必须在同一服务锁内完成，避免仍处于 50ms 合并窗口的指标任务
/// 观察到“当前已空、历史未写”的瞬态零值并以更高 revision 对外广播。
pub(super) fn archiveHttpRuntimeMetrics(service: &mut ManagedService) {
    let Some(metrics) = service.httpMetrics.take() else {
        return;
    };
    service.archivedMetrics = combineMetrics(&service.archivedMetrics, &metrics.snapshot());
}

/// 释放已经退出的数据面代际句柄；并发重启写入的新句柄具有不同 Arc 身份，必须原样保留。
pub(super) fn releaseExitedCaptureGeneration(
    service: &mut ManagedService,
    exitingCaptureGeneration: Option<&CaptureGeneration>,
) {
    if service
        .captureGeneration
        .as_ref()
        .zip(exitingCaptureGeneration)
        .is_some_and(|(current, exiting)| current.sameInstance(exiting))
    {
        service.captureGeneration = None;
    }
}

/// 等待会话事件投影任务排空；超过固定生命周期边界时中止任务，避免服务停止永久挂起。
pub(super) async fn drainRuntimeEventForwarder(eventForwarder: Option<JoinHandle<()>>) {
    let Some(mut eventForwarder) = eventForwarder else {
        return;
    };
    match timeout(runtimeEventDrainTimeout, &mut eventForwarder).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("SOCKS5 会话投影任务异常结束：{error}");
        }
        Err(_) => {
            eventForwarder.abort();
            let _ = eventForwarder.await;
            eprintln!("SOCKS5 会话投影任务排空超时，已中止残留任务");
        }
    }
}

/// 投影一组权威会话快照；单条 Capture 失败只隔离该会话，SOCKS5 数据转发保持独立。
async fn projectSocksSessions(
    state: &ControlState,
    projector: &mut SocksTransactionProjector,
    captureGeneration: &CaptureGeneration,
    sessions: &[SessionSnapshot],
) {
    let mut orderedSessions = sessions.iter().collect::<Vec<_>>();
    orderedSessions.sort_by_key(|session| session.createdAtMilliseconds);
    for session in orderedSessions {
        projectSocksSession(state, projector, captureGeneration, session).await;
    }
}

/// 投影单个会话增量；错误使用稳定机器码记录，不把目标地址或身份材料写入日志。
async fn projectSocksSession(
    state: &ControlState,
    projector: &mut SocksTransactionProjector,
    captureGeneration: &CaptureGeneration,
    session: &SessionSnapshot,
) {
    let projectionResult = {
        let _updateGuard = state.recordingUpdateLock.lock().await;
        projector.advanceCaptureGeneration(captureGeneration.current());
        projector.project(session).await
    };
    match projectionResult {
        Ok(()) => {
            if matches!(session.state, SessionState::Closed | SessionState::Failed) {
                state.releaseSocksCapturedBytes(&session.sessionId).await;
            }
        }
        Err(error) => {
            eprintln!(
                "SOCKS5 会话事务投影失败：sessionId={}，code={}",
                session.sessionId,
                error.code()
            );
        }
    }
}

/// 将高频会话事件写入事务录制器并合并公开快照；丢帧时从运行实例重放权威基线。
pub(super) async fn forwardRuntimeEvents(
    state: ControlState,
    mut sessionEvents: broadcast::Receiver<socks5_core::SessionEvent>,
    captureGeneration: CaptureGeneration,
    initialSessions: Vec<SessionSnapshot>,
    historyLimit: usize,
) {
    let mut projector = SocksTransactionProjector::new(state.recording.clone(), historyLimit);
    projectSocksSessions(&state, &mut projector, &captureGeneration, &initialSessions).await;
    loop {
        match sessionEvents.recv().await {
            Ok(event) => {
                projectSocksSession(&state, &mut projector, &captureGeneration, &event.snapshot)
                    .await
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let sessions = state.currentSocksSessions().await;
                projectSocksSessions(&state, &mut projector, &captureGeneration, &sessions).await;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
        let publishAt = Instant::now() + runtimeEventCoalescingInterval;
        loop {
            tokio::select! {
                _ = sleep_until(publishAt) => {
                    state.publishRuntimeViews().await;
                    break;
                }
                event = sessionEvents.recv() => {
                    match event {
                        Ok(event) => {
                            projectSocksSession(
                                &state,
                                &mut projector,
                                &captureGeneration,
                                &event.snapshot,
                            )
                            .await;
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            let sessions = state.currentSocksSessions().await;
                            projectSocksSessions(
                                &state,
                                &mut projector,
                                &captureGeneration,
                                &sessions,
                            )
                            .await;
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
        }
    }
}

/// 将 HTTP 套接字变化按固定短窗口合并为指标事件；`watch` 覆盖旧版本，流量突发不会堆积逐包消息。
pub(super) async fn forwardHttpMetricEvents(
    state: ControlState,
    mut changes: watch::Receiver<u64>,
) {
    while changes.changed().await.is_ok() {
        let publishAt = Instant::now() + runtimeEventCoalescingInterval;
        loop {
            tokio::select! {
                _ = sleep_until(publishAt) => {
                    state.publishMetricsView().await;
                    break;
                }
                changed = changes.changed() => {
                    if changed.is_err() {
                        // HTTP 指标发送端只会在监听生命周期结束后关闭；此时当前指标已从服务槽位取出、
                        // 但尚未归档。这里不得发布缺失本轮指标的过渡快照，最终权威值由归档完成后的
                        // `publishRuntimeViews` 统一发送。
                        return;
                    }
                }
            }
        }
    }
}

/// 将高级重复作业变化按短窗口合并为权威集合；高并发迭代不会把逐次进度放大为无界广播。
pub(super) async fn forwardAdvancedRepeatEvents(
    state: ControlState,
    mut changes: watch::Receiver<u64>,
) {
    let mut shutdownReceiver = state.shutdownSender.subscribe();
    loop {
        tokio::select! {
            _ = waitForControlShutdown(&mut shutdownReceiver) => return,
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
        let publishAt = Instant::now() + runtimeEventCoalescingInterval;
        loop {
            tokio::select! {
                _ = waitForControlShutdown(&mut shutdownReceiver) => return,
                _ = sleep_until(publishAt) => break,
                changed = changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
        let jobs = state.repeatRuntime.list().await;
        state.publishProjectionRevisioned(|serverInstanceId, revision| {
            EventMessage::AdvancedRepeats {
                serverInstanceId,
                revision,
                jobs,
            }
        });
    }
}

/// 将插件生命周期与活动连接计数按 50ms 窗口合并；事件只携带公开快照，不暴露插件配置或宿主内部状态。
pub(super) async fn forwardPluginEvents(state: ControlState, mut changes: watch::Receiver<u64>) {
    let mut shutdownReceiver = state.shutdownSender.subscribe();
    loop {
        tokio::select! {
            _ = waitForControlShutdown(&mut shutdownReceiver) => return,
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
        let publishAt = Instant::now() + runtimeEventCoalescingInterval;
        loop {
            tokio::select! {
                _ = waitForControlShutdown(&mut shutdownReceiver) => return,
                _ = sleep_until(publishAt) => break,
                changed = changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
        let plugins = state.pluginHost.snapshots();
        state.publishProjectionRevisioned(|serverInstanceId, revision| EventMessage::Plugins {
            serverInstanceId,
            revision,
            plugins,
        });
    }
}

/// 将高频 Capture 变化合并到 50ms 窗口，每次发送录制状态和权威全量事务摘要。
pub(super) async fn forwardRecordingEvents(state: ControlState, mut changes: watch::Receiver<u64>) {
    let mut shutdownReceiver = state.shutdownSender.subscribe();
    loop {
        tokio::select! {
            _ = waitForControlShutdown(&mut shutdownReceiver) => return,
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
        let publishAt = Instant::now() + runtimeEventCoalescingInterval;
        loop {
            tokio::select! {
                _ = waitForControlShutdown(&mut shutdownReceiver) => return,
                _ = sleep_until(publishAt) => {
                    if state.publishRecordingViews().await.is_err() {
                        return;
                    }
                    break;
                }
                changed = changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// 监听断点队列版本并推送完整队列快照；草稿只在队列变更时广播，避免每个事务事件重复复制正文。
pub(super) async fn forwardBreakpointEvents(
    state: ControlState,
    mut changes: watch::Receiver<u64>,
) {
    let mut shutdownReceiver = state.shutdownSender.subscribe();
    loop {
        tokio::select! {
            _ = waitForControlShutdown(&mut shutdownReceiver) => return,
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
        state.publishBreakpointQueue();
    }
}

/// 等待控制面关闭标记；先检查当前值，覆盖订阅发生在关闭通知之后的竞态。
pub(super) async fn waitForControlShutdown(shutdownReceiver: &mut watch::Receiver<bool>) {
    if *shutdownReceiver.borrow() {
        return;
    }
    while shutdownReceiver.changed().await.is_ok() {
        if *shutdownReceiver.borrow() {
            return;
        }
    }
}

/// 等待 SOCKS5 接受循环发布最终结果；发送端异常丢失时返回明确运行错误。
pub(super) async fn waitForServerExit(
    mut exitReceiver: watch::Receiver<Option<String>>,
) -> Option<String> {
    if let Some(errorMessage) = exitReceiver.borrow().clone() {
        return Some(errorMessage);
    }
    loop {
        if exitReceiver.changed().await.is_err() {
            return Some("SOCKS5 服务退出监控通道关闭".to_owned());
        }
        if let Some(errorMessage) = exitReceiver.borrow().clone() {
            return Some(errorMessage);
        }
    }
}
