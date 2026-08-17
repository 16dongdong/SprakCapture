use std::{
    convert::Infallible,
    io,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use capture_core::{
    BeginTransaction, RecordingSession, TransactionProtocol, currentTimeMilliseconds,
};
use http::Method;
use hyper::{Request, body::Incoming, service::service_fn};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
};
use location_core::ResolvedLocation;
use plugin_host::PluginHost;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::{
    DnsSpoofingConfiguration, DnsSpoofingTool, HttpProxyConfig, HttpProxyDependencies,
    HttpProxyError, SslMitmManager, ToolPipeline,
    captureBridge::CaptureTransaction,
    connector::FixedConnectTarget,
    error::RequestFailure,
    forwarder::{ProxyContext, UpstreamTransport, failureResponse, forwardDecodedHttp},
    server::{buildHttpUpstreamClient, buildHttpUpstreamClientWithFixedTarget},
    target::{parseHttpTarget, parseHttpsTarget},
    taskTracker::ProxyTaskTracker,
    upstreamClient::HttpsUpstreamClients,
};

/// 复用 HTTP/HTTPS 转发、录制和工具流水线，处理已由 SOCKS5 CONNECT 建立的应用层连接。
///
/// 运行上下文：SOCKS5 核心只在首段字节确认协议后调用本处理器；它不监听端口，也不参与 SOCKS5 协商。
/// 参数：new 接收与 HTTP 监听器一致的配置和共享运行时依赖；各 serve 方法接收已完成 SOCKS5 响应的客户端套接字。
/// 失败语义：TLS 配置、握手或客户端 I/O 失败以 io::Error 返回给 SOCKS5 会话；HTTP 请求级失败仍通过 HTTP 响应与录制状态表达。
#[derive(Clone)]
pub struct SocksHttpTunnelHandler {
    context: ProxyContext,
}

/// 描述 SOCKS/透明 HTTP 接管的逻辑 authority 与可选固定建连 IP。
#[derive(Clone)]
pub struct SocksHttpTarget {
    pub host: String,
    pub port: u16,
    pub fixedAddress: Option<IpAddr>,
    pub clientProcessName: Option<String>,
    pub clientProcessId: Option<u32>,
}

/// 指定 SOCKS5 已识别连接的 HTTP 解释方式，HTTPS 必须固定到 SOCKS5 CONNECT 的目标以阻止跨主机复用。
#[derive(Clone)]
enum SocksHttpMode {
    Plain { host: String, port: u16 },
    Tls { host: String, port: u16 },
}

impl SocksHttpTunnelHandler {
    /// 创建 SOCKS5 应用层处理器；上游客户端池与普通 HTTP 监听器使用同一证书验证和超时配置。
    ///
    /// 运行上下文：控制服务启动 SOCKS5 时构造一次并在会话间共享，避免每个 CONNECT 重建 TLS 客户端池。
    /// 参数：config、capture、ssl、pipeline、pluginHost 和 cancellation 均来自同一控制服务实例。
    /// 失败语义：HTTP 配置或上游 TLS 配置无效时返回 HttpProxyError，调用方不得启动带半初始化拦截器的 SOCKS5 服务。
    pub fn new(
        config: HttpProxyConfig,
        capture: RecordingSession,
        ssl: SslMitmManager,
        pipeline: ToolPipeline,
        pluginHost: PluginHost,
    ) -> Result<Self, HttpProxyError> {
        let dnsSpoofing = Arc::new(
            DnsSpoofingTool::new(DnsSpoofingConfiguration::default())
                .expect("默认 DNS 配置必须有效"),
        );
        Self::newWithDns(
            config,
            HttpProxyDependencies {
                capture,
                ssl,
                pipeline,
                pluginHost,
                dnsSpoofing,
            },
        )
    }

