//! 管理代理数据面启停、异常退出与配置切换事务。
//!
//! 所有入口依赖 `serviceOperationLock` 串行化，配置切换必须把持久化配置与监听器视为同一事务。

use super::*;
use std::net::{Ipv4Addr, Ipv6Addr};

/// 把账号服务监听地址投影为服务端本机可达地址；未指定地址必须转换为同地址族回环，禁止再次访问公网入口。
fn localRuleServiceIp(configuration: &MultiAccountConfiguration) -> Option<IpAddr> {
    if !configuration.enabled {
        return None;
    }
    let address = configuration.remoteHost.parse::<IpAddr>().ok()?;
    Some(match address {
        IpAddr::V4(address) if address.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(address) if address.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        address => address,
    })
}

/// 收拢一次融合数据面启动所需的私有候选与事件发布边界。
///
/// 候选配置只在事务验收期间可见；启动失败不会把它写入共享配置，生命周期事件也由调用方显式决定。
struct ServiceStartRequest<'a> {
    candidate: Option<&'a ConfigurationTransactionSnapshot>,
    publishLifecycleEvents: bool,
    startingHttpConfiguration: ManagedHttpProxyConfiguration,
}

/// 保存已经过账号服务准备的完整启动配置，后续阶段只消费该快照，避免跨 `await` 重读到不同代次。
struct ServiceStartPlan {
    socks5: Socks5Config,
    multiAccount: MultiAccountConfiguration,
    accountService: Option<AccountServiceClientConfig>,
    http: ManagedHttpProxyConfiguration,
    processCapture: ProcessCaptureConfiguration,
    auxiliary: AuxiliaryListenerConfiguration,
}

/// 聚合已绑定主服务与预订阅 HTTP 指标；激活阶段必须整体接管或整体回滚这些资源。
struct PrimaryActivation {
    runningServer: RunningServer,
    httpMetrics: HttpRuntimeMetrics,
    httpMetricChanges: watch::Receiver<u64>,
}

/// 记录主监听提交后的最终状态，锁外只据此发布运行视图或返回稳定错误。
struct ServiceStartOutcome {
    running: bool,
    errorMessage: Option<String>,
}

impl ControlState {
    /// 监督账号服务健康与代理恢复；pending 代际只对应一次有效配置，健康后代理失败按一至三十秒退避重试。
    ///
    /// 运行上下文：后台任务持续执行，所有恢复都在生命周期锁内复核配置和用户运行意图。
    /// 失败语义：子进程或代理恢复错误写入中文日志并继续有界重试；显式停止和配置换代立即取消 pending。
    pub(super) async fn monitorAccountService(&self) {
        let mut shutdownReceiver = self.shutdownSender.subscribe();
        let mut retryDelay = Duration::from_secs(1);
        let mut pendingProxyRecoveryGeneration = None;
        loop {
            tokio::select! {
                _ = waitForControlShutdown(&mut shutdownReceiver) => return,
                _ = tokio::time::sleep(retryDelay) => {}
            }
            let observedGeneration = self.multiAccountGeneration.load(Ordering::Acquire);
            let observedConfiguration = self.multiAccountConfiguration.read().await.clone();
            let accountServiceHealthy = self.accountService.health().await.is_ok();
            if accountServiceHealthy && pendingProxyRecoveryGeneration.is_none() {
                retryDelay = Duration::from_secs(1);
                continue;
            }
            let _operationGuard = self.serviceOperationLock.lock().await;
            let currentGeneration = self.multiAccountGeneration.load(Ordering::Acquire);
            let currentConfiguration = self.multiAccountConfiguration.read().await.clone();
            if !accountRecoveryMatchesCurrentConfiguration(
                observedGeneration,
                currentGeneration,
                &observedConfiguration,
                &currentConfiguration,
            ) {
                retryDelay = Duration::from_secs(1);
                pendingProxyRecoveryGeneration = None;
                continue;
            }
            if accountServiceHealthy {
                if pendingProxyRecoveryGeneration != Some(currentGeneration)
                    || !accountRecoveryMayRestartProxy(
                        self.serviceRunIntent.load(Ordering::Acquire),
                        currentGeneration,
                        self.multiAccountGeneration.load(Ordering::Acquire),
                    )
                {
                    pendingProxyRecoveryGeneration = None;
                    retryDelay = Duration::from_secs(1);
                    continue;
                }
                match self.startServiceExclusive().await {
                    Ok(_) => {
                        pendingProxyRecoveryGeneration = None;
                        retryDelay = Duration::from_secs(1);
                    }
                    Err(error) => {
                        eprintln!("账号服务健康后重试代理数据面失败：{}", error.message());
                        retryDelay = nextAccountRecoveryDelay(retryDelay);
                    }
                }
                continue;
            }
            if currentConfiguration.enabled
                && self.service.lock().await.state == ServiceState::Running
                && let Err(error) = self.stopServiceExclusive().await
            {
                eprintln!("账号服务失联后停止代理数据面失败：{}", error.message());
                retryDelay = nextAccountRecoveryDelay(retryDelay);
                continue;
            }
            pendingProxyRecoveryGeneration = (currentConfiguration.enabled
                && self.serviceRunIntent.load(Ordering::Acquire))
            .then_some(currentGeneration);
            let _ = self.accountService.stop().await;
            match self.accountService.start(&currentConfiguration).await {
                Ok(_) => {
                    self.publishCurrentConfiguration().await;
                    if currentConfiguration.enabled
                        && accountRecoveryMayRestartProxy(
                            self.serviceRunIntent.load(Ordering::Acquire),
                            currentGeneration,
                            self.multiAccountGeneration.load(Ordering::Acquire),
                        )
                    {
                        match self.startServiceExclusive().await {
                            Ok(_) => {
                                pendingProxyRecoveryGeneration = None;
                                retryDelay = Duration::from_secs(1);
                            }
                            Err(error) => {
                                eprintln!("账号服务恢复后重启代理数据面失败：{}", error.message());
                                retryDelay = nextAccountRecoveryDelay(retryDelay);
                            }
                        }
                    } else {
                        pendingProxyRecoveryGeneration = None;
                        retryDelay = Duration::from_secs(1);
                    }
                }
                Err(error) => {
                    eprintln!("账号服务重启失败：{error}");
                    self.publishCurrentConfiguration().await;
                    retryDelay = nextAccountRecoveryDelay(retryDelay);
                }
            }
        }
    }

