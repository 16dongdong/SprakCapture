use std::{net::SocketAddr, sync::Arc};

use capture_core::RecordingSession;
use hyper::{Request, body::Incoming, service::service_fn};
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
};
use plugin_host::PluginHost;
use socks5_core::{ServiceMetrics, interception::PortProtocolHandler};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    DnsSpoofingConfiguration, DnsSpoofingTool, HttpProxyConfig, HttpProxyError, SslMitmManager,
    ToolPipeline,
    connector::ProxyConnector,
    forwarder::{HttpUpstreamClient, ProxyContext, forwardRequest},
    runtimeMetrics::{HttpMetricsStream, HttpRuntimeMetrics},
    taskTracker::ProxyTaskTracker,
    upstreamClient::HttpsUpstreamClients,
};

/// 聚合拥有者停止与外部守护停止信号；任一触发都会进入同一有序关闭路径。
struct ShutdownSignals {
    internal: CancellationToken,
    external: CancellationToken,
}

/// 汇总监听循环建立数据面上下文所需的长期依赖，避免参数扩张后破坏启动路径的可读性与原子所有权转移。
struct ServerRuntime {
    config: HttpProxyConfig,
    capture: RecordingSession,
    ssl: SslMitmManager,
    pipeline: ToolPipeline,
    pluginHost: PluginHost,
    httpClient: HttpUpstreamClient,
    httpsClients: HttpsUpstreamClients,
    dnsSpoofing: Arc<DnsSpoofingTool>,
    outbound: transport_core::OutboundConnector,
    metrics: HttpRuntimeMetrics,
}

/// 在融合监听器中运行 HTTP 状态机；端口、并发额度和连接任务由 SOCKS5 核心统一拥有。
#[derive(Clone)]
pub struct HttpConnectionHandler {
    context: ProxyContext,
    metrics: HttpRuntimeMetrics,
}

impl PortProtocolHandler for HttpConnectionHandler {
    /// 接管已分类为 HTTP 的原始连接；首字节仍留在套接字中，由 Hyper 完整解析。
    fn serve(
        &self,
        stream: TcpStream,
        clientAddress: SocketAddr,
        cancellation: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let mut context = self.context.clone();
        let metrics = self.metrics.clone();
        context.cancellation = cancellation;
        Box::pin(async move {
            serveConnection(HttpConnection {
                stream,
                clientAddress,
                context,
                permit: None,
                metrics,
            })
            .await;
        })
    }

    /// 关闭任务追踪入口并等待 CONNECT、TLS 与响应泵全部退出。
    ///
    /// 运行上下文：SOCKS5 融合监听已停止接收并排空外层连接后调用；超时由外层统一控制。
    fn shutdown(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>> {
        let tasks = self.context.tasks.clone();
        Box::pin(async move {
            tasks.close();
            tasks.wait().await;
            Ok(())
        })
    }

    /// 中止优雅排空预算内未退出的 HTTP 派生任务并等待析构，避免监听器已报告停止但连接仍在后台运行。
    fn abortAndWait(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let tasks = self.context.tasks.clone();
        Box::pin(async move {
            tasks.abortAllAndWait().await;
        })
    }
}

/// 聚合 HTTP 代理与 SOCKS5 应用层接管器共享的运行依赖，避免新增工具持续扩张构造函数参数。
#[derive(Clone)]
pub struct HttpProxyDependencies {
    pub capture: RecordingSession,
    pub ssl: SslMitmManager,
    pub pipeline: ToolPipeline,
    pub pluginHost: PluginHost,
    pub dnsSpoofing: Arc<DnsSpoofingTool>,
}

/// 持有已绑定 HTTP 代理的唯一生命周期；stop 返回后监听端口和受跟踪连接均已释放。
pub struct RunningHttpProxy {
    boundAddress: SocketAddr,
    cancellation: CancellationToken,
    serverTask: Option<JoinHandle<Result<(), HttpProxyError>>>,
    exitReceiver: watch::Receiver<Option<HttpProxyExit>>,
    metrics: HttpRuntimeMetrics,
}

/// 描述 HTTP 接受循环的意外终止；控制层按稳定 code/messageKey 更新监听状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpProxyExit {
    pub code: String,
    pub messageKey: String,
}

impl RunningHttpProxy {
    /// 返回操作系统确认的监听地址；配置端口为零时这里包含实际分配端口。
    pub const fn boundAddress(&self) -> SocketAddr {
        self.boundAddress
    }