    /// 创建与 SOCKS5 核心共享 DNS 映射器的应用层处理器，保证预连接和接管后的新连接指向同一 IP。
    ///
    /// 运行上下文：控制服务在启动监听器时调用，配置热更新通过共享工具立即作用于后续解析。
    /// 失败语义：HTTP 或 TLS 客户端配置无效时返回 `HttpProxyError`，不保留半初始化连接池。
    pub fn newWithDns(
        config: HttpProxyConfig,
        dependencies: HttpProxyDependencies,
    ) -> Result<Self, HttpProxyError> {
        let HttpProxyDependencies {
            capture,
            ssl,
            pipeline,
            pluginHost,
            dnsSpoofing,
        } = dependencies;
        config.validate()?;
        let httpClient = buildHttpUpstreamClient(&config, dnsSpoofing.clone());
        ssl.upstreamClientConfiguration(None)
            .map_err(|_| HttpProxyError::TlsConfigurationFailed)?;
        let outbound = crate::server::buildOutboundConnector(&config);
        let httpsClients =
            HttpsUpstreamClients::new(ssl.clone(), dnsSpoofing.clone(), outbound.clone());
        Ok(Self {
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
                // SOCKS5 会话在接管时传入自己的取消令牌，构造期不绑定某个监听实例。
                cancellation: CancellationToken::new(),
                // SOCKS5 已在外层管理会话生命周期；此处禁止 CONNECT 进入升级路径，因此不会派生受跟踪隧道任务。
                tasks: ProxyTaskTracker::new(),
                // SOCKS5 已由外层注册表计量，独立账本仅满足共享 HTTP 上下文契约，避免重复累计公开指标。
                metrics: crate::HttpRuntimeMetrics::default(),
                clientProcessName: None,
                clientProcessId: None,
            },
        })
    }

    /// 关闭任务跟踪器并等待 SOCKS5 隧道派生的升级与响应任务全部退出。
    ///
    /// 运行上下文：外层 SOCKS5 接收循环已取消全部会话后调用；失败语义由外层统一停机预算决定。
    pub async fn shutdown(&self) {
        self.context.tasks.close();
        self.context.tasks.wait().await;
    }

    /// 强制中止停机预算内未退出的隧道派生任务并等待析构，确保超时不会留下后台连接。
    pub async fn abortAndWait(&self) {
        self.context.tasks.abortAllAndWait().await;
    }

    /// 服务经 SOCKS5 CONNECT 进入的明文 HTTP/1.x 或 HTTP/2 连接，并将每个请求录制为 HTTP 事务。
    ///
    /// 运行上下文：分类器已验证完整请求头的 Host 与 SOCKS5 目标一致，避免把“通往另一个 HTTP 代理”的原始隧道误接管。
    /// 参数：clientStream 是 SOCKS5 成功响应后的客户端字节流；clientAddress 用于事务来源字段。
    /// 失败语义：连接级协议中断按正常 EOF 收束；取消信号或底层 I/O 失败返回 io::Error。
    pub async fn servePlainHttp<S>(
        &self,
        clientStream: S,
        clientAddress: SocketAddr,
        target: SocksHttpTarget,
        cancellation: CancellationToken,
    ) -> io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut context = self.contextForTarget(&target);
        context.cancellation = cancellation;
        serveHttpConnection(
            TokioIo::new(clientStream),
            clientAddress,
            context,
            SocksHttpMode::Plain {
                host: target.host,
                port: target.port,
            },
        )
        .await
    }

    /// 接收 SOCKS5 CONNECT 后的 TLS ClientHello，签发目标证书并把解密 HTTP/1.x 或 HTTP/2 请求录制为 HTTPS 事务。
    ///
    /// 运行上下文：仅在 SSL 规则已经命中 SOCKS5 目标且首段符合 TLS 记录时调用；上游仍由共享 HTTPS 客户端执行严格证书验证。
    /// 参数：targetHost、targetPort 来自 SOCKS5 CONNECT，决定叶证书、URL 位置和上游目标绑定。
    /// 失败语义：证书生成、握手超时或取消均关闭当前 SOCKS5 会话，并创建一条使用稳定错误码的
    /// CONNECT 失败事务；不存在 HTTP 请求不代表失败可以消失，界面必须能展示握手阶段及原因。
    pub async fn serveInterceptedHttps<S>(
        &self,
        clientStream: S,
        clientAddress: SocketAddr,
        target: SocksHttpTarget,
        cancellation: CancellationToken,
    ) -> io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut context = self.contextForTarget(&target);
        context.cancellation = cancellation;
        let configuration = match self.context.ssl.downstreamServerConfiguration(&target.host) {
            Ok(configuration) => configuration,
            Err(_) => {
                recordSocksConnectionFailure(
                    &context,
                    SocksConnectionFailure::fromTarget(
                        &target,
                        clientAddress,
                        RequestFailure::DownstreamTlsHandshake,
                    ),
                )
                .await;
                return Err(io::Error::other("SOCKS5 TLS 服务器配置失败"));
            }
        };
        let acceptor = TlsAcceptor::from(configuration);
        let downstreamTls = tokio::select! {
            biased;
            () = context.cancellation.cancelled() => {
                recordSocksConnectionFailure(
                    &context,
                    SocksConnectionFailure::fromTarget(
                        &target,
                        clientAddress,
                        RequestFailure::Cancelled,
                    ),
                ).await;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "SOCKS5 TLS 解密已取消"));
            }
            result = timeout(context.config.connectTimeout(), acceptor.accept(clientStream)) => result,
        };
        let downstreamTls = match downstreamTls {
            Ok(Ok(stream)) => {
                context.ssl.recordHandshakeSuccess();
                stream
            }
            Ok(Err(error)) => {
                context.ssl.recordHandshakeFailure();
                recordSocksConnectionFailure(
                    &context,
                    SocksConnectionFailure::fromTarget(
                        &target,
                        clientAddress,
                        RequestFailure::DownstreamTlsHandshake,
                    ),
                )
                .await;
                return Err(io::Error::other(error));
            }
            Err(_) => {
                context.ssl.recordHandshakeFailure();
                recordSocksConnectionFailure(
                    &context,
                    SocksConnectionFailure::fromTarget(
                        &target,
                        clientAddress,
                        RequestFailure::DownstreamTlsHandshake,
                    ),
                )
                .await;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "SOCKS5 TLS 握手超时",
                ));
            }
        };
        serveHttpConnection(
            TokioIo::new(downstreamTls),
            clientAddress,
            context,
            SocksHttpMode::Tls {
                host: target.host,
                port: target.port,
            },
        )
        .await
    }

    /// 为透明连接创建固定原始 IP 的独立连接池；普通 SOCKS5 连接复用共享池。
    ///
    /// 运行上下文：仅透明接管设置 `fixedAddress`，HTTP URI 与 TLS SNI 仍使用已核验逻辑域名。
    /// 失败语义：本函数只组装不可变连接策略，实际 I/O 错误由请求转发阶段返回。
    fn contextForTarget(&self, target: &SocksHttpTarget) -> ProxyContext {
        let mut context = self.context.clone();
        context.clientProcessName = target.clientProcessName.clone();
        context.clientProcessId = target.clientProcessId;
        let Some(address) = target.fixedAddress else {
            return context;
        };
        let fixedTarget = FixedConnectTarget {
            host: target.host.clone(),
            port: target.port,
            address,
        };
        context.httpClient = buildHttpUpstreamClientWithFixedTarget(
            &context.config,
            Arc::clone(&context.dnsSpoofing),
            fixedTarget.clone(),
        );
        context.httpsClients = HttpsUpstreamClients::newWithFixedTarget(
            context.ssl.clone(),
            Arc::clone(&context.dnsSpoofing),
            context.outbound.clone(),
            fixedTarget,
        );
        context
    }
}