    /// 在已持有服务操作锁时启动数据面；只有融合 SOCKS 主监听成功才允许提交 Running。
    ///
    /// 运行上下文：普通启动和运行中配置替换复用同一生命周期实现。
    /// 失败语义：主监听失败返回启动错误；辅助监听失败只进入监听投影，不掩盖主数据面结果。
    pub(super) async fn startServiceExclusive(&self) -> Result<ControlSnapshot, ApiError> {
        self.startServiceExclusiveWithConfiguration(None, true)
            .await?;
        Ok(self.snapshot().await)
    }

    /// 在事务屏障内按当前权威配置启动但不发布中间事件；调用方提交最终投影后再释放屏障。
    pub(super) async fn startServiceExclusiveStaged(&self) -> Result<(), ApiError> {
        self.startServiceExclusiveWithConfiguration(None, false)
            .await
    }

    /// 使用私有候选配置启动融合数据面；候选验收期间不写共享配置，GET 与 SSE 只能看到旧权威状态。
    async fn startServiceExclusiveWithConfiguration(
        &self,
        candidate: Option<&ConfigurationTransactionSnapshot>,
        publishLifecycleEvents: bool,
    ) -> Result<(), ApiError> {
        let mut service = self.service.lock().await;
        if !matches!(service.state, ServiceState::Stopped | ServiceState::Faulted) {
            return Err(ApiError::conflict(ErrorCode::ServiceNotStartable));
        }
        service.state = ServiceState::Starting;
        service.socksError = None;
        service.errorMessage = None;
        let startingHttpConfiguration = self.httpConfiguration.read().await.clone();
        let startingListeners = listenerSnapshots(&service, &startingHttpConfiguration);
        if publishLifecycleEvents {
            self.publishProjectionRevisioned(|serverInstanceId, revision| {
                EventMessage::ServiceState {
                    serverInstanceId,
                    revision,
                    serviceState: ServiceState::Starting,
                    listeners: startingListeners,
                }
            });
        }
        let request = ServiceStartRequest {
            candidate,
            publishLifecycleEvents,
            startingHttpConfiguration,
        };
        let mut plan = self.prepareServiceStart(&mut service, &request).await?;
        self.startPrimaryDataPlane(&mut service, &mut plan).await;
        self.startConfiguredAuxiliary(&mut service, &plan).await;
        let outcome = self.commitServiceStartState(&mut service, &plan, publishLifecycleEvents);
        drop(service);
        if publishLifecycleEvents {
            self.publishRuntimeViews().await;
        }
        if outcome.running {
            return Ok(());
        }
        Err(match outcome.errorMessage {
            Some(detail) => {
                ApiError::internal(ErrorCode::ServiceStartFailed).withParam("detail", detail)
            }
            None => ApiError::internal(ErrorCode::ServiceStartFailed),
        })
    }

    /// 读取同一代次的启动配置并准备账号服务；账号服务失败会直接提交 Faulted，候选配置保持私有。
    async fn prepareServiceStart(
        &self,
        service: &mut ManagedService,
        request: &ServiceStartRequest<'_>,
    ) -> Result<ServiceStartPlan, ApiError> {
        let mut socks5 = match request.candidate {
            Some(candidate) => candidate.socks5.clone(),
            None => self.configuration.read().await.clone(),
        };
        let multiAccount = match request.candidate {
            Some(candidate) => candidate.multiAccount.clone(),
            None => self.multiAccountConfiguration.read().await.clone(),
        };
        let accountServiceEndpoint = match self.accountService.start(&multiAccount).await {
            Ok(endpoint) => endpoint,
            Err(detail) => {
                self.commitAccountServiceStartFailure(service, request);
                return Err(
                    ApiError::internal(ErrorCode::ServiceStartFailed).withParam("detail", detail)
                );
            }
        };
        let accountService = multiAccount.enabled.then(|| {
            socks5.authenticationMode = AuthenticationMode::AccountService;
            socks5.users.clear();
            AccountServiceClientConfig {
                endpoint: accountServiceEndpoint.internalEndpoint,
                internalToken: accountServiceEndpoint.internalToken,
                synchronizationIntervalMilliseconds: 2_000,
                requestTimeoutMilliseconds: 3_000,
            }
        });
        let http = match request.candidate {
            Some(candidate) => candidate.http.clone(),
            None => self.httpConfiguration.read().await.clone(),
        };
        let mut processCapture = match request.candidate {
            Some(candidate) => candidate.processCapture.clone(),
            None => self.processCaptureConfiguration.read().await.clone(),
        };
        processCapture.proxyPort = socks5.listenPort;
        processCapture.proxyAddress = socks5.listenHost;
        Ok(ServiceStartPlan {
            socks5,
            multiAccount,
            accountService,
            http,
            processCapture,
            auxiliary: self.auxiliaryConfiguration.read().await.clone(),
        })
    }

    /// 提交账号服务启动失败的唯一终态；诊断正文只返回调用方，不进入持久服务投影。
    fn commitAccountServiceStartFailure(
        &self,
        service: &mut ManagedService,
        request: &ServiceStartRequest<'_>,
    ) {
        service.state = ServiceState::Faulted;
        service.socksError = Some(listenerError(
            "accountServiceStartFailed",
            "error.serviceStartFailed",
        ));
        service.errorMessage = Some("[accountService] startFailed".to_owned());
        if !request.publishLifecycleEvents {
            return;
        }
        let listeners = listenerSnapshots(service, &request.startingHttpConfiguration);
        self.publishProjectionRevisioned(|serverInstanceId, revision| EventMessage::ServiceState {
            serverInstanceId,
            revision,
            serviceState: ServiceState::Faulted,
            listeners,
        });
    }

