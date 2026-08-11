//! 提供反向 HTTP 代理和字节级 TCP 端口转发监听器。

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use capture_core::RecordingSession;
use hyper::{Request, body::Incoming, service::service_fn};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    DnsSpoofingConfiguration, DnsSpoofingTool, HttpProxyConfig, SslMitmManager, ToolPipeline,
    forwarder::{ProxyContext, ReverseRequestTarget, forwardReverseRequest},
    server::buildHttpUpstreamClient,
    taskTracker::ProxyTaskTracker,
    upstreamClient::HttpsUpstreamClients,
};

const maximumListenerEntries: usize = 128;
const maximumIdentifierLength: usize = 64;
const maximumHostLength: usize = 253;
const defaultShutdownTimeout: Duration = Duration::from_secs(5);

/// 定义反向代理允许的上游协议；未知协议在配置阶段拒绝而不是运行时回退。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReverseProxyScheme {
    Http,
    Https,
}

impl ReverseProxyScheme {
    /// 返回出站 URI 使用的稳定协议文本。
    const fn asStr(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// 描述单条本地 HTTP 监听到固定远端 HTTP(S) 上游的映射规则。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReverseProxyEntry {
    pub id: String,
    pub enabled: bool,
    pub listenHost: String,
    pub listenPort: u16,
    pub remoteHost: String,
    pub remotePort: u16,
    pub remoteScheme: ReverseProxyScheme,
    pub preserveHostHeader: bool,
    pub stripPathPrefix: String,
}

impl ReverseProxyEntry {
    /// 解析本机绑定地址；监听地址只接受 IP，避免 DNS 解析在服务启动时阻塞。
    pub fn listenAddress(&self) -> Result<SocketAddr, AuxiliaryListenerError> {
        let host = self
            .listenHost
            .parse::<IpAddr>()
            .map_err(|_| AuxiliaryListenerError::InvalidListenHost)?;
        if self.listenPort == 0 {
            return Err(AuxiliaryListenerError::InvalidListenPort);
        }
        Ok(SocketAddr::new(host, self.listenPort))
    }

    /// 校验条目静态边界；远端主机保持域名语义，实际 DNS 解析仍交由 Hyper 出站连接器完成。
    pub fn validate(&self) -> Result<(), AuxiliaryListenerError> {
        validateListenerIdentifier(&self.id)?;
        self.listenAddress()?;
        validateRemoteHost(&self.remoteHost)?;
        if self.remotePort == 0 {
            return Err(AuxiliaryListenerError::InvalidRemotePort);
        }
        if !self.stripPathPrefix.is_empty()
            && (!self.stripPathPrefix.starts_with('/')
                || self
                    .stripPathPrefix
                    .split('/')
                    .any(|segment| segment == ".."))
        {
            return Err(AuxiliaryListenerError::InvalidPathPrefix);
        }
        Ok(())
    }
}

/// 描述单条本地 TCP 监听到固定目标的透明字节转发规则。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortForwardEntry {
    pub id: String,
    pub enabled: bool,
    pub listenHost: String,
    pub listenPort: u16,
    pub targetHost: String,
    pub targetPort: u16,
}

impl PortForwardEntry {
    /// 解析本机绑定地址；端口零不属于产品配置，测试需显式预留可用端口。
    pub fn listenAddress(&self) -> Result<SocketAddr, AuxiliaryListenerError> {
        let host = self
            .listenHost
            .parse::<IpAddr>()
            .map_err(|_| AuxiliaryListenerError::InvalidListenHost)?;
        if self.listenPort == 0 {
            return Err(AuxiliaryListenerError::InvalidListenPort);
        }
        Ok(SocketAddr::new(host, self.listenPort))
    }

    /// 校验条目字段；目标主机允许 DNS 名称或 IP，但空白和过长输入立即拒绝。
    pub fn validate(&self) -> Result<(), AuxiliaryListenerError> {
        validateListenerIdentifier(&self.id)?;
        self.listenAddress()?;
        validateRemoteHost(&self.targetHost)?;
        if self.targetPort == 0 {
            return Err(AuxiliaryListenerError::InvalidRemotePort);
        }
        Ok(())
    }
}

/// 聚合两类辅助监听规则；更新时使用整体替换保证冲突检测覆盖跨集合端口。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AuxiliaryListenerConfiguration {
    pub reverseProxies: Vec<ReverseProxyEntry>,
    pub portForwards: Vec<PortForwardEntry>,
}