    /// 订阅接受循环意外终止；正常 stop 由生命周期拥有者撤销监控，不发布故障。
    pub fn subscribeExit(&self) -> watch::Receiver<Option<HttpProxyExit>> {
        self.exitReceiver.clone()
    }

    /// 返回公开 HTTP 监听器的真实套接字指标；读操作仅复制原子值，不阻塞连接任务。
    pub fn metrics(&self) -> ServiceMetrics {
        self.metrics.snapshot()
    }

    /// 订阅 HTTP 连接、流量与终态变化；事件只用于唤醒，调用方应重新读取 `metrics`。
    pub fn subscribeMetricChanges(&self) -> watch::Receiver<u64> {
        self.metrics.subscribeChanges()
    }

    /// 停止监听并强制中止全部 HTTP 连接、响应泵和 CONNECT 隧道。
    ///
    /// 运行上下文：用户已经选择停止或重启，客户端负责重新建连；这里不执行 keep-alive、SSE
    /// 或隧道的优雅排空。返回前只等待已中止 future 完成析构，失败仅表示服务任务本身异常。
    pub async fn stop(mut self) -> Result<(), HttpProxyError> {
        self.cancellation.cancel();
        let Some(serverTask) = self.serverTask.take() else {
            return Ok(());
        };
        match serverTask.await {
            Ok(result) => result,
            Err(_) => Err(HttpProxyError::RuntimeJoinFailed),
        }
    }
}