    /// 构建融合 HTTP/SOCKS 依赖并启动主监听；所有成功资源只在进程捕获也启动后写入 `service`。
    async fn startPrimaryDataPlane(
        &self,
        service: &mut ManagedService,
        plan: &mut ServiceStartPlan,
    ) {
        let dnsSpoofing = self.tools.dnsSpoofing();
        let httpDependencies = HttpProxyDependencies {
            capture: self.recording.clone(),
            ssl: self.ssl.clone(),
            pipeline: self.toolPipeline(),
            pluginHost: self.pluginHost.clone(),
            dnsSpoofing: dnsSpoofing.clone(),
        };
        let tunnelInspector = SocksHttpInspector::newWithDns(
            plan.http.configuration.clone(),
            httpDependencies.clone(),
        );
        let httpHandler =
            buildHttpConnectionHandler(plan.http.configuration.clone(), httpDependencies);
        let outboundConnector = transport_core::OutboundConnector::new(
            plan.http.configuration.upstreamProxy.clone(),
            plan.http.configuration.connectTimeout(),
        )
        .expect("HTTP 配置已在融合监听器启动前完成二级代理校验");
        let (Ok(tunnelInspector), Ok(httpHandler)) = (tunnelInspector, httpHandler) else {
            service.socksError = Some(listenerError(
                "proxyListenerStartFailed",
                "error.serviceStartFailed",
            ));
            return;
        };
        // HTTP 在融合监听中绕过 SOCKS 会话注册表，必须先订阅指标再开放端口，首条快速连接才能进入观察窗口。
        let httpMetrics = httpHandler.runtimeMetrics();
        let httpMetricChanges = httpMetrics.subscribeChanges();
        let tunnelInterceptor = Arc::new(tunnelInspector.clone())
            as Arc<dyn socks5_core::interception::TcpTunnelInterceptor>;
        let unifiedHandler = Arc::new(UnifiedProtocolHandler {
            http: httpHandler,
            inspector: tunnelInspector,
            processCapture: self.processCapture.clone(),
            outbound: outboundConnector.clone(),
            processSelection: self.processSelection.clone(),
            transparentRecording: TransparentRecording::new(self.recording.clone()),
            pluginHost: self.pluginHost.clone(),
        }) as Arc<dyn PortProtocolHandler>;
        let runningServer = startFusedProxyServer(
            plan.socks5.clone(),
            FusedProxyDependencies {
                pluginHost: self.pluginHost.clone(),
                tunnelInterceptor: Some(tunnelInterceptor),
                addressOverride: Some(Arc::new(SocksDnsOverride {
                    tool: dnsSpoofing,
                    ruleServiceIp: localRuleServiceIp(&plan.multiAccount),
                })),
                protocolHandler: Some(unifiedHandler),
                outboundConnector: Some(outboundConnector),
            },
            FusedProxyOptions {
                // 内部入口与公开代理共享生命周期但不公开；常驻入口避免进程捕获热启停中断已有连接。
                enableInternalCaptureListener: true,
                accountServiceConfig: plan.accountService.clone(),
            },
        )
        .await;
        let Ok(runningServer) = runningServer else {
            service.socksError = Some(listenerError(
                "socks5StartFailed",
                "error.serviceStartFailed",
            ));
            return;
        };
        self.activateRunningServer(
            service,
            plan,
            PrimaryActivation {
                runningServer,
                httpMetrics,
                httpMetricChanges,
            },
        )
        .await;
    }

    /// 启动进程捕获并提交主服务所有权；任一步失败都会先停录制代际和监听器，不留下半运行资源。
    async fn activateRunningServer(
        &self,
        service: &mut ManagedService,
        plan: &mut ServiceStartPlan,
        activation: PrimaryActivation,
    ) {
        let PrimaryActivation {
            runningServer,
            httpMetrics,
            httpMetricChanges,
        } = activation;
        let internalCaptureAddress = runningServer
            .internalCaptureAddress()
            .expect("融合服务必须持有热启停使用的内部双栈捕获入口");
        plan.processCapture.proxyAddress = internalCaptureAddress.ip();
        plan.processCapture.proxyPort = internalCaptureAddress.port();
        let udpRecording = crate::udpRecording::startCoordinatedUdpRecordingGeneration(
            &self.dataDirectory,
            self.recording.clone(),
            Arc::new(self.processSelection.clone()),
            Arc::clone(&self.udpRecordingCoordination),
            Arc::clone(&self.recordingUpdateLock),
        );
        let processCaptureStart = match udpRecording.as_ref() {
            Ok(runtime) => {
                self.processCapture.setUdpDatagramSink(Some(runtime.sink()));
                self.processCapture.start(plan.processCapture.clone())
            }
            Err(error) => Err(ProcessCaptureError::Worker {
                worker: "UDP 录制代际创建",
                detail: error.to_string(),
            }),
        };
        if let Err(error) = processCaptureStart {
            // WinDivert 在监听绑定后加载；失败时必须按录制代际、监听器顺序回收，再提交 Faulted。
            self.processCapture.setUdpDatagramSink(None);
            if let Ok(runtime) = udpRecording {
                let _ = runtime.stopAndDrain().await;
            }
            let _ = runningServer.stop().await;
            service.socksError = Some(listenerError(
                "processCaptureStartFailed",
                "error.serviceStartFailed",
            ));
            service.errorMessage = Some(format!("[processCapture] {error}"));
            return;
        }
        service.udpRecording = udpRecording.ok();
        let sessionEvents = runningServer.subscribeEvents();
        let captureGeneration = runningServer.captureGeneration();
        // 订阅先于基线读取；绑定成功到控制 API 返回之间的连接不会落入观察窗口之外。
        let initialSessions = runningServer.snapshot().sessions;
        let exitReceiver = runningServer.subscribeExit();
        service.captureGeneration = Some(captureGeneration.clone());
        service.runningServer = Some(runningServer);
        service.httpMetrics = Some(httpMetrics);
        let forwardingState = self.clone();
        let sessionHistoryLimit = plan.socks5.sessionHistoryLimit;
        service.eventForwarder = Some(tokio::spawn(async move {
            forwardRuntimeEvents(
                forwardingState,
                sessionEvents,
                captureGeneration,
                initialSessions,
                sessionHistoryLimit,
            )
            .await;
        }));
        let metricState = self.clone();
        service.httpMetricForwarder = Some(tokio::spawn(async move {
            forwardHttpMetricEvents(metricState, httpMetricChanges).await;
        }));
        let exitState = self.clone();
        service.exitMonitor = Some(tokio::spawn(async move {
            if waitForServerExit(exitReceiver).await.is_some() {
                exitState.handleUnexpectedServerExit().await;
            }
        }));
    }