/// 在一个已分流的下游字节流上运行 HTTP/1.x 与 HTTP/2 状态机；Plain 与 TLS 仅在目标解析和上游传输选择处不同。
///
/// 运行上下文：本函数不处理 SOCKS5 协商、原始 TCP 复制或 UDP 数据报，确保已解密事务不会回流到原始流录制。
/// 参数：downstream 是明文或已解密的双向字节流；mode 标识协议边界；context 提供录制和上游客户端。
/// 失败语义：请求形成后的错误由标准 HTTP 事务记录；请求形成前的协议错误会生成已知目标的
/// CONNECT 失败事务并返回 I/O 错误；取消时优雅关闭，排空超时返回中断错误。
async fn serveHttpConnection<S>(
    downstream: S,
    clientAddress: SocketAddr,
    context: ProxyContext,
    mode: SocksHttpMode,
) -> io::Result<()>
where
    S: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let requestObserved = Arc::new(AtomicBool::new(false));
    let serviceRequestObserved = Arc::clone(&requestObserved);
    let serviceContext = context.clone();
    let serviceMode = mode.clone();
    let service = service_fn(move |request: Request<Incoming>| {
        serviceRequestObserved.store(true, Ordering::Release);
        let requestContext = serviceContext.clone();
        let requestMode = serviceMode.clone();
        async move {
            let response = match requestMode {
                SocksHttpMode::Plain { host, port } => match parseHttpTarget(&request) {
                    Ok(target)
                        if target.location.host.eq_ignore_ascii_case(&host)
                            && target.location.port == port =>
                    {
                        forwardDecodedHttp(
                            request,
                            clientAddress,
                            requestContext,
                            target,
                            UpstreamTransport::Http,
                            false,
                        )
                        .await
                    }
                    _ => {
                        recordSocksConnectionFailure(
                            &requestContext,
                            SocksConnectionFailure::new(
                                SocksFailureTarget {
                                    host,
                                    port,
                                    tls: false,
                                    clientAddress,
                                },
                                RequestFailure::InvalidRequest,
                            ),
                        )
                        .await;
                        failureResponse(RequestFailure::InvalidRequest)
                    }
                },
                SocksHttpMode::Tls { host, port } => {
                    match parseHttpsTarget(&request, &host, port) {
                        Ok(target) => {
                            forwardDecodedHttp(
                                request,
                                clientAddress,
                                requestContext,
                                target,
                                UpstreamTransport::Https,
                                false,
                            )
                            .await
                        }
                        Err(_) => {
                            recordSocksConnectionFailure(
                                &requestContext,
                                SocksConnectionFailure::new(
                                    SocksFailureTarget {
                                        host,
                                        port,
                                        tls: true,
                                        clientAddress,
                                    },
                                    RequestFailure::InvalidRequest,
                                ),
                            )
                            .await;
                            failureResponse(RequestFailure::InvalidRequest)
                        }
                    }
                }
            };
            Ok::<_, Infallible>(response)
        }
    });
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .keep_alive(true)
        .max_buf_size(context.config.maxHeaderBytes)
        .timer(TokioTimer::new())
        .header_read_timeout(context.config.headerReadTimeout());
    let connection = builder.serve_connection(downstream, service);
    tokio::pin!(connection);
    tokio::select! {
        result = &mut connection => {
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    // Hyper 在构造 Request 之前拒绝畸形首行/头部时不会进入 service；过去该失败既无
                    // HTTP 事务也无 SOCKS 原始事务。目标已由 CONNECT 确认，因此在此补一条可定位记录。
                    if !requestObserved.load(Ordering::Acquire) {
                        recordSocksConnectionFailure(
                            &context,
                            SocksConnectionFailure::fromMode(
                                &mode,
                                clientAddress,
                                RequestFailure::InvalidRequest,
                            ),
                        ).await;
                    }
                    Err(io::Error::other(error))
                },
            }
        }
        () = context.cancellation.cancelled() => {
            connection.as_mut().graceful_shutdown();
            if timeout(context.config.connectionDrainTimeout(), &mut connection).await.is_err() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "SOCKS5 HTTP 连接关闭超时"));
            }
            Ok(())
        }
    }
}

