//! 构建控制面权威快照，并按修订号发布配置、运行指标、录制和进程捕获事件。
//!
//! 该模块只读取共享状态并投影事件，不拥有监听器或账号服务生命周期，避免快照读取改变运行状态。

use super::*;

impl ControlState {
    /// 订阅控制面广播事件；接收方落后时应通过 snapshotEvent 重建完整视图。
    pub fn subscribeEvents(&self) -> broadcast::Receiver<EventMessage> {
        self.eventSender.subscribe()
    }

    /// 订阅进程关闭状态；新订阅方可立即读取最近一次关闭标记。
    pub fn subscribeShutdown(&self) -> watch::Receiver<bool> {
        self.shutdownSender.subscribe()
    }

    /// 发布会改变完整快照结构的控制事件；投影代际与事件修订在同一临界区推进。
    pub(super) fn publishProjectionRevisioned<F>(&self, eventFactory: F)
    where
        F: FnOnce(String, u64) -> EventMessage,
    {
        let _publishGuard = self.eventPublishLock.lock();
        self.projectionGeneration.fetch_add(1, Ordering::Release);
        self.publishEventLocked(eventFactory);
    }

    /// 在调用方持有事件发布锁时分配修订号并广播；仅用于把同一投影批次原子接到事件序列末尾。
    fn publishEventLocked<F>(&self, eventFactory: F)
    where
        F: FnOnce(String, u64) -> EventMessage,
    {
        let revision = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self
            .eventSender
            .send(eventFactory(self.serverInstanceId.to_string(), revision));
    }

    /// 返回当前修订号，不为只读 GET 人为制造状态变更。
    pub(super) fn currentRevision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// 复制当前 SOCKS5 会话基线；仅供广播丢帧后的投影重放，不包含跨服务周期归档历史。
    pub(super) async fn currentSocksSessions(&self) -> Vec<SessionSnapshot> {
        let service = self.service.lock().await;
        service
            .runningServer
            .as_ref()
            .map(|server| server.snapshot().sessions)
            .unwrap_or_default()
    }

    /// 在事务录制器确认接管终态正文后释放数据面镜像；服务周期已经结束时由实例析构统一回收。
    pub(super) async fn releaseSocksCapturedBytes(&self, sessionId: &str) {
        let service = self.service.lock().await;
        if let Some(server) = service.runningServer.as_ref() {
            server.releaseCapturedBytes(sessionId);
        }
    }

    /// 返回控制面快照；低频投影代际保证配置与生命周期一致，高频指标修订不会触发重试饥饿。
    ///
    /// 运行上下文：各异步数据源在短临界区复制，读取期间不持同步事件锁。
    /// 失败语义：低频配置或生命周期并发提交时重新读取；遥测持续刷新不影响本调用完成。
    pub async fn snapshot(&self) -> ControlSnapshot {
        self.snapshotWithProjectionGeneration().await.0
    }