    /// 主数据面成功后按同一配置启动附属监听；附属失败只写诊断，不能把主监听回滚或伪装成未运行。
    async fn startConfiguredAuxiliary(
        &self,
        service: &mut ManagedService,
        plan: &ServiceStartPlan,
    ) {
        if service.runningServer.is_none()
            || !(plan
                .auxiliary
                .reverseProxies
                .iter()
                .any(|entry| entry.enabled)
                || plan
                    .auxiliary
                    .portForwards
                    .iter()
                    .any(|entry| entry.enabled))
        {
            return;
        }
        match startAuxiliaryListeners(
            plan.auxiliary.clone(),
            plan.http.configuration.clone(),
            self.recording.clone(),
            self.ssl.clone(),
            self.toolPipeline(),
            CancellationToken::new(),
        )
        .await
        {
            Ok(listeners) => service.runningAuxiliaryListeners = Some(listeners),
            Err(error) => {
                service.errorMessage = Some(format!("[auxiliary] {}", error.code()));
            }
        }
    }

    /// 在 `service` 临界区一次性提交最终状态与监听投影；返回值只携带锁外响应所需的脱敏结果。
    fn commitServiceStartState(
        &self,
        service: &mut ManagedService,
        plan: &ServiceStartPlan,
        publishLifecycleEvents: bool,
    ) -> ServiceStartOutcome {
        // 多账号认证、共享限速和连接租约都挂在融合 SOCKS 主数据面上；辅助监听器只能作为附属能力，
        // 绝不能把主监听绑定失败伪装成服务已运行，否则已提交的账号策略不会作用于任何公开入口。
        let running = primaryDataPlaneCommitSucceeded(
            service.runningServer.is_some(),
            service.runningAuxiliaryListeners.is_some(),
        );
        if service.errorMessage.is_none() {
            service.errorMessage = aggregateListenerErrors(service);
        }
        service.state = if running {
            ServiceState::Running
        } else {
            ServiceState::Faulted
        };
        let finalState = service.state;
        let listeners = listenerSnapshots(service, &plan.http);
        // 服务状态与对应修订必须在同一 service 临界区提交；快照读取者取得锁后只能看到提交前或提交后状态。
        if publishLifecycleEvents {
            self.publishProjectionRevisioned(|serverInstanceId, revision| {
                EventMessage::ServiceState {
                    serverInstanceId,
                    revision,
                    serviceState: finalState,
                    listeners,
                }
            });
        }
        ServiceStartOutcome {
            running,
            errorMessage: service.errorMessage.clone(),
        }
    }

    /// 归档融合 SOCKS 意外退出并统一回收辅助监听、捕获与录制代际，最终状态固定为 Faulted。
    ///
    /// 运行上下文：退出监视任务调用，生命周期锁保证清理完成前新启动无法进入。
    /// 失败语义：辅助监听关闭错误写入日志；主服务终态仍完整归档，后续显式停止可转换为 Stopped。
    pub(super) async fn handleUnexpectedServerExit(&self) {
        // 意外退出与显式启停必须共享同一操作锁。若先发布 Faulted 再无锁排空旧 UDP
        // 代际，新的 start 会并发打开同一 spool 目录，破坏单 writer、单 reader 和捕获顺序。
        // 持锁覆盖完整清理后，下一代只能在旧代际提交完正文并释放文件句柄后创建。
        let _operationGuard = self.serviceOperationLock.lock().await;
        let (
            runningServer,
            runningAuxiliaryListeners,
            eventForwarder,
            httpMetricForwarder,
            udpRecording,
            exitingCaptureGeneration,
        ) = {
            let mut service = self.service.lock().await;
            if service.state != ServiceState::Running {
                return;
            }
            service.socksError = Some(listenerError(
                "socks5RuntimeFailed",
                "error.serviceStartFailed",
            ));
            let runningServer = service.runningServer.take();
            let runningAuxiliaryListeners = service.runningAuxiliaryListeners.take();
            let eventForwarder = service.eventForwarder.take();
            let httpMetricForwarder = service.httpMetricForwarder.take();
            let udpRecording = service.udpRecording.take();
            let exitingCaptureGeneration = service.captureGeneration.clone();
            service.state = ServiceState::Faulted;
            service.errorMessage = aggregateListenerErrors(&service);
            let httpConfiguration = self.httpConfiguration.read().await;
            let listeners = listenerSnapshots(&service, &httpConfiguration);
            self.publishProjectionRevisioned(|serverInstanceId, revision| {
                EventMessage::ServiceState {
                    serverInstanceId,
                    revision,
                    serviceState: ServiceState::Faulted,
                    listeners,
                }
            });
            (
                runningServer,
                runningAuxiliaryListeners,
                eventForwarder,
                httpMetricForwarder,
                udpRecording,
                exitingCaptureGeneration,
            )
        };
        let _ = self.processCapture.stop();
        self.processCapture.setUdpDatagramSink(None);
        if let Some(runtime) = udpRecording {
            let _ = runtime.stopAndDrain().await;
        }
        let stopOutcome = match runningServer {
            Some(server) => Some(server.stop().await),
            None => None,
        };
        if let Some(listeners) = runningAuxiliaryListeners
            && let Err(error) = listeners.stop().await
        {
            eprintln!("融合 SOCKS 意外退出后回收辅助监听器失败：{}", error.code());
        }
        {
            // HTTP 监听已经停止，原子账本不会再变化；在同一锁内从当前槽位迁入历史，
            // 让尚处于合并定时器中的转发任务始终读取到等价权威值。
            let mut service = self.service.lock().await;
            archiveHttpRuntimeMetrics(&mut service);
        }
        drainRuntimeEventForwarder(httpMetricForwarder).await;
        drainRuntimeEventForwarder(eventForwarder).await;
        let historyLimit = self.configuration.read().await.sessionHistoryLimit;
        {
            let mut service = self.service.lock().await;
            if let Some(stopOutcome) = stopOutcome {
                // 意外退出也必须在终态投影排空后归档最终快照，避免正文镜像或末尾指标跨周期残留。
                archiveRuntimeSnapshot(&mut service, stopOutcome.snapshot, historyLimit);
            }
            // 只释放本次退出所属句柄，避免 clear 与退出归档交错时重新接纳已删除的旧队列事件。
            releaseExitedCaptureGeneration(&mut service, exitingCaptureGeneration.as_ref());
        }
        self.publishRuntimeViews().await;
    }

    /// 在已持有服务操作锁时终止全部数据面；活动 SOCKS5 与 HTTP 转发连接会在本次停止中关闭。
    ///
    /// 运行上下文：配置重启调用本函数先回收旧监听器、会话任务和断点等待项。
    /// 失败语义：任一数据面停止失败时返回错误，调用方不得把新配置写入运行中的旧实例。
    pub(super) async fn stopServiceExclusive(&self) -> Result<ControlSnapshot, ApiError> {
        self.stopServiceExclusiveWithEvents(true).await?;
        Ok(self.snapshot().await)
    }