impl Drop for RunningHttpProxy {
    /// 遗忘显式 stop 时只发出取消信号；后台任务继续完成有序资源回收而不被强制中断。
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// 校验配置并绑定监听端口；成功返回即保证代理已可接受连接。
pub async fn startHttpProxy(
    config: HttpProxyConfig,
    capture: RecordingSession,
    ssl: SslMitmManager,
    pipeline: ToolPipeline,
    externalShutdown: CancellationToken,
) -> Result<RunningHttpProxy, HttpProxyError> {
    startHttpProxyWithPlugins(
        config,
        capture,
        ssl,
        pipeline,
        PluginHost::disabled(),
        externalShutdown,
    )
    .await
}

/// 校验配置、绑定监听器并注入统一插件宿主；旧入口保持无插件行为以兼容独立库调用方。
pub async fn startHttpProxyWithPlugins(
    config: HttpProxyConfig,
    capture: RecordingSession,
    ssl: SslMitmManager,
    pipeline: ToolPipeline,
    pluginHost: PluginHost,
    externalShutdown: CancellationToken,
) -> Result<RunningHttpProxy, HttpProxyError> {
    let dnsSpoofing = Arc::new(
        DnsSpoofingTool::new(DnsSpoofingConfiguration::default()).expect("默认 DNS 配置必须有效"),
    );
    startHttpProxyWithPluginsAndDns(
        config,
        HttpProxyDependencies {
            capture,
            ssl,
            pipeline,
            pluginHost,
            dnsSpoofing,
        },
        externalShutdown,
    )
    .await
}

/// 启动注入共享 DNS 映射器的 HTTP 代理；控制服务用该入口让配置热更新立即作用于新连接。
///
/// 运行上下文：连接池、CONNECT 路径和 SOCKS5 HTTP 接管器必须持有同一工具实例。
/// 失败语义：监听或 TLS 客户端初始化失败时不启动后台任务，调用方仍持有原配置对象。
pub async fn startHttpProxyWithPluginsAndDns(
    mut config: HttpProxyConfig,
    dependencies: HttpProxyDependencies,
    externalShutdown: CancellationToken,
) -> Result<RunningHttpProxy, HttpProxyError> {
    let HttpProxyDependencies {
        capture,
        ssl,
        pipeline,
        pluginHost,
        dnsSpoofing,
    } = dependencies;
    config.validate()?;
    let outbound = buildOutboundConnector(&config);
    let httpClient = buildHttpUpstreamClient(&config, dnsSpoofing.clone());
    ssl.upstreamClientConfiguration(None)
        .map_err(|_| HttpProxyError::TlsConfigurationFailed)?;
    let httpsClients =
        HttpsUpstreamClients::new(ssl.clone(), dnsSpoofing.clone(), outbound.clone());
    let listener = TcpListener::bind(config.listenAddress())
        .await
        .map_err(HttpProxyError::bind)?;
    let boundAddress = listener.local_addr().map_err(HttpProxyError::bind)?;
    // 端口为 0 时监听器会分配真实端口；运行上下文必须保存该端口，后续自环检测才能识别实际入口。
    config.listenPort = boundAddress.port();
    let cancellation = CancellationToken::new();
    let metrics = HttpRuntimeMetrics::default();
    let serverMetrics = metrics.clone();
    let serverCancellation = cancellation.clone();
    let (exitSender, exitReceiver) = watch::channel(None);
    let serverTask = tokio::spawn(async move {
        let result = runServer(
            listener,
            ServerRuntime {
                config,
                capture,
                ssl,
                pipeline,
                pluginHost,
                httpClient,
                httpsClients,
                dnsSpoofing,
                outbound,
                metrics: serverMetrics,
            },
            ShutdownSignals {
                internal: serverCancellation,
                external: externalShutdown,
            },
        )
        .await;
        if let Err(error) = result.as_ref() {
            exitSender.send_replace(Some(HttpProxyExit {
                code: error.code().to_owned(),
                messageKey: error.messageKey().to_owned(),
            }));
        }
        result
    });
    Ok(RunningHttpProxy {
        boundAddress,
        cancellation,
        serverTask: Some(serverTask),
        exitReceiver,
        metrics,
    })
}

/// 运行 accept 循环并在任一取消源触发后强制中止所有受跟踪任务。
async fn runServer(
    listener: TcpListener,
    runtime: ServerRuntime,
    shutdown: ShutdownSignals,
) -> Result<(), HttpProxyError> {
    let tasks = ProxyTaskTracker::new();
    let connectionSlots = Arc::new(Semaphore::new(runtime.config.maxConnections));
    let context = ProxyContext {
        config: runtime.config,
        capture: runtime.capture,
        httpClient: runtime.httpClient,
        httpsClients: runtime.httpsClients,
        ssl: runtime.ssl,
        pipeline: runtime.pipeline,
        pluginHost: runtime.pluginHost,
        dnsSpoofing: runtime.dnsSpoofing,
        outbound: runtime.outbound,
        metrics: runtime.metrics,
        cancellation: shutdown.internal.clone(),
        tasks: tasks.clone(),
        clientProcessName: None,
        clientProcessId: None,
    };
    let acceptResult =
        acceptConnections(listener, context, connectionSlots, shutdown.external).await;
    shutdown.internal.cancel();
    // 停止是破坏性生命周期边界：直接丢弃所有套接字所有者，禁止长响应、SSE 或 CONNECT
    // 把服务关闭拖到配置中的优雅排空超时后再误报故障。
    tasks.abortAllAndWait().await;
    acceptResult
}

/// 接受受并发上限保护的客户端连接；监听错误会结束服务并交由生命周期拥有者处理。
async fn acceptConnections(
    listener: TcpListener,
    context: ProxyContext,
    connectionSlots: Arc<Semaphore>,
    externalShutdown: CancellationToken,
) -> Result<(), HttpProxyError> {
    loop {
        // 先取得容量再 accept，保证内核已接收但尚未纳管的连接不会超过 maxConnections。
        let permit = tokio::select! {
            () = context.cancellation.cancelled() => return Ok(()),
            () = externalShutdown.cancelled() => return Ok(()),
            permit = connectionSlots.clone().acquire_owned() => {
                permit.map_err(|_| HttpProxyError::RuntimeJoinFailed)?
            }
        };
        let accepted = tokio::select! {
            () = context.cancellation.cancelled() => return Ok(()),
            () = externalShutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => accepted.map_err(HttpProxyError::accept)?,
        };
        let connectionContext = context.clone();
        context.tasks.spawn(async move {
            let metrics = connectionContext.metrics.clone();
            serveConnection(HttpConnection {
                stream: accepted.0,
                clientAddress: accepted.1,
                context: connectionContext,
                permit: Some(permit),
                metrics,
            })
            .await;
        });
    }
}

/// 聚合单条 HTTP 客户端连接的套接字、地址、代理上下文、并发许可与指标账本。
struct HttpConnection {
    stream: TcpStream,
    clientAddress: SocketAddr,
    context: ProxyContext,
    permit: Option<OwnedSemaphorePermit>,
    metrics: HttpRuntimeMetrics,
}

/// 为单个 TCP 客户端运行支持 keep-alive 与升级的 HTTP/1.1 状态机，并在真实套接字边界计量流量。
async fn serveConnection(connection: HttpConnection) {
    let HttpConnection {
        stream,
        clientAddress,
        context,
        permit,
        metrics,
    } = connection;
    let serviceContext = context.clone();
    let service = service_fn(move |request: Request<Incoming>| {
        forwardRequest(request, clientAddress, serviceContext.clone())
    });
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .keep_alive(true)
        .max_buf_size(context.config.maxHeaderBytes)
        .timer(TokioTimer::new())
        .header_read_timeout(context.config.headerReadTimeout());
    let (measuredStream, failureMarker) = HttpMetricsStream::new(stream, metrics);
    let connection = builder.serve_connection_with_upgrades(TokioIo::new(measuredStream), service);
    tokio::pin!(connection);
    tokio::select! {
        result = &mut connection => {
            if result.is_err() {
                failureMarker.markFailed();
                tracing::debug!(
                    errorCode = "httpProxyClientProtocolClosed",
                    messageKey = "error.httpProxy.clientDisconnected"
                );
            }
        }
        () = context.cancellation.cancelled() => {
            // 取消分支结束后立即析构 Hyper 连接及底层套接字；客户端自行重连。
        }
    }
    drop(permit);
}

/// 构建共享连接池；HttpConnector 仅接受明文 HTTP，CONNECT 由独立 TCP 路径处理。
pub(crate) fn buildHttpUpstreamClient(
    config: &HttpProxyConfig,
    dnsSpoofing: Arc<DnsSpoofingTool>,
) -> HttpUpstreamClient {
    let connector = ProxyConnector::new(buildOutboundConnector(config), dnsSpoofing);
    Client::builder(TokioExecutor::new()).build(connector)
}

/// 构建对单一透明目标固定原始 IP 的 HTTP 连接池，Host 与录制位置仍保留逻辑域名。
///
/// 运行上下文：透明 HTTP 接管会丢弃分类前的探针连接并由 Hyper 重建上游，此连接器防止二级代理再次解析到其他地址。
/// 失败语义：连接失败仍由 Hyper 客户端错误返回；固定条件只匹配完整主机和端口。
pub(crate) fn buildHttpUpstreamClientWithFixedTarget(
    config: &HttpProxyConfig,
    dnsSpoofing: Arc<DnsSpoofingTool>,
    fixedTarget: crate::connector::FixedConnectTarget,
) -> HttpUpstreamClient {
    let connector = ProxyConnector::newWithFixedTarget(
        buildOutboundConnector(config),
        dnsSpoofing,
        fixedTarget,
    );
    Client::builder(TokioExecutor::new()).build(connector)
}

/// 从已验证 HTTP 配置构造共享出站策略；启动边界在调用本函数前必须完成 validate。
pub(crate) fn buildOutboundConnector(
    config: &HttpProxyConfig,
) -> transport_core::OutboundConnector {
    transport_core::OutboundConnector::new(config.upstreamProxy.clone(), config.connectTimeout())
        .expect("HTTP 配置已在构建出站连接器前完成校验")
}

/// 构建不绑定端口的 HTTP 连接处理器，供融合监听器复用代理、录制和 TLS 能力。
///
/// 运行上下文：每条连接会用融合服务器的取消令牌覆盖构造令牌，保证统一停止。
/// 失败语义：配置或 TLS 初始化失败时返回错误，不创建后台任务。
pub fn buildHttpConnectionHandler(
    config: HttpProxyConfig,
    dependencies: HttpProxyDependencies,
) -> Result<HttpConnectionHandler, HttpProxyError> {
    config.validate()?;
    let HttpProxyDependencies {
        capture,
        ssl,
        pipeline,
        pluginHost,
        dnsSpoofing,
    } = dependencies;
    ssl.upstreamClientConfiguration(None)
        .map_err(|_| HttpProxyError::TlsConfigurationFailed)?;
    let outbound = buildOutboundConnector(&config);
    let httpClient = buildHttpUpstreamClient(&config, dnsSpoofing.clone());
    let httpsClients =
        HttpsUpstreamClients::new(ssl.clone(), dnsSpoofing.clone(), outbound.clone());
    let metrics = HttpRuntimeMetrics::default();
    Ok(HttpConnectionHandler {
        context: ProxyContext {
            config,
            capture,
            httpClient,
            httpsClients,
            ssl,
            pipeline,
            pluginHost,
            dnsSpoofing,
            outbound,
            cancellation: CancellationToken::new(),
            tasks: ProxyTaskTracker::new(),
            metrics: metrics.clone(),
            clientProcessName: None,
            clientProcessId: None,
        },
        metrics,
    })
}

impl HttpConnectionHandler {
    /// 返回当前融合监听服务周期共享的 HTTP 指标账本；克隆只增加引用计数，不复制累计值。
    pub fn runtimeMetrics(&self) -> HttpRuntimeMetrics {
        self.metrics.clone()
    }
}