    /// 构建快照并返回与载荷匹配的低频投影代际；运行事件据此拒绝跨配置代际的旧载荷。
    async fn snapshotWithProjectionGeneration(&self) -> (ControlSnapshot, u64) {
        let mut transactionReceiver = self.configurationTransactionSender.subscribe();
        loop {
            while *transactionReceiver.borrow_and_update() {
                if transactionReceiver.changed().await.is_err() {
                    break;
                }
            }
            let projectionGeneration = self.projectionGeneration.load(Ordering::Acquire);
            let revision = self.currentRevision();
            // 先在 service -> configuration 固定锁序下复制纯视图，再释放生命周期锁执行 Capture await，
            // 避免慢磁盘元数据操作阻塞 start/stop/configuration。
            let (
                serviceState,
                configuration,
                httpConfiguration,
                processCaptureConfiguration,
                listeners,
                archivedSessions,
                archivedMetrics,
                currentSessions,
                currentMetrics,
            ) = {
                let service = self.service.lock().await;
                let configuration = self.configuration.read().await.clone();
                let httpConfiguration = self.httpConfiguration.read().await.clone();
                let processCaptureConfiguration =
                    self.processCaptureConfiguration.read().await.clone();
                let (currentSessions, socksMetrics) =
                    if let Some(server) = service.runningServer.as_ref() {
                        let snapshot = server.snapshot();
                        (snapshot.sessions, snapshot.metrics)
                    } else {
                        (Vec::new(), ServiceMetrics::default())
                    };
                let httpMetrics = service
                    .httpMetrics
                    .as_ref()
                    .map(HttpRuntimeMetrics::snapshot)
                    .unwrap_or_default();
                let currentMetrics = combineConcurrentMetrics(&socksMetrics, &httpMetrics);
                (
                    service.state,
                    configuration,
                    httpConfiguration.clone(),
                    processCaptureConfiguration,
                    listenerSnapshots(&service, &httpConfiguration),
                    service.archivedSessions.clone(),
                    service.archivedMetrics.clone(),
                    currentSessions,
                    currentMetrics,
                )
            };
            let sessions = combineSessions(
                &archivedSessions,
                &currentSessions,
                configuration.sessionHistoryLimit,
            );
            let metrics = combineMetrics(&archivedMetrics, &currentMetrics);
            let RecordingPageView {
                recording,
                collectionToken,
                total,
                offset: transactionOffset,
                transactions: allTransactions,
            } = self
                .recording
                .pageView(None, snapshotTransactionLimit, None)
                .await
                .expect("控制面持有的 RecordingSession 在进程退出前不得关闭");
            let advancedRepeats = self.repeatRuntime.list().await;
            let plugins = self.pluginHost.snapshots();
            let mcp = self.mcp.publicState().await;
            let multiAccountConfiguration = self.multiAccountConfiguration.read().await.clone();
            let multiAccount = self
                .accountService
                .publicState(&multiAccountConfiguration)
                .await;
            let transactions = buildTransactionPage(TransactionPageSource {
                revision,
                recordingSessionId: recording.recordingSessionId.clone(),
                collectionToken,
                total,
                transactions: allTransactions,
                offset: transactionOffset,
                limit: snapshotTransactionLimit,
                preferLatest: true,
            });
            let snapshot = ControlSnapshot {
                serverInstanceId: self.serverInstanceId.to_string(),
                revision,
                serviceState,
                metrics,
                sessions,
                configuration: PublicConfiguration::fromInternal(
                    ConfigurationProjectionSource {
                        socks5: &configuration,
                        http: &httpConfiguration,
                        processCapture: &processCaptureConfiguration,
                        startServiceOnLaunch: self.startServiceOnLaunch.load(Ordering::Acquire),
                    },
                    multiAccount,
                ),
                processCapture: self.processCapture.snapshot(),
                listeners,
                ssl: self.ssl.publicState(),
                recording,
                tools: self.tools.publicState(),
                transactions,
                advancedRepeats,
                plugins,
                mcp,
            };
            // 快照读取包含多个异步数据源，不能在期间持有同步发布锁。完成后以版本校验确认读取窗口内
            // 没有控制事件插入；若版本推进则重新投影，避免旧 revision 携带新配置或新运行状态。
            let publishGuard = self.eventPublishLock.lock();
            if projectionRevisionIsStable(
                projectionGeneration,
                self.projectionGeneration.load(Ordering::Acquire),
            ) && !*transactionReceiver.borrow()
            {
                drop(publishGuard);
                return (snapshot, projectionGeneration);
            }
            drop(publishGuard);
            tokio::task::yield_now().await;
        }
    }

    /// 构造自校验完整事件；事件顶层与嵌套快照必须携带同一后台实例标识。
    pub async fn snapshotEvent(&self) -> EventMessage {
        let snapshot = self.snapshot().await;
        EventMessage::Snapshot {
            serverInstanceId: snapshot.serverInstanceId.clone(),
            snapshot: Box::new(snapshot),
        }
    }