    /// 回收全部数据面；staged 回滚传 false，避免候选 Stopping/Stopped 在旧配置恢复前进入 SSE。
    async fn stopServiceExclusiveWithEvents(
        &self,
        publishLifecycleEvents: bool,
    ) -> Result<(), ApiError> {
        let (
            runningServer,
            runningAuxiliaryListeners,
            eventForwarder,
            httpMetricForwarder,
            exitMonitor,
            udpRecording,
        ) = {
            let mut service = self.service.lock().await;
            if service.state == ServiceState::Stopped {
                return Ok(());
            }
            if service.state == ServiceState::Faulted
                && service.runningServer.is_none()
                && service.runningAuxiliaryListeners.is_none()
                && service.udpRecording.is_none()
            {
                service.state = ServiceState::Stopped;
                service.socksError = None;
                service.errorMessage = None;
                let httpConfiguration = self.httpConfiguration.read().await.clone();
                let listeners = listenerSnapshots(&service, &httpConfiguration);
                if publishLifecycleEvents {
                    self.publishProjectionRevisioned(|serverInstanceId, revision| {
                        EventMessage::ServiceState {
                            serverInstanceId,
                            revision,
                            serviceState: ServiceState::Stopped,
                            listeners,
                        }
                    });
                }
                return Ok(());
            }
            if !serviceStateCanEnterStop(service.state) {
                return Err(ApiError::conflict(ErrorCode::ServiceNotStoppable));
            }
            service.state = ServiceState::Stopping;
            let httpConfiguration = self.httpConfiguration.read().await.clone();
            let listeners = listenerSnapshots(&service, &httpConfiguration);
            if publishLifecycleEvents {
                self.publishProjectionRevisioned(|serverInstanceId, revision| {
                    EventMessage::ServiceState {
                        serverInstanceId,
                        revision,
                        serviceState: ServiceState::Stopping,
                        listeners,
                    }
                });
            }
            (
                service.runningServer.take(),
                service.runningAuxiliaryListeners.take(),
                service.eventForwarder.take(),
                service.httpMetricForwarder.take(),
                service.exitMonitor.take(),
                service.udpRecording.take(),
            )
        };
        // 停止数据面前先唤醒全部断点等待者；否则连接任务可能在监听器退出后仍持有暂停槽位。
        self.releaseBreakpointQueue();
        // WinDivert 必须先恢复网络路径再关闭本地监听器，避免已反射连接继续命中失效端口。
        let processCaptureStop = self.processCapture.stop();
        self.processCapture.setUdpDatagramSink(None);
        if let Some(exitMonitor) = exitMonitor {
            exitMonitor.abort();
            let _ = exitMonitor.await;
        }
        let socksStop = async move {
            match runningServer {
                Some(server) => Some(server.stop().await),
                None => None,
            }
        };
        let auxiliaryStop = async move {
            match runningAuxiliaryListeners {
                Some(listeners) => Some(listeners.stop().await),
                None => None,
            }
        };
        let (stopOutcome, auxiliaryStopOutcome) = tokio::join!(socksStop, auxiliaryStop);
        {
            // 数据面停止后立即在服务锁内迁移 HTTP 账本；即使转发任务的 50ms 定时器随后触发，
            // `publishMetricsView` 也只能读到数值相同的历史账本，不会产生高 revision 回退。
            let mut service = self.service.lock().await;
            archiveHttpRuntimeMetrics(&mut service);
        }
        let udpRecordingStop = match udpRecording {
            Some(runtime) => runtime.stopAndDrain().await,
            None => Ok(()),
        };
        // 镜像仅在代理链已停止接纳新报文后排空，确保 stop 成功返回时不会再有挂起文件写入。
        let mirrorFlushOutcome = self.flushMirrorWrites().await;
        // SOCKS5 停止会先发布每个活动会话的终态；投影任务排空后，最终快照才可对外承诺事务已完成。
        drainRuntimeEventForwarder(eventForwarder).await;
        drainRuntimeEventForwarder(httpMetricForwarder).await;
        let historyLimit = self.configuration.read().await.sessionHistoryLimit;
        let mut service = self.service.lock().await;
        let mut stopDiagnostics = Vec::new();
        if let Err(error) = processCaptureStop {
            stopDiagnostics.push(format!("[processCapture] {error}"));
        }
        if let Err(error) = udpRecordingStop {
            stopDiagnostics.push(format!("[udpRecording] {error}"));
        }
        if let Some(stopOutcome) = stopOutcome {
            archiveRuntimeSnapshot(&mut service, stopOutcome.snapshot, historyLimit);
            if let Some(errorMessage) = stopOutcome.errorMessage {
                service.socksError =
                    Some(listenerError("socks5StopFailed", "error.serviceStopFailed"));
                stopDiagnostics.push(format!("[socks5] {errorMessage}"));
            }
        }
        // 投影任务已经排空且归档已移除正文，停止周期的代际句柄不再参与 clear 竞态。
        service.captureGeneration = None;
        if let Some(Err(error)) = auxiliaryStopOutcome {
            stopDiagnostics.push(format!("[auxiliary] {}", error.code()));
        }
        if let Err(error) = mirrorFlushOutcome {
            let _ = error;
            stopDiagnostics.push("[mirror] mirrorFlushFailed".to_owned());
        }
        let stopError = if stopDiagnostics.is_empty() {
            service.state = ServiceState::Stopped;
            service.socksError = None;
            service.errorMessage = None;
            None
        } else {
            let detail = stopDiagnostics.join("; ");
            service.state = ServiceState::Faulted;
            // 对外快照只暴露稳定、本地化的监听错误；底层停止诊断仅保留在当前失败响应中。
            service.errorMessage = aggregateListenerErrors(&service);
            Some(ApiError::internal(ErrorCode::ServiceStopFailed).withParam("detail", detail))
        };
        let finalState = service.state;
        let httpConfiguration = self.httpConfiguration.read().await.clone();
        let listeners = listenerSnapshots(&service, &httpConfiguration);
        // 停止终态和修订号在 service 锁释放前一起发布，禁止并发快照读取旧修订与新终态。
        if publishLifecycleEvents {
            self.publishProjectionRevisioned(|serverInstanceId, revision| {
                EventMessage::ServiceState {
                    serverInstanceId,
                    revision,
                    serviceState: finalState,
                    listeners,
                }
            });
        }
        drop(service);
        if publishLifecycleEvents {
            self.publishRuntimeViews().await;
        }
        if let Some(stopError) = stopError {
            Err(stopError)
        } else {
            Ok(())
        }
    }