impl AuxiliaryListenerConfiguration {
    /// 校验各类规则和它们之间的本机端口冲突；禁用条目不占用端口也不参与冲突集合。
    pub fn validate(&self) -> Result<(), AuxiliaryListenerError> {
        if self.reverseProxies.len() > maximumListenerEntries
            || self.portForwards.len() > maximumListenerEntries
        {
            return Err(AuxiliaryListenerError::TooManyEntries);
        }
        let mut reverseIdentifiers = std::collections::BTreeSet::new();
        let mut forwardIdentifiers = std::collections::BTreeSet::new();
        let mut addresses = Vec::new();
        for entry in &self.reverseProxies {
            entry.validate()?;
            if !reverseIdentifiers.insert(entry.id.clone()) {
                return Err(AuxiliaryListenerError::DuplicateIdentifier);
            }
            if entry.enabled {
                addresses.push(entry.listenAddress()?);
            }
        }
        for entry in &self.portForwards {
            entry.validate()?;
            if !forwardIdentifiers.insert(entry.id.clone()) {
                return Err(AuxiliaryListenerError::DuplicateIdentifier);
            }
            if entry.enabled {
                addresses.push(entry.listenAddress()?);
            }
        }
        for (index, address) in addresses.iter().enumerate() {
            if addresses
                .iter()
                .skip(index + 1)
                .any(|other| listenerAddressesConflict(*address, *other))
            {
                return Err(AuxiliaryListenerError::ListenerConflict);
            }
        }
        Ok(())
    }
}

/// 聚合正在运行的辅助监听器；停止返回后所有绑定端口和连接任务均已释放。
pub struct RunningAuxiliaryListeners {
    cancellation: CancellationToken,
    tasks: Vec<JoinHandle<Result<(), AuxiliaryListenerError>>>,
    boundEndpoints: Arc<RwLock<AuxiliaryListenerBindings>>,
}

/// 保存条目 ID 到实际绑定端点的只读视图；绑定端口由操作系统决定，不能从配置猜测。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuxiliaryListenerBindings {
    pub reverseProxies: Vec<ListenerBinding>,
    pub portForwards: Vec<ListenerBinding>,
}

/// 描述一个成功运行条目的稳定 ID 和实际端点。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListenerBinding {
    pub id: String,
    pub boundEndpoint: String,
}

impl RunningAuxiliaryListeners {
    /// 返回成功绑定条目的稳定快照；失败条目由控制层配置校验阻止，不生成半运行状态。
    pub fn bindings(&self) -> AuxiliaryListenerBindings {
        self.boundEndpoints.read().clone()
    }

    /// 关闭所有辅助监听器并强制中止其客户端与上游套接字；不等待连接优雅排空。
    pub async fn stop(mut self) -> Result<(), AuxiliaryListenerError> {
        self.cancellation.cancel();
        let mut firstError = None;
        for task in self.tasks.drain(..) {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    firstError.get_or_insert(error);
                }
                Err(_) => {
                    firstError.get_or_insert(AuxiliaryListenerError::RuntimeJoinFailed);
                }
            };
        }
        firstError.map_or(Ok(()), Err)
    }
}