    /// 发布单个录制代际的快照和权威全量事务摘要；变化通知保留后续代际，单轮发布不会追逐写入而饥饿。
    ///
    /// 失败语义：录制页读取失败返回结构化错误；成功只确认当前代际已发布，后续变化由下一次通知继续投影。
    pub(super) async fn publishRecordingViews(&self) -> Result<(), ApiError> {
        let _publishGuard = self.capturePublishLock.lock().await;
        let captureRevision = self.recording.currentChangeRevision();
        if captureRevision <= self.publishedCaptureRevision.load(Ordering::Acquire) {
            return Ok(());
        }
        let RecordingPageView {
            recording,
            collectionToken,
            total,
            offset,
            transactions: allTransactions,
        } = self
            .recording
            .pageView(None, snapshotTransactionLimit, None)
            .await
            .map_err(mapCaptureOperationError)?;
        let recordingSessionId = recording.recordingSessionId.clone();
        let publishGuard = self.eventPublishLock.lock();
        self.publishEventLocked(|serverInstanceId, revision| EventMessage::Recording {
            serverInstanceId,
            revision,
            recording,
        });
        self.publishEventLocked(|serverInstanceId, revision| EventMessage::Transactions {
            serverInstanceId,
            revision,
            transactions: buildTransactionPage(TransactionPageSource {
                revision,
                recordingSessionId,
                collectionToken,
                total,
                transactions: allTransactions,
                offset,
                limit: snapshotTransactionLimit,
                preferLatest: true,
            }),
        });
        drop(publishGuard);
        self.publishedCaptureRevision
            .store(captureRevision, Ordering::Release);
        Ok(())
    }

    /// 从控制层权威历史与当前运行实例发布 sessions、metrics 两种严格增量消息。
    pub(super) async fn publishRuntimeViews(&self) {
        loop {
            let (snapshot, expectedProjectionGeneration) =
                self.snapshotWithProjectionGeneration().await;
            let publishGuard = self.eventPublishLock.lock();
            if !projectionRevisionIsStable(
                expectedProjectionGeneration,
                self.projectionGeneration.load(Ordering::Acquire),
            ) {
                drop(publishGuard);
                tokio::task::yield_now().await;
                continue;
            }
            self.publishEventLocked(|serverInstanceId, revision| EventMessage::Sessions {
                serverInstanceId,
                revision,
                sessions: snapshot.sessions,
            });
            self.publishEventLocked(|serverInstanceId, revision| EventMessage::Metrics {
                serverInstanceId,
                revision,
                metrics: snapshot.metrics,
            });
            self.publishEventLocked(|serverInstanceId, revision| EventMessage::ProcessCapture {
                serverInstanceId,
                revision,
                processCapture: snapshot.processCapture,
            });
            drop(publishGuard);
            return;
        }
    }

    /// 发布 SOCKS 与 HTTP 合并后的真实服务指标；高频 HTTP I/O 不读取事务页或进程捕获快照。
    pub(super) async fn publishMetricsView(&self) {
        let metrics = {
            let service = self.service.lock().await;
            let socksMetrics = service
                .runningServer
                .as_ref()
                .map(|server| server.snapshot().metrics)
                .unwrap_or_default();
            let httpMetrics = service
                .httpMetrics
                .as_ref()
                .map(HttpRuntimeMetrics::snapshot)
                .unwrap_or_default();
            combineMetrics(
                &service.archivedMetrics,
                &combineConcurrentMetrics(&socksMetrics, &httpMetrics),
            )
        };
        let publishGuard = self.eventPublishLock.lock();
        self.publishEventLocked(|serverInstanceId, revision| EventMessage::Metrics {
            serverInstanceId,
            revision,
            metrics,
        });
        drop(publishGuard);
    }

    /// 发布 WinDivert 运行快照；高频数据包计数与普通代理会话事件相互独立，避免无代理会话时工作台停止刷新。
    ///
    /// 运行上下文：进程路径同步任务每秒调用一次，服务启停则由 `publishRuntimeViews` 发布最终状态。
    /// 失败语义：快照只复制原子计数和有界流表长度，不执行驱动 IO；事件慢订阅者沿用广播通道的丢帧重连语义。
    pub(super) async fn publishProcessCaptureView(&self) {
        let processCapture = self.processCapture.snapshot();
        let publishGuard = self.eventPublishLock.lock();
        self.publishEventLocked(|serverInstanceId, revision| EventMessage::ProcessCapture {
            serverInstanceId,
            revision,
            processCapture,
        });
        drop(publishGuard);
    }
}

/// 判断一次无锁异步投影期间修订是否稳定；不稳定载荷必须丢弃并重新读取。
fn projectionRevisionIsStable(expectedRevision: u64, currentRevision: u64) -> bool {
    expectedRevision == currentRevision
}
#[cfg(test)]
#[path = "../../tests/unit/controlApi/stateProjectionTests.rs"]
mod tests;