    /// 原子替换服务配置；运行或故障中的数据面先强制停止并断开全部转发连接，再以新配置启动。
    ///
    /// 运行上下文：设置对话框提交完整配置时调用，操作锁覆盖校验、停止、写入和重启全过程。
    /// 参数：update 为已反序列化的完整公开配置，认证口令仅在本次更新中用于构建内部配置。
    /// 失败语义：校验、持久化、账号服务或新数据面任一步失败都会恢复旧磁盘、内存和运行状态；恢复错误合并返回。
    pub async fn replaceConfiguration(
        &self,
        update: ConfigurationUpdate,
    ) -> Result<ControlSnapshot, ApiError> {
        let _operationGuard = self.serviceOperationLock.lock().await;
        let _transactionBarrier = ConfigurationTransactionBarrier::activate(self);
        let startServiceOnLaunch = update.startServiceOnLaunch;
        let current = self.configuration.read().await.clone();
        let currentHttp = self.httpConfiguration.read().await.clone();
        let (
            configuration,
            httpConfiguration,
            processCaptureConfiguration,
            multiAccountConfiguration,
        ) = update.intoInternal(&current, &currentHttp)?;
        let auxiliaryConfiguration = self.auxiliaryConfiguration.read().await.clone();
        listenerControl::validateAuxiliaryListenerConfiguration(
            &auxiliaryConfiguration,
            &configuration,
            &httpConfiguration,
            &multiAccountConfiguration,
        )?;
        let previousMultiAccountConfiguration = self.multiAccountConfiguration.read().await.clone();
        let previousProcessCaptureConfiguration =
            self.processCaptureConfiguration.read().await.clone();
        let previousStartServiceOnLaunch = self.startServiceOnLaunch.load(Ordering::Acquire);
        let previous = ConfigurationTransactionSnapshot {
            socks5: current,
            http: currentHttp,
            processCapture: previousProcessCaptureConfiguration,
            multiAccount: previousMultiAccountConfiguration,
            startServiceOnLaunch: previousStartServiceOnLaunch,
        };
        let candidate = ConfigurationTransactionSnapshot {
            socks5: configuration.clone(),
            http: httpConfiguration.clone(),
            processCapture: processCaptureConfiguration.clone(),
            multiAccount: multiAccountConfiguration.clone(),
            startServiceOnLaunch,
        };

        let restartRequired = {
            let service = self.service.lock().await;
            service.state != ServiceState::Stopped
        };
        if restartRequired {
            self.stopServiceExclusiveWithEvents(false).await?;
        }
        let multiAccountChanged = multiAccountConfiguration != previous.multiAccount;
        if multiAccountChanged
            && let Err(error) = self
                .switchAccountService(&previous.multiAccount, &multiAccountConfiguration)
                .await
        {
            let error = self
                .restoreConfigurationTransaction(ConfigurationRollbackContext {
                    previous: &previous,
                    candidate: &candidate,
                    durability: ConfigurationDurability::Previous,
                    restartRequired,
                    originalError: error,
                })
                .await;
            drop(_transactionBarrier);
            self.publishCurrentServiceState().await;
            self.publishRuntimeViews().await;
            return Err(error);
        }
        if let Err(error) = self.persistConfiguration(&candidate) {
            let error = self
                .restoreConfigurationTransaction(ConfigurationRollbackContext {
                    previous: &previous,
                    candidate: &candidate,
                    durability: ConfigurationDurability::Previous,
                    restartRequired,
                    originalError: error,
                })
                .await;
            drop(_transactionBarrier);
            self.publishCurrentServiceState().await;
            self.publishRuntimeViews().await;
            return Err(error);
        }

        if restartRequired {
            match self
                .startServiceExclusiveWithConfiguration(Some(&candidate), false)
                .await
            {
                Ok(_) => {
                    self.commitConfigurationMemory(&candidate, multiAccountChanged)
                        .await;
                    self.publishCommittedConfiguration(
                        ConfigurationProjectionSource {
                            socks5: &configuration,
                            http: &httpConfiguration,
                            processCapture: &processCaptureConfiguration,
                            startServiceOnLaunch,
                        },
                        &multiAccountConfiguration,
                    )
                    .await;
                    self.publishCurrentServiceState().await;
                    drop(_transactionBarrier);
                    self.publishRuntimeViews().await;
                    Ok(self.snapshot().await)
                }
                Err(error) => {
                    let error = self
                        .restoreConfigurationTransaction(ConfigurationRollbackContext {
                            previous: &previous,
                            candidate: &candidate,
                            durability: ConfigurationDurability::Candidate,
                            restartRequired: true,
                            originalError: error,
                        })
                        .await;
                    drop(_transactionBarrier);
                    self.publishCurrentServiceState().await;
                    self.publishRuntimeViews().await;
                    Err(error)
                }
            }
        } else {
            self.commitConfigurationMemory(&candidate, multiAccountChanged)
                .await;
            self.publishCommittedConfiguration(
                ConfigurationProjectionSource {
                    socks5: &configuration,
                    http: &httpConfiguration,
                    processCapture: &processCaptureConfiguration,
                    startServiceOnLaunch,
                },
                &multiAccountConfiguration,
            )
            .await;
            drop(_transactionBarrier);
            Ok(self.snapshot().await)
        }
    }

    /// 在数据面验收前原子提交候选配置的 durable 状态；失败时旧配置文件仍是唯一权威版本。
    fn persistConfiguration(
        &self,
        configuration: &ConfigurationTransactionSnapshot,
    ) -> Result<(), ApiError> {
        self.processSelection
            .replaceServiceConfiguration(configuration.persistedConfiguration())
            .map_err(|error| {
                ApiError::internal(ErrorCode::ConfigurationPersistenceFailed)
                    .withParam("detail", error.to_string())
            })
    }