/// 保存一次尚未形成 HTTP 请求的已知目标连接失败；该领域对象收拢协议、端点与失败原因，避免调用方传递易错的位置参数。
struct SocksConnectionFailure {
    host: String,
    port: u16,
    tls: bool,
    clientAddress: SocketAddr,
    failure: RequestFailure,
}

/// 保存失败事务的已验证端点与协议身份；该对象避免 host、port、TLS 标志和来源地址在调用链中错位。
struct SocksFailureTarget {
    host: String,
    port: u16,
    tls: bool,
    clientAddress: SocketAddr,
}

impl SocksConnectionFailure {
    /// 从已验证 SOCKS 目标构造失败描述；目标字段只用于事务 Location，不包含底层错误文本。
    fn fromTarget(
        target: &SocksHttpTarget,
        clientAddress: SocketAddr,
        failure: RequestFailure,
    ) -> Self {
        Self::new(
            SocksFailureTarget {
                host: target.host.clone(),
                port: target.port,
                tls: true,
                clientAddress,
            },
            failure,
        )
    }

    /// 从 HTTP 连接模式构造失败描述；Plain 与 TLS 共用相同的结构化录制入口。
    fn fromMode(mode: &SocksHttpMode, clientAddress: SocketAddr, failure: RequestFailure) -> Self {
        match mode {
            SocksHttpMode::Plain { host, port } => Self::new(
                SocksFailureTarget {
                    host: host.clone(),
                    port: *port,
                    tls: false,
                    clientAddress,
                },
                failure,
            ),
            SocksHttpMode::Tls { host, port } => Self::new(
                SocksFailureTarget {
                    host: host.clone(),
                    port: *port,
                    tls: true,
                    clientAddress,
                },
                failure,
            ),
        }
    }