impl Drop for RunningAuxiliaryListeners {
    /// 遗忘生命周期句柄时请求取消；后台任务会中止连接而不是等待对端结束。
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// 启动启用的反向代理和端口转发条目；配置校验先于任何 bind，因此失败不会留下部分监听器。
pub async fn startAuxiliaryListeners(
    configuration: AuxiliaryListenerConfiguration,
    httpConfiguration: HttpProxyConfig,
    recording: RecordingSession,
    ssl: SslMitmManager,
    pipeline: ToolPipeline,
    externalShutdown: CancellationToken,
) -> Result<RunningAuxiliaryListeners, AuxiliaryListenerError> {
    configuration.validate()?;
    let cancellation = CancellationToken::new();
    let bindings = Arc::new(RwLock::new(AuxiliaryListenerBindings::default()));
    let mut tasks = Vec::new();
    for entry in configuration
        .reverseProxies
        .into_iter()
        .filter(|entry| entry.enabled)
    {
        let listener = match TcpListener::bind(entry.listenAddress()?).await {
            Ok(listener) => listener,
            Err(_) => {
                return rollbackAuxiliaryStartup(
                    &cancellation,
                    tasks,
                    AuxiliaryListenerError::BindFailed,
                )
                .await;
            }
        };
        let boundEndpoint = match listener.local_addr() {
            Ok(boundEndpoint) => boundEndpoint,
            Err(_) => {
                return rollbackAuxiliaryStartup(
                    &cancellation,
                    tasks,
                    AuxiliaryListenerError::BindFailed,
                )
                .await;
            }
        };
        bindings.write().reverseProxies.push(ListenerBinding {
            id: entry.id.clone(),
            boundEndpoint: boundEndpoint.to_string(),
        });
        let runtime = match ReverseProxyRuntime::new(
            httpConfiguration.clone(),
            recording.clone(),
            ssl.clone(),
            pipeline.clone(),
        ) {
            Ok(runtime) => runtime,
            Err(error) => return rollbackAuxiliaryStartup(&cancellation, tasks, error).await,
        };
        tasks.push(tokio::spawn(runReverseProxy(
            listener,
            entry,
            runtime,
            cancellation.clone(),
            externalShutdown.clone(),
        )));
    }
    for entry in configuration
        .portForwards
        .into_iter()
        .filter(|entry| entry.enabled)
    {
        let listener = match TcpListener::bind(entry.listenAddress()?).await {
            Ok(listener) => listener,
            Err(_) => {
                return rollbackAuxiliaryStartup(
                    &cancellation,
                    tasks,
                    AuxiliaryListenerError::BindFailed,
                )
                .await;
            }
        };
        let boundEndpoint = match listener.local_addr() {
            Ok(boundEndpoint) => boundEndpoint,
            Err(_) => {
                return rollbackAuxiliaryStartup(
                    &cancellation,
                    tasks,
                    AuxiliaryListenerError::BindFailed,
                )
                .await;
            }
        };
        bindings.write().portForwards.push(ListenerBinding {
            id: entry.id.clone(),
            boundEndpoint: boundEndpoint.to_string(),
        });
        tasks.push(tokio::spawn(runPortForward(
            listener,
            entry,
            cancellation.clone(),
            externalShutdown.clone(),
        )));
    }
    Ok(RunningAuxiliaryListeners {
        cancellation,
        tasks,
        boundEndpoints: bindings,
    })
}

/// 启动中途绑定失败时等待已创建任务退出；避免返回错误后端口仍被暂态任务占用。
async fn awaitAuxiliaryStartupRollback(tasks: Vec<JoinHandle<Result<(), AuxiliaryListenerError>>>) {
    for task in tasks {
        let _ = timeout(defaultShutdownTimeout, task).await;
    }
}

/// 回滚部分成功的启动过程；任何条目绑定失败后都等待先前监听器释放端口，再把原始失败原因交还控制层。
async fn rollbackAuxiliaryStartup(
    cancellation: &CancellationToken,
    tasks: Vec<JoinHandle<Result<(), AuxiliaryListenerError>>>,
    error: AuxiliaryListenerError,
) -> Result<RunningAuxiliaryListeners, AuxiliaryListenerError> {
    cancellation.cancel();
    awaitAuxiliaryStartupRollback(tasks).await;
    Err(error)
}

/// 为一个反向代理监听器构造长期依赖；所有条目共享配置限制但各自拥有独立上游连接池。
struct ReverseProxyRuntime {
    config: HttpProxyConfig,
    capture: RecordingSession,
    ssl: SslMitmManager,
    pipeline: ToolPipeline,
}

impl ReverseProxyRuntime {
    /// 创建可供单条反向代理使用的运行时；TLS 配置错误在条目启动前失败而不是首个请求时失败。
    fn new(
        config: HttpProxyConfig,
        capture: RecordingSession,
        ssl: SslMitmManager,
        pipeline: ToolPipeline,
    ) -> Result<Self, AuxiliaryListenerError> {
        config
            .validate()
            .map_err(|_| AuxiliaryListenerError::InvalidHttpConfiguration)?;
        ssl.upstreamClientConfiguration(None)
            .map_err(|_| AuxiliaryListenerError::InvalidHttpConfiguration)?;
        Ok(Self {
            config,
            capture,
            ssl,
            pipeline,
        })
    }
}

/// 运行反向代理 accept 循环并在停止时中止已接纳连接；每条连接都有同一并发上限保护。
async fn runReverseProxy(
    listener: TcpListener,
    entry: ReverseProxyEntry,
    runtime: ReverseProxyRuntime,
    cancellation: CancellationToken,
    externalShutdown: CancellationToken,
) -> Result<(), AuxiliaryListenerError> {
    let tasks = ProxyTaskTracker::new();
    let slots = Arc::new(Semaphore::new(runtime.config.maxConnections));
    let dnsSpoofing = Arc::new(
        DnsSpoofingTool::new(DnsSpoofingConfiguration::default()).expect("默认 DNS 配置必须有效"),
    );
    let httpClient = buildHttpUpstreamClient(&runtime.config, dnsSpoofing.clone());
    let outbound = crate::server::buildOutboundConnector(&runtime.config);
    let httpsClients =
        HttpsUpstreamClients::new(runtime.ssl.clone(), dnsSpoofing.clone(), outbound.clone());
    let context = ProxyContext {
        config: runtime.config.clone(),
        capture: runtime.capture,
        httpClient,
        httpsClients,
        ssl: runtime.ssl,
        pipeline: runtime.pipeline,
        pluginHost: plugin_host::PluginHost::disabled(),
        dnsSpoofing,
        outbound,
        cancellation: cancellation.clone(),
        tasks: tasks.clone(),
        metrics: crate::HttpRuntimeMetrics::default(),
        clientProcessName: None,
        clientProcessId: None,
    };
    loop {
        let permit = tokio::select! {
            () = cancellation.cancelled() => break,
            () = externalShutdown.cancelled() => break,
            permit = slots.clone().acquire_owned() => permit.map_err(|_| AuxiliaryListenerError::RuntimeJoinFailed)?,
        };
        let accepted = tokio::select! {
            () = cancellation.cancelled() => break,
            () = externalShutdown.cancelled() => break,
            result = listener.accept() => result.map_err(|_| AuxiliaryListenerError::AcceptFailed)?,
        };
        let connectionContext = context.clone();
        let requestTarget = ReverseRequestTarget {
            scheme: entry.remoteScheme.asStr(),
            host: entry.remoteHost.clone(),
            port: entry.remotePort,
            preserveHostHeader: entry.preserveHostHeader,
            stripPathPrefix: entry.stripPathPrefix.clone(),
        };
        tasks.spawn(async move {
            serveReverseConnection(
                accepted.0,
                accepted.1,
                connectionContext,
                requestTarget,
                permit,
            )
            .await;
        });
    }
    cancellation.cancel();
    tasks.abortAllAndWait().await;
    Ok(())
}

/// 为单条反向代理连接运行 HTTP/1.1 服务；请求处理始终委托到共享转发器以保留事务和工具语义。
async fn serveReverseConnection(
    stream: TcpStream,
    clientAddress: SocketAddr,
    context: ProxyContext,
    target: ReverseRequestTarget,
    permit: OwnedSemaphorePermit,
) {
    let requestContext = context.clone();
    let service = service_fn(move |request: Request<Incoming>| {
        let context = requestContext.clone();
        let target = target.clone();
        async move {
            Ok::<_, std::convert::Infallible>(
                forwardReverseRequest(request, clientAddress, context, target).await,
            )
        }
    });
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .keep_alive(true)
        .max_buf_size(context.config.maxHeaderBytes)
        .timer(TokioTimer::new())
        .header_read_timeout(context.config.headerReadTimeout());
    let connection = builder.serve_connection(TokioIo::new(stream), service);
    tokio::pin!(connection);
    tokio::select! {
        _ = &mut connection => {}
        () = context.cancellation.cancelled() => {
            connection.as_mut().graceful_shutdown();
            let _ = timeout(context.config.connectionDrainTimeout(), &mut connection).await;
        }
    }
    drop(permit);
}

/// 接受 TCP 连接、建立目标连接并执行双向字节复制；该路径不解析任何应用层数据。
async fn runPortForward(
    listener: TcpListener,
    entry: PortForwardEntry,
    cancellation: CancellationToken,
    externalShutdown: CancellationToken,
) -> Result<(), AuxiliaryListenerError> {
    let tasks = ProxyTaskTracker::new();
    loop {
        let accepted = tokio::select! {
            () = cancellation.cancelled() => break,
            () = externalShutdown.cancelled() => break,
            result = listener.accept() => result.map_err(|_| AuxiliaryListenerError::AcceptFailed)?,
        };
        let targetHost = entry.targetHost.clone();
        let targetPort = entry.targetPort;
        let connectionCancellation = cancellation.clone();
        tasks.spawn(async move {
            forwardTcpConnection(accepted.0, targetHost, targetPort, connectionCancellation).await;
        });
    }
    cancellation.cancel();
    tasks.abortAllAndWait().await;
    Ok(())
}

/// 在停止请求、目标建连与复制之间建立优先级；取消时立即释放两端连接而不保留半开流。
async fn forwardTcpConnection(
    mut client: TcpStream,
    targetHost: String,
    targetPort: u16,
    cancellation: CancellationToken,
) {
    let upstream = tokio::select! {
        () = cancellation.cancelled() => return,
        result = TcpStream::connect((targetHost.as_str(), targetPort)) => result,
    };
    let Ok(mut upstream) = upstream else {
        return;
    };
    let _ = tokio::select! {
        () = cancellation.cancelled() => Ok((0_u64, 0_u64)),
        result = copy_bidirectional(&mut client, &mut upstream) => result,
    };
}

/// 校验条目 ID 为简洁稳定键；该 ID 仅用于控制 API 和 UI 的绑定状态映射。
fn validateListenerIdentifier(identifier: &str) -> Result<(), AuxiliaryListenerError> {
    if identifier.trim().is_empty() || identifier.len() > maximumIdentifierLength {
        return Err(AuxiliaryListenerError::InvalidIdentifier);
    }
    Ok(())
}

/// 校验远端主机的基础边界；完整 DNS 语义在每次连接时由系统解析器处理。
fn validateRemoteHost(host: &str) -> Result<(), AuxiliaryListenerError> {
    if host.trim().is_empty()
        || host.len() > maximumHostLength
        || host.chars().any(char::is_whitespace)
    {
        return Err(AuxiliaryListenerError::InvalidRemoteHost);
    }
    Ok(())
}

/// 判断两个本机监听是否覆盖同一 socket；同端口的 unspecified 地址与任何具体地址冲突。
fn listenerAddressesConflict(left: SocketAddr, right: SocketAddr) -> bool {
    left.port() == right.port()
        && (left.ip() == right.ip() || left.ip().is_unspecified() || right.ip().is_unspecified())
}

/// 描述辅助监听配置、绑定和生命周期中的稳定失败，不向控制面泄露网络路径或系统 I/O 文本。
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AuxiliaryListenerError {
    #[error("error.listeners.tooManyEntries")]
    TooManyEntries,
    #[error("error.listeners.invalidIdentifier")]
    InvalidIdentifier,
    #[error("error.listeners.duplicateIdentifier")]
    DuplicateIdentifier,
    #[error("error.listeners.invalidListenHost")]
    InvalidListenHost,
    #[error("error.listeners.invalidListenPort")]
    InvalidListenPort,
    #[error("error.listeners.invalidRemoteHost")]
    InvalidRemoteHost,
    #[error("error.listeners.invalidRemotePort")]
    InvalidRemotePort,
    #[error("error.listeners.invalidPathPrefix")]
    InvalidPathPrefix,
    #[error("error.listeners.listenerConflict")]
    ListenerConflict,
    #[error("error.listeners.invalidHttpConfiguration")]
    InvalidHttpConfiguration,
    #[error("error.listeners.bindFailed")]
    BindFailed,
    #[error("error.listeners.acceptFailed")]
    AcceptFailed,
    #[error("error.listeners.runtimeJoinFailed")]
    RuntimeJoinFailed,
    #[error("error.listeners.shutdownTimeout")]
    ShutdownTimeout,
}

impl AuxiliaryListenerError {
    /// 返回控制层、MCP 与测试共用的稳定机器错误码。
    pub const fn code(self) -> &'static str {
        match self {
            Self::TooManyEntries => "listenerTooManyEntries",
            Self::InvalidIdentifier => "listenerInvalidIdentifier",
            Self::DuplicateIdentifier => "listenerDuplicateIdentifier",
            Self::InvalidListenHost => "listenerInvalidListenHost",
            Self::InvalidListenPort => "listenerInvalidListenPort",
            Self::InvalidRemoteHost => "listenerInvalidRemoteHost",
            Self::InvalidRemotePort => "listenerInvalidRemotePort",
            Self::InvalidPathPrefix => "listenerInvalidPathPrefix",
            Self::ListenerConflict => "listenerConfigurationConflict",
            Self::InvalidHttpConfiguration => "listenerInvalidHttpConfiguration",
            Self::BindFailed => "listenerBindFailed",
            Self::AcceptFailed => "listenerAcceptFailed",
            Self::RuntimeJoinFailed => "listenerRuntimeJoinFailed",
            Self::ShutdownTimeout => "listenerShutdownTimeout",
        }
    }
}