    /// 一次写入 durable 权威配置的四份共享状态；停止态提交和回滚不要求存在运行监听器。
    async fn commitConfigurationMemory(
        &self,
        configuration: &ConfigurationTransactionSnapshot,
        multiAccountChanged: bool,
    ) {
        let mut socks5Guard = self.configuration.write().await;
        let mut httpGuard = self.httpConfiguration.write().await;
        let mut processCaptureGuard = self.processCaptureConfiguration.write().await;
        let mut multiAccountGuard = self.multiAccountConfiguration.write().await;
        *socks5Guard = configuration.socks5.clone();
        *httpGuard = configuration.http.clone();
        *processCaptureGuard = configuration.processCapture.clone();
        *multiAccountGuard = configuration.multiAccount.clone();
        if multiAccountChanged {
            self.multiAccountGeneration.fetch_add(1, Ordering::AcqRel);
        }
        self.startServiceOnLaunch
            .store(configuration.startServiceOnLaunch, Ordering::Release);
    }

    /// 在候选主数据面验收完成后发布配置事件；运行中替换失败时 SSE 永远看不到候选配置。
    pub(super) async fn publishCommittedConfiguration(
        &self,
        source: ConfigurationProjectionSource<'_>,
        multiAccountConfiguration: &MultiAccountConfiguration,
    ) {
        let multiAccount = self
            .accountService
            .publicState(multiAccountConfiguration)
            .await;
        self.publishProjectionRevisioned(|serverInstanceId, revision| {
            EventMessage::Configuration {
                serverInstanceId,
                revision,
                configuration: Box::new(PublicConfiguration::fromInternal(source, multiAccount)),
            }
        });
    }

    /// 发布当前脱敏配置与账号服务身份状态；管理身份和 API Key 指纹变化通过统一 Configuration 事件同步。
    pub(super) async fn publishCurrentConfiguration(&self) {
        let configuration = self.configuration.read().await.clone();
        let httpConfiguration = self.httpConfiguration.read().await.clone();
        let processCapture = self.processCaptureConfiguration.read().await.clone();
        let multiAccount = self.multiAccountConfiguration.read().await.clone();
        self.publishCommittedConfiguration(
            ConfigurationProjectionSource {
                socks5: &configuration,
                http: &httpConfiguration,
                processCapture: &processCapture,
                startServiceOnLaunch: self.startServiceOnLaunch.load(Ordering::Acquire),
            },
            &multiAccount,
        )
        .await;
    }

    /// 恢复配置替换事务的旧权威状态；所有恢复步骤都会执行，避免单个失败遮蔽其余可恢复资源。
    ///
    /// 运行上下文：调用方持有服务操作锁；`Previous` 表示候选尚未 durable 提交，`Candidate` 表示候选已落盘。
    /// 参数上下文同时保留事务前运行意图；返回值始终保留原始错误，并把全部恢复错误写入 detail。
    async fn restoreConfigurationTransaction(
        &self,
        context: ConfigurationRollbackContext<'_>,
    ) -> ApiError {
        let ConfigurationRollbackContext {
            previous,
            candidate,
            durability,
            restartRequired,
            originalError,
        } = context;
        let mut restoreErrors = Vec::new();
        let serviceNeedsStop = self.service.lock().await.state != ServiceState::Stopped;
        if serviceNeedsStop && let Err(error) = self.stopServiceExclusiveWithEvents(false).await {
            restoreErrors.push(format!("停止候选数据面失败：{}", error.message()));
        }
        if durability == ConfigurationDurability::Candidate
            && let Err(error) = self
                .processSelection
                .replaceServiceConfiguration(previous.persistedConfiguration())
        {
            // 磁盘恢复失败时候选文件仍是唯一持久化权威，内存必须与之对齐，禁止继续启动旧数据面形成分叉。
            restoreErrors.push(format!("恢复配置文件失败：{error}"));
            self.commitConfigurationMemory(candidate, true).await;
            self.publishCommittedSnapshot(candidate).await;
            if restartRequired
                && let Err(error) = self
                    .startServiceExclusiveWithConfiguration(Some(candidate), false)
                    .await
            {
                restoreErrors.push(format!("恢复候选代理数据面失败：{}", error.message()));
            }
            return mergeConfigurationTransactionError(originalError, restoreErrors);
        }
        if let Err(error) = self
            .switchAccountService(&candidate.multiAccount, &previous.multiAccount)
            .await
        {
            restoreErrors.push(format!("恢复账号服务失败：{}", error.message()));
        }
        self.restoreConfigurationMemoryAndPublish(previous).await;
        if restartRequired
            && let Err(error) = self
                .startServiceExclusiveWithConfiguration(Some(previous), false)
                .await
        {
            restoreErrors.push(format!("恢复代理数据面失败：{}", error.message()));
        }
        mergeConfigurationTransactionError(originalError, restoreErrors)
    }

    /// 发布完整事务快照对应的配置事件；仅在磁盘与内存已经对齐该快照后调用。
    async fn publishCommittedSnapshot(&self, configuration: &ConfigurationTransactionSnapshot) {
        self.publishCommittedConfiguration(
            ConfigurationProjectionSource {
                socks5: &configuration.socks5,
                http: &configuration.http,
                processCapture: &configuration.processCapture,
                startServiceOnLaunch: configuration.startServiceOnLaunch,
            },
            &configuration.multiAccount,
        )
        .await;
    }

    /// 在 staged 候选提交后发布唯一服务终态；候选 Starting/Faulted 不会提前进入 SSE。
    pub(super) async fn publishCurrentServiceState(&self) {
        let service = self.service.lock().await;
        let httpConfiguration = self.httpConfiguration.read().await.clone();
        let listeners = listenerSnapshots(&service, &httpConfiguration);
        let serviceState = service.state;
        self.publishProjectionRevisioned(|serverInstanceId, revision| EventMessage::ServiceState {
            serverInstanceId,
            revision,
            serviceState,
            listeners,
        });
    }

    /// 发布事务恢复后的旧配置视图；账号服务状态在发布前重新读取，避免恢复事件携带候选实例指纹。
    async fn restoreConfigurationMemoryAndPublish(
        &self,
        configuration: &ConfigurationTransactionSnapshot,
    ) {
        let multiAccount = self
            .accountService
            .publicState(&configuration.multiAccount)
            .await;
        let mut socks5Guard = self.configuration.write().await;
        let mut httpGuard = self.httpConfiguration.write().await;
        let mut processCaptureGuard = self.processCaptureConfiguration.write().await;
        let mut multiAccountGuard = self.multiAccountConfiguration.write().await;
        *socks5Guard = configuration.socks5.clone();
        *httpGuard = configuration.http.clone();
        *processCaptureGuard = configuration.processCapture.clone();
        *multiAccountGuard = configuration.multiAccount.clone();
        self.startServiceOnLaunch
            .store(configuration.startServiceOnLaunch, Ordering::Release);
        self.multiAccountGeneration.fetch_add(1, Ordering::AcqRel);
        self.publishProjectionRevisioned(|serverInstanceId, revision| {
            EventMessage::Configuration {
                serverInstanceId,
                revision,
                configuration: Box::new(PublicConfiguration::fromInternal(
                    ConfigurationProjectionSource {
                        socks5: &configuration.socks5,
                        http: &configuration.http,
                        processCapture: &configuration.processCapture,
                        startServiceOnLaunch: configuration.startServiceOnLaunch,
                    },
                    multiAccount,
                )),
            }
        });
        drop(multiAccountGuard);
        drop(processCaptureGuard);
        drop(httpGuard);
        drop(socks5Guard);
    }