    /// 构造完整失败描述；参数由已验证目标提供，失败语义始终通过稳定错误码而不是底层字符串公开。
    fn new(target: SocksFailureTarget, failure: RequestFailure) -> Self {
        Self {
            host: target.host,
            port: target.port,
            tls: target.tls,
            clientAddress: target.clientAddress,
            failure,
        }
    }
}

/// 将 TLS 握手、TLS 配置和请求解析失败写入统一事务存储；录制关闭或已清空时不影响真实连接终止语义。
async fn recordSocksConnectionFailure(context: &ProxyContext, failure: SocksConnectionFailure) {
    let scheme = if failure.tls { "https" } else { "http" };
    let displayHost = if failure.host.contains(':') {
        format!("[{}]", failure.host)
    } else {
        failure.host.clone()
    };
    let input = BeginTransaction {
        protocol: TransactionProtocol::Tunnel,
        method: Method::CONNECT.as_str().to_owned(),
        location: ResolvedLocation {
            protocol: scheme.to_owned(),
            host: failure.host,
            port: failure.port,
            path: String::new(),
            query: String::new(),
            display: format!("{scheme}://{displayHost}:{}", failure.port),
        },
        clientAddress: failure.clientAddress.to_string(),
        clientProcessName: context.clientProcessName.clone(),
        clientProcessId: context.clientProcessId,
        contentType: String::new(),
        startAtMilliseconds: currentTimeMilliseconds(),
    };
    let transaction = match CaptureTransaction::begin(&context.capture, input).await {
        Ok(transaction) => transaction,
        Err(error) => {
            tracing::error!(
                errorCode = error.code(),
                messageKey = error.messageKey(),
                "captureOperationFailed"
            );
            return;
        }
    };
    let Some(transaction) = transaction else {
        return;
    };
    if let Err(error) = transaction.fail(failure.failure).await {
        tracing::error!(
            errorCode = error.code(),
            messageKey = error.messageKey(),
            "captureOperationFailed"
        );
    }
}