    /// 从 source 账号配置切换到 target；关闭、候选启动或来源恢复任一失败均返回结构化错误。
    ///
    /// 运行上下文：仅在配置事务锁内调用；target 启动失败时立即恢复 source，禁止遗留候选进程。
    /// 失败语义：原失败和恢复失败合并到 detail，调用方继续执行完整配置事务恢复。
    async fn switchAccountService(
        &self,
        source: &MultiAccountConfiguration,
        target: &MultiAccountConfiguration,
    ) -> Result<(), ApiError> {
        if source == target {
            return Ok(());
        }
        let stopError = self.accountService.stop().await.err();
        if let Err(detail) = self.accountService.start(target).await {
            let mut failureDetail = detail;
            if let Err(restoreError) = self.accountService.start(source).await {
                failureDetail.push_str(&format!("; 恢复原账号服务失败：{restoreError}"));
            }
            return Err(ApiError::internal(ErrorCode::ServiceStartFailed)
                .withParam("detail", failureDetail));
        }
        match stopError {
            Some(error) => {
                Err(ApiError::internal(ErrorCode::ServiceStopFailed).withParam("detail", error))
            }
            None => Ok(()),
        }
    }
}

/// 保存配置替换前完整权威状态；恢复路径据此重写磁盘、内存并重启旧数据面。
struct ConfigurationTransactionSnapshot {
    socks5: Socks5Config,
    http: ManagedHttpProxyConfiguration,
    processCapture: ProcessCaptureConfiguration,
    multiAccount: MultiAccountConfiguration,
    startServiceOnLaunch: bool,
}

/// 标识失败发生时磁盘唯一权威配置；恢复路径据此决定是否允许重写旧文件。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigurationDurability {
    Previous,
    Candidate,
}

/// 聚合回滚所需完整事务上下文，避免阶段标记与快照参数在调用点错位。
struct ConfigurationRollbackContext<'a> {
    previous: &'a ConfigurationTransactionSnapshot,
    candidate: &'a ConfigurationTransactionSnapshot,
    durability: ConfigurationDurability,
    restartRequired: bool,
    originalError: ApiError,
}

/// 在配置事务期间屏蔽公开快照；析构时唤醒等待者，所有返回路径都能可靠释放屏障。
pub(super) struct ConfigurationTransactionBarrier<'a> {
    state: &'a ControlState,
}

impl<'a> ConfigurationTransactionBarrier<'a> {
    /// 激活事务屏障；调用方已经持有配置操作锁，因此不会与另一配置事务重叠。
    pub(super) fn activate(state: &'a ControlState) -> Self {
        state.configurationTransactionSender.send_replace(true);
        Self { state }
    }
}

impl Drop for ConfigurationTransactionBarrier<'_> {
    /// 释放事务屏障并唤醒全部快照等待者；Release 保证它们观察到最终配置与服务句柄。
    fn drop(&mut self) {
        self.state
            .configurationTransactionSender
            .send_replace(false);
    }
}

impl ConfigurationTransactionSnapshot {
    /// 生成带秘密字段的旧持久化配置；仅写入本地配置文件，不进入公开事件或日志。
    fn persistedConfiguration(&self) -> ConfigurationUpdate {
        ConfigurationUpdate::fromInternal(
            ConfigurationProjectionSource {
                socks5: &self.socks5,
                http: &self.http,
                processCapture: &self.processCapture,
                startServiceOnLaunch: self.startServiceOnLaunch,
            },
            self.multiAccount.clone(),
        )
    }
}

/// 合并配置事务原始错误与全部恢复错误；恢复成功时保持原机器错误码，失败细节不会被静默丢弃。
fn mergeConfigurationTransactionError(
    originalError: ApiError,
    restoreErrors: Vec<String>,
) -> ApiError {
    if restoreErrors.is_empty() {
        return originalError;
    }
    let originalMessage = originalError.message();
    originalError.withParam(
        "detail",
        format!(
            "{}; 配置事务恢复失败：{}",
            originalMessage,
            restoreErrors.join("; ")
        ),
    )
}

/// 计算监督器下一次有界退避；一秒起步、三十秒封顶，避免持续故障形成忙循环。
fn nextAccountRecoveryDelay(current: Duration) -> Duration {
    (current * 2).min(Duration::from_secs(30))
}

/// 判定启动事务是否拥有可提交的主数据面；辅助监听器只补充能力，不能独立构成代理服务成功。
fn primaryDataPlaneCommitSucceeded(
    fusedSocksRunning: bool,
    _auxiliaryListenersRunning: bool,
) -> bool {
    fusedSocksRunning
}

/// 判定服务终态是否可进入资源回收；Faulted 可能仍持有候选辅助监听，必须允许统一 stop 闭环。
fn serviceStateCanEnterStop(state: ServiceState) -> bool {
    matches!(state, ServiceState::Running | ServiceState::Faulted)
}

/// 核对故障观察是否仍对应锁内权威配置；代际、内容或启用状态变化都会取消恢复。
fn accountRecoveryMatchesCurrentConfiguration(
    observedGeneration: u64,
    currentGeneration: u64,
    observed: &MultiAccountConfiguration,
    current: &MultiAccountConfiguration,
) -> bool {
    observedGeneration == currentGeneration && observed == current
}

/// 判断恢复完成后是否仍应重启代理；显式停止或配置代际推进都会取消重启。
fn accountRecoveryMayRestartProxy(
    serviceRunIntent: bool,
    observedGeneration: u64,
    currentGeneration: u64,
) -> bool {
    serviceRunIntent && observedGeneration == currentGeneration
}

#[cfg(test)]
#[path = "../../tests/unit/controlApi/accountRecoveryTests.rs"]
mod tests;
