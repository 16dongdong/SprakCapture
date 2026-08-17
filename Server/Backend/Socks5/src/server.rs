use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use plugin_host::{
    ConnectionMetadata, PluginHost, Socks5AuthenticationDecision, Socks5AuthenticationRequest,
    TransportKind,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    accountService::{AccountServiceClient, AccountServiceClientConfig, AccountTrafficLease},
    address::{AddressOverride, TargetAddress, TargetHost},
    config::{AuthenticationMode, Socks5Config},
    error::{Result, Socks5Error},
    interception::{PortProtocolHandler, TcpTunnel, TcpTunnelDisposition, TcpTunnelInterceptor},
    model::{
        CaptureGeneration, ServerSnapshot, ServerStopOutcome, SessionApplicationProtocol,
        SessionEvent, SessionState,
    },
    protocol::{
        SocksRequest, commandBind, commandConnect, commandUdpAssociate, mapIoErrorToReply,
        negotiateAuthentication, negotiateExternalAuthentication, negotiatePluginAuthentication,
        readRequest, replyAddressTypeNotSupported, replyCommandNotSupported,
        replyConnectionNotAllowed, replyGeneralFailure, replyHostUnreachable, replySucceeded,
        replyTtlExpired, writeReply,
    },
    registry::{SessionRegistry, SessionUpdate},
    relay::{RelaySession, relayBidirectional},
    udpRelay::{UdpAssociationContext, UdpAssociationSession, runUdpAssociation},
};

/// 聚合单个客户端任务依赖，避免异步任务参数顺序与生命周期发生漂移。
#[derive(Clone)]
struct ClientContext {
    config: Socks5Config,
    registry: SessionRegistry,
    cancellation: CancellationToken,
    pluginHost: PluginHost,
    tunnelInterceptor: Option<Arc<dyn TcpTunnelInterceptor>>,
    addressOverride: Option<Arc<dyn AddressOverride>>,
    outboundConnector: Option<transport_core::OutboundConnector>,
    accountServiceClient: Option<AccountServiceClient>,
}

/// 聚合已创建会话及客户端依赖，命令处理器共享稳定会话 ID。
#[derive(Clone)]
struct SessionContext {
    client: ClientContext,
    sessionId: String,
}

/// 持有透明进程捕获专用的双栈回环监听器；两族共享端口，控制层只需配置一次代理端口。
struct InternalCaptureListeners {
    ipv4: TcpListener,
    ipv6: TcpListener,
}

impl InternalCaptureListeners {
    /// 返回两个真实绑定端点；顺序固定为 IPv4、IPv6，便于状态检查和契约测试。
    fn addresses(&self) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![self.ipv4.local_addr()?, self.ipv6.local_addr()?])
    }
}

/// 表示一个已绑定且由后台任务驱动的 SOCKS5 服务实例。
pub struct RunningServer {
    boundAddress: SocketAddr,
    internalCaptureAddresses: Vec<SocketAddr>,
    cancellation: CancellationToken,
    task: JoinHandle<Result<()>>,
    registry: SessionRegistry,
    exitReceiver: watch::Receiver<Option<String>>,
}

impl RunningServer {
    /// 返回数据面实际监听地址。
    pub fn boundAddress(&self) -> SocketAddr {
        self.boundAddress
    }

    /// 返回供 WinDivert 配置使用的双栈通配端点；实际监听仅绑定 IPv4/IPv6 回环地址。
    ///
    /// 运行上下文：ProcessCapture 以未指定地址表示“按原流地址族选择回环地址”，两个族共享
    /// 返回端口。普通 SOCKS5 调用方未启用内部捕获时返回 None。
    pub fn internalCaptureAddress(&self) -> Option<SocketAddr> {
        self.internalCaptureAddresses
            .first()
            .map(|address| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), address.port()))
    }

    /// 返回内部捕获监听器的真实双栈回环端点；用于诊断和验证，不作为 WinDivert 配置值。
    pub fn internalCaptureAddresses(&self) -> &[SocketAddr] {
        &self.internalCaptureAddresses
    }

    /// 订阅会话创建、流量、状态和关闭事件；需要基线时必须在本调用之后读取 snapshot。
    pub fn subscribeEvents(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.registry.subscribe()
    }

    /// 返回数据面录制代际句柄；清空录制后投影器用它拒绝旧广播队列中的会话快照。
    pub fn captureGeneration(&self) -> CaptureGeneration {
        self.registry.captureGeneration()
    }

    /// 订阅接受循环完成状态；空字符串表示非请求的正常结束，非空字符串表示运行错误。
    pub fn subscribeExit(&self) -> watch::Receiver<Option<String>> {
        self.exitReceiver.clone()
    }

    /// 返回当前服务与会话快照；流量总数由注册表的同步指标账本读取。
    pub fn snapshot(&self) -> ServerSnapshot {
        let sessions = self.registry.snapshots();
        ServerSnapshot {
            boundAddress: self.boundAddress,
            sessions,
            metrics: self.registry.metrics(),
        }
    }

    /// 清除已关闭历史并返回删除 ID；活动连接不受影响。
    pub fn clearClosedSessions(&self) -> Vec<String> {
        self.registry.clearClosed()
    }

    /// 释放全部会话的原始流镜像；录制清空后连接继续透明转发但不再持有已失效正文。
    pub fn clearCapturedBytes(&self) {
        self.registry.clearCapturedBytes();
    }

    /// 释放单条已成功投影会话的原始流镜像；录制器确认接管前必须保留，以支持广播丢帧恢复。
    pub fn releaseCapturedBytes(&self, sessionId: &str) {
        self.registry.releaseCapturedBytes(sessionId);
    }

    /// 通知接受循环并强制中止所有会话；停止和重启均不等待客户端或上游优雅排空。
    pub async fn stop(self) -> ServerStopOutcome {
        let Self {
            boundAddress,
            internalCaptureAddresses: _,
            cancellation,
            task,
            registry,
            exitReceiver: _,
        } = self;
        cancellation.cancel();
        let errorMessage = match task.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => {
                let errorMessage = format!("服务任务异常结束：{error}");
                registry.closeActive(&errorMessage);
                Some(errorMessage)
            }
        };
        ServerStopOutcome {
            snapshot: ServerSnapshot {
                boundAddress,
                sessions: registry.snapshots(),
                metrics: registry.metrics(),
            },
            errorMessage,
        }
    }
}

/// 校验配置、绑定唯一数据面监听器并返回可控制的运行实例。
pub async fn startSocks5Server(config: Socks5Config) -> Result<RunningServer> {
    startSocks5ServerWithInterception(config, PluginHost::disabled(), None).await
}

/// 校验配置、绑定 SOCKS5 监听器并注入统一插件宿主；无插件调用方继续使用 `startSocks5Server`。
pub async fn startSocks5ServerWithPlugins(
    config: Socks5Config,
    pluginHost: PluginHost,
) -> Result<RunningServer> {
    startSocks5ServerWithInterception(config, pluginHost, None).await
}

/// 启动支持应用层分类的 SOCKS5 服务；拦截器只有在 CONNECT 成功后拿到独占套接字，原始 TCP 和 UDP 生命周期仍由核心统一管理。
///
/// 运行上下文：后台将 HTTP/HTTPS 分类器注入该入口，使 SOCKS5 隧道可复用 HTTP 录制与 TLS 解密链；独立调用方传入 None 时保持纯 SOCKS5 行为。
/// 参数：config 是已公开的 SOCKS5 配置；pluginHost 承担原始流 Hook；tunnelInterceptor 可接管已确认的应用层连接。
/// 失败语义：配置或监听绑定失败直接返回错误；单连接拦截失败只关闭该会话，不使监听循环退出。
pub async fn startSocks5ServerWithInterception(
    config: Socks5Config,
    pluginHost: PluginHost,
    tunnelInterceptor: Option<Arc<dyn TcpTunnelInterceptor>>,
) -> Result<RunningServer> {
    startSocks5ServerWithInterceptionAndResolver(config, pluginHost, tunnelInterceptor, None).await
}

/// 启动同时支持应用层接管与出站域名覆盖的 SOCKS5 服务。
///
/// 运行上下文：控制服务注入热更新解析器；独立库调用方继续通过旧入口获得系统 DNS 行为。
/// 失败语义：配置或监听失败时不创建后台任务，单次域名解析失败只终止对应会话。
pub async fn startSocks5ServerWithInterceptionAndResolver(
    config: Socks5Config,
    pluginHost: PluginHost,
    tunnelInterceptor: Option<Arc<dyn TcpTunnelInterceptor>>,
    addressOverride: Option<Arc<dyn AddressOverride>>,
) -> Result<RunningServer> {
    startFusedProxyServer(
        config,
        FusedProxyDependencies {
            pluginHost,
            tunnelInterceptor,
            addressOverride,
            protocolHandler: None,
            outboundConnector: None,
        },
        FusedProxyOptions::default(),
    )
    .await
}

/// 汇总融合代理的数据面依赖；这些对象共享整个监听生命周期，并由接受循环按连接只读复用。
pub struct FusedProxyDependencies {
    pub pluginHost: PluginHost,
    pub tunnelInterceptor: Option<Arc<dyn TcpTunnelInterceptor>>,
    pub addressOverride: Option<Arc<dyn AddressOverride>>,
    pub protocolHandler: Option<Arc<dyn PortProtocolHandler>>,
    pub outboundConnector: Option<transport_core::OutboundConnector>,
}

/// 描述融合代理的可选启动能力；默认值保持普通单账号 SOCKS 行为，不创建内部捕获监听器。
#[derive(Default)]
pub struct FusedProxyOptions {
    pub enableInternalCaptureListener: bool,
    pub accountServiceConfig: Option<AccountServiceClientConfig>,
}

/// 绑定唯一代理端口，并按首字节把 SOCKS5 与其它连接交给独立协议状态机。
///
/// 运行上下文：控制服务注入 HTTP/透明处理器后只启动本监听器；独立库调用方继续使用旧入口。
/// 失败语义：绑定失败不创建后台任务；单条非 SOCKS5 连接失败不影响接受循环。
pub async fn startFusedProxyServer(
    config: Socks5Config,
    dependencies: FusedProxyDependencies,
    options: FusedProxyOptions,
) -> Result<RunningServer> {
    let FusedProxyDependencies {
        pluginHost,
        tunnelInterceptor,
        addressOverride,
        protocolHandler,
        outboundConnector,
    } = dependencies;
    let FusedProxyOptions {
        enableInternalCaptureListener,
        accountServiceConfig,
    } = options;
    config.validate()?;
    let accountServiceClient = match (&config.authenticationMode, accountServiceConfig) {
        (AuthenticationMode::AccountService, Some(accountConfig)) => {
            Some(AccountServiceClient::new(accountConfig)?)
        }
        (AuthenticationMode::AccountService, None) => {
            return Err(Socks5Error::Configuration(
                "账号服务认证模式缺少内部客户端配置".to_owned(),
            ));
        }
        (_, Some(_)) => {
            return Err(Socks5Error::Configuration(
                "非账号服务认证模式不得配置内部账号客户端".to_owned(),
            ));
        }
        (_, None) => None,
    };
    let listener = TcpListener::bind(config.listenAddress()).await?;
    let boundAddress = listener.local_addr()?;
    let internalListeners = if enableInternalCaptureListener {
        Some(bindInternalCaptureListeners().await?)
    } else {
        None
    };
    let internalCaptureAddresses = internalListeners
        .as_ref()
        .map(InternalCaptureListeners::addresses)
        .transpose()?
        .unwrap_or_default();
    let registry = SessionRegistry::new(config.sessionHistoryLimit);
    let cancellation = CancellationToken::new();
    let serverCancellation = cancellation.clone();
    let serverRegistry = registry.clone();
    let (exitSender, exitReceiver) = watch::channel(None);
    let task = tokio::spawn(async move {
        let result = runAcceptLoop(
            listener,
            internalListeners,
            AcceptLoopContext {
                config,
                registry: serverRegistry,
                cancellation: serverCancellation,
                pluginHost,
                tunnelInterceptor,
                addressOverride,
                protocolHandler,
                outboundConnector,
                accountServiceClient,
            },
        )
        .await;
        let errorMessage = result
            .as_ref()
            .err()
            .map_or_else(String::new, ToString::to_string);
        exitSender.send_replace(Some(errorMessage));
        result
    });
    Ok(RunningServer {
        boundAddress,
        internalCaptureAddresses,
        cancellation,
        task,
        registry,
        exitReceiver,
    })
}

/// 汇总融合监听循环共享的只读依赖，避免生命周期参数在调用链中分散和错位。
struct AcceptLoopContext {
    config: Socks5Config,
    registry: SessionRegistry,
    cancellation: CancellationToken,
    pluginHost: PluginHost,
    tunnelInterceptor: Option<Arc<dyn TcpTunnelInterceptor>>,
    addressOverride: Option<Arc<dyn AddressOverride>>,
    protocolHandler: Option<Arc<dyn PortProtocolHandler>>,
    outboundConnector: Option<transport_core::OutboundConnector>,
    accountServiceClient: Option<AccountServiceClient>,
}

/// 运行接受循环并拥有全部会话任务；取消后不再接受新连接并立即中止现有任务。
///
/// 运行上下文：融合端口绑定成功后由唯一后台任务调用，所有连接继承同一配置和取消令牌。
/// 参数：listener 为已绑定监听器，context 持有会话注册表、协议处理器和出站连接器。
/// 失败语义：accept、子任务或排空失败会终止服务任务，并由控制面发布故障状态。
async fn runAcceptLoop(
    listener: TcpListener,
    internalListeners: Option<InternalCaptureListeners>,
    context: AcceptLoopContext,
) -> Result<()> {
    let AcceptLoopContext {
        config,
        registry,
        cancellation,
        pluginHost,
        tunnelInterceptor,
        addressOverride,
        protocolHandler,
        outboundConnector,
        accountServiceClient,
    } = context;
    let mut connections = JoinSet::new();
    let hasInternalListener = internalListeners.is_some();
    let connectionPermits = Arc::new(Semaphore::new(config.maxConnections));
    let acceptResult = loop {
        tokio::select! {
            _ = cancellation.cancelled() => break Ok(()),
            accepted = acceptConnection(&listener, internalListeners.as_ref()) => {
                let (stream, peerAddress, internalCaptureConnection) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(Socks5Error::Io(error)),
                };
                let Ok(connectionPermit) = connectionPermits.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let connectionConfig = config.clone();
                let connectionRegistry = registry.clone();
                // 使用服务令牌的子级，让账号租约撤销只影响当前连接而不停止整个 SOCKS5 实例。
                let connectionCancellation = cancellation.child_token();
                let connectionPluginHost = pluginHost.clone();
                let connectionTunnelInterceptor = tunnelInterceptor.clone();
                let connectionAddressOverride = addressOverride.clone();
                let connectionProtocolHandler = protocolHandler.clone();
                let connectionOutboundConnector = outboundConnector.clone();
                let connectionAccountServiceClient = accountServiceClient.clone();
                connections.spawn(async move {
                    let _connectionPermit = connectionPermit;
                    if internalCaptureConnection {
                        if let Some(handler) = connectionProtocolHandler
                            && handler.claimsConnection(&stream, peerAddress)
                        {
                            handler
                                .serve(stream, peerAddress, connectionCancellation)
                                .await;
                        }
                        // 内部入口只承接已经登记的透明连接；未命中时立即关闭，禁止回落到显式 HTTP/SOCKS。
                        return;
                    }
                    if let Some(handler) = connectionProtocolHandler {
                        if !hasInternalListener && handler.claimsConnection(&stream, peerAddress)
                        {
                            handler
                                .serve(stream, peerAddress, connectionCancellation)
                                .await;
                            return;
                        }
                        let mut firstByte = [0_u8; 1];
                        let peekResult = tokio::select! {
                            _ = connectionCancellation.cancelled() => return,
                            result = timeout(connectionConfig.readTimeout(), stream.peek(&mut firstByte)) => result,
                        };
                        match peekResult {
                            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return,
                            Ok(Ok(_)) if firstByte[0] != 0x05 => {
                                handler
                                    .serve(stream, peerAddress, connectionCancellation)
                                    .await;
                                return;
                            }
                            Ok(Ok(_)) => {}
                        }
                    }
                    runClient(
                        stream,
                        peerAddress,
                        ClientContext {
                            config: connectionConfig,
                            registry: connectionRegistry,
                            cancellation: connectionCancellation,
                            pluginHost: connectionPluginHost,
                            tunnelInterceptor: connectionTunnelInterceptor,
                            addressOverride: connectionAddressOverride,
                            outboundConnector: connectionOutboundConnector,
                            accountServiceClient: connectionAccountServiceClient,
                        },
                    ).await;
                });
            }
            Some(joinResult) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = joinResult {
                    break Err(Socks5Error::Runtime(error.to_string()));
                }
            }
        }
    };
    cancellation.cancel();
    // 停止和重启均以释放套接字为第一目标；JoinSet 中止会直接析构 SOCKS5、原始 TCP、UDP
    // 与透明入口连接，不等待读写超时或对端关闭。处理器的派生 CONNECT/TLS 任务使用同一语义。
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    if let Some(handler) = &protocolHandler {
        handler.abortAndWait().await;
    }
    registry.closeActive("服务已停止，连接已强制关闭");
    if let Err(error) = &acceptResult {
        registry.closeActive(&format!("服务任务异常结束：{error}"));
    }
    acceptResult
}

/// 在 IPv4 与 IPv6 回环地址上绑定同一个随机端口，使透明捕获可以按原流地址族接入。
///
/// 运行上下文：内部入口不暴露在网卡地址上；若跨地址族端口发生暂态碰撞，则重新选择一组端口。
/// 失败语义：任一地址族无法绑定时拒绝启动进程捕获，禁止静默退化为单栈漏流量。
async fn bindInternalCaptureListeners() -> io::Result<InternalCaptureListeners> {
    let mut lastIpv6Error = None;
    for _ in 0..16 {
        let ipv4 = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
        let port = ipv4.local_addr()?.port();
        match TcpListener::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)).await {
            Ok(ipv6) => return Ok(InternalCaptureListeners { ipv4, ipv6 }),
            Err(error) => lastIpv6Error = Some(error),
        }
    }
    Err(lastIpv6Error.expect("双栈绑定至少执行一次"))
}

/// 从公开端口和可选内部回环端口接受下一条连接；任一监听错误都会终止共享生命周期。
async fn acceptConnection(
    listener: &TcpListener,
    internalListeners: Option<&InternalCaptureListeners>,
) -> io::Result<(TcpStream, SocketAddr, bool)> {
    match internalListeners {
        Some(internalListeners) => tokio::select! {
            accepted = listener.accept() => accepted.map(|(stream, peer)| (stream, peer, false)),
            accepted = internalListeners.ipv4.accept() => accepted.map(|(stream, peer)| (stream, peer, true)),
            accepted = internalListeners.ipv6.accept() => accepted.map(|(stream, peer)| (stream, peer, true)),
        },
        None => listener
            .accept()
            .await
            .map(|(stream, peer)| (stream, peer, false)),
    }
}

/// 为单个控制连接建立会话，并保证取消、失败和正常结束都发布最终快照。
async fn runClient(stream: TcpStream, peerAddress: SocketAddr, client: ClientContext) {
    let sessionId = client.registry.create(peerAddress.to_string());
    let session = SessionContext { client, sessionId };
    let sessionResult = tokio::select! {
        _ = session.client.cancellation.cancelled() => Ok(()),
        result = handleSession(stream, peerAddress, &session) => result,
    };
    let errorMessage = sessionResult
        .err()
        .map_or_else(String::new, |error| error.to_string());
    session
        .client
        .registry
        .close(&session.sessionId, errorMessage);
}

/// 执行认证、请求解析和命令分派；网络流只属于当前会话。
async fn handleSession(
    mut stream: TcpStream,
    peerAddress: SocketAddr,
    session: &SessionContext,
) -> Result<()> {
    let config = &session.client.config;
    let registry = &session.client.registry;
    let sessionId = &session.sessionId;
    registry.update(
        sessionId,
        SessionUpdate {
            username: None,
            command: None,
            targetAddress: None,
            applicationProtocol: None,
            state: SessionState::Authenticating,
        },
    );
    let (username, accountLease) = if config.authenticationMode == AuthenticationMode::Plugin {
        let pluginHost = session.client.pluginHost.clone();
        let connectionId = sessionId.clone();
        let username = negotiatePluginAuthentication(
            &mut stream,
            config.readTimeout(),
            move |username, password| {
                let pluginHost = pluginHost.clone();
                let clientAddress = peerAddress.to_string();
                let connectionId = connectionId.clone();
                async move {
                    match pluginHost
                        .authenticateSocks5(Socks5AuthenticationRequest {
                            connectionId,
                            clientAddress,
                            username,
                            password,
                        })
                        .await
                    {
                        Socks5AuthenticationDecision::Accepted { principalId } => Some(principalId),
                        Socks5AuthenticationDecision::Rejected
                        | Socks5AuthenticationDecision::Unavailable => None,
                    }
                }
            },
        )
        .await?;
        (username, None)
    } else if config.authenticationMode == AuthenticationMode::AccountService {
        let accountServiceClient = session
            .client
            .accountServiceClient
            .clone()
            .ok_or_else(|| Socks5Error::Configuration("账号服务客户端未初始化".to_owned()))?;
        let connectionId = sessionId.clone();
        let cancellation = session.client.cancellation.clone();
        let lease = negotiateExternalAuthentication(
            &mut stream,
            config.readTimeout(),
            move |username, password| async move {
                accountServiceClient
                    .authenticate(crate::accountService::AccountLeaseAuthentication {
                        connectionId: &connectionId,
                        username: &username,
                        password: &password,
                        sourceIp: peerAddress.ip(),
                        cancellation,
                    })
                    .await
            },
        )
        .await?;
        (lease.username().to_owned(), Some(lease))
    } else {
        let username = negotiateAuthentication(
            &mut stream,
            &config.authenticationMode,
            &config.users,
            config.readTimeout(),
        )
        .await?;
        (username, None)
    };
    registry.update(
        sessionId,
        SessionUpdate {
            username: Some(username),
            command: None,
            targetAddress: None,
            applicationProtocol: None,
            state: SessionState::Negotiating,
        },
    );
    // 租约从认证成功即进入心跳生命周期，命令解析或目标连接失败也必须立即提交 final 释放占用。
    let commandFuture = async {
        let request = match readRequest(&mut stream, config.readTimeout()).await {
            Ok(request) => request,
            Err(error) => {
                let replyCode = match error {
                    Socks5Error::UnsupportedAddressType(_) => replyAddressTypeNotSupported,
                    _ => replyGeneralFailure,
                };
                let localAddress = stream.local_addr()?;
                let _ = writeReply(&mut stream, replyCode, localAddress).await;
                return Err(error);
            }
        };
        let commandName = match request.command {
            commandConnect => "connect",
            commandBind => "bind",
            commandUdpAssociate => "udpAssociate",
            other => {
                let localAddress = stream.local_addr()?;
                writeReply(&mut stream, replyCommandNotSupported, localAddress).await?;
                return Err(Socks5Error::UnsupportedCommand(other));
            }
        };
        let state = match request.command {
            commandBind => SessionState::Binding,
            commandUdpAssociate => SessionState::UdpAssociating,
            _ => SessionState::Connecting,
        };
        let applicationProtocol = match request.command {
            commandBind => SessionApplicationProtocol::Tcp,
            commandUdpAssociate => SessionApplicationProtocol::Udp,
            commandConnect => SessionApplicationProtocol::Undetermined,
            _ => unreachable!("命令已在分派前校验"),
        };
        registry.update(
            sessionId,
            SessionUpdate {
                username: None,
                command: Some(commandName.to_owned()),
                // UDP 请求中的地址描述客户端数据报端点而非代理目标；首个成功转发的数据报再发布真实远端。
                targetAddress: Some(if request.command == commandUdpAssociate {
                    String::new()
                } else {
                    request.destination.toString()
                }),
                applicationProtocol: Some(applicationProtocol),
                state,
            },
        );
        match request.command {
            commandConnect => runConnect(stream, request, session, accountLease.clone()).await,
            commandBind => runBind(&mut stream, request, session, accountLease.clone()).await,
            commandUdpAssociate => {
                let controlLocal = stream.local_addr()?;
                runUdpAssociation(
                    &mut stream,
                    request.destination,
                    UdpAssociationContext {
                        controlPeer: peerAddress,
                        controlLocal,
                        config: config.clone(),
                        addressOverride: session.client.addressOverride.clone(),
                    },
                    UdpAssociationSession {
                        registry: registry.clone(),
                        sessionId: sessionId.to_owned(),
                        cancellation: session.client.cancellation.clone(),
                        pluginHost: session.client.pluginHost.clone(),
                        clientAddress: peerAddress.to_string(),
                        accountLease: accountLease.clone(),
                    },
                )
                .await
            }
            _ => unreachable!("命令已在分派前校验"),
        }
    };
    let commandResult = match accountLease.as_ref() {
        Some(lease) => tokio::select! {
            result = commandFuture => result,
            _ = lease.cancelled() => Err(Socks5Error::AuthenticationFailed),
        },
        None => commandFuture.await,
    };
    if let Some(lease) = accountLease {
        lease.finish().await;
    }
    commandResult
}

/// 建立远端 TCP 连接、发送成功响应并进入双向转发。
async fn runConnect(
    mut stream: TcpStream,
    request: SocksRequest,
    session: &SessionContext,
    accountLease: Option<AccountTrafficLease>,
) -> Result<()> {
    let config = &session.client.config;
    let remoteStream = match connectTarget(
        &request.destination,
        config,
        session.client.addressOverride.as_deref(),
        session.client.outboundConnector.as_ref(),
    )
    .await
    {
        Ok(remoteStream) => remoteStream,
        Err(error) => {
            let localAddress = stream.local_addr()?;
            let replyCode = match &error {
                Socks5Error::Io(ioError) => mapIoErrorToReply(ioError),
                Socks5Error::Timeout(_) => replyTtlExpired,
                _ => replyHostUnreachable,
            };
            writeReply(&mut stream, replyCode, localAddress).await?;
            return Err(error);
        }
    };
    let boundAddress = remoteStream.local_addr()?;
    writeReply(&mut stream, replySucceeded, boundAddress).await?;
    session.client.registry.update(
        &session.sessionId,
        SessionUpdate {
            username: None,
            command: None,
            targetAddress: None,
            applicationProtocol: None,
            state: SessionState::Relaying,
        },
    );
    let clientAddress = stream.peer_addr()?;
    let targetHost = request.destination.hostString();
    let targetPort = request.destination.port;
    // 只有进程内地址覆盖会改变逻辑目标与实际目标的对应关系。二级代理返回的
    // `peer_addr` 是代理端点而不是最终目标，若把它交给 HTTP/TLS 接管器，重建
    // 上游连接时会错误地直连代理端口。因此命中覆盖时固定映射 IP，其余路径继续
    // 保留原始目标，让共享出站连接器按既有策略完成 DNS 或二级代理握手。
    let connectHost = match &request.destination.host {
        TargetHost::Domain(domain) => session
            .client
            .addressOverride
            .as_deref()
            .and_then(|resolver| resolver.resolveIp(domain))
            .map_or_else(|| targetHost.clone(), |address| address.to_string()),
        TargetHost::Ip(address) => address.to_string(),
    };
    let tunnel = TcpTunnel {
        clientStream: stream,
        remoteStream,
        clientAddress,
        clientProcessName: None,
        clientProcessId: None,
        connectHost,
        targetHost,
        routePinned: false,
        targetPort,
        cancellation: session.client.cancellation.clone(),
        accountLease: accountLease.clone(),
    };
    let disposition = match session.client.tunnelInterceptor.as_ref() {
        Some(interceptor) => interceptor.intercept(tunnel).await?,
        None => TcpTunnelDisposition::Raw {
            tunnel: Box::new(tunnel),
            applicationProtocol: SessionApplicationProtocol::Tcp,
        },
    };
    let tunnel = match disposition {
        TcpTunnelDisposition::Handled(applicationProtocol) => {
            session.client.registry.update(
                &session.sessionId,
                SessionUpdate {
                    username: None,
                    command: None,
                    targetAddress: None,
                    applicationProtocol: Some(applicationProtocol),
                    state: SessionState::Relaying,
                },
            );
            return Ok(());
        }
        TcpTunnelDisposition::Failed {
            applicationProtocol,
            error,
        } => {
            // 应用层处理器已经拥有并关闭套接字，也已经记录精确失败事务；这里只补齐会话协议后
            // 传播失败，防止投影器把它当作未分类连接再次生成一条模糊或重复事务。
            session.client.registry.update(
                &session.sessionId,
                SessionUpdate {
                    username: None,
                    command: None,
                    targetAddress: None,
                    applicationProtocol: Some(applicationProtocol),
                    state: SessionState::Relaying,
                },
            );
            return Err(error.into());
        }
        TcpTunnelDisposition::Raw {
            tunnel,
            applicationProtocol,
        } => {
            session.client.registry.update(
                &session.sessionId,
                SessionUpdate {
                    username: None,
                    command: None,
                    targetAddress: None,
                    applicationProtocol: Some(applicationProtocol),
                    state: SessionState::Relaying,
                },
            );
            tunnel
        }
    };
    let pluginConnection = session
        .client
        .pluginHost
        .openConnection(ConnectionMetadata {
            transport: TransportKind::Tcp,
            clientAddress: tunnel.clientAddress.to_string(),
            // 插件匹配按主机和端口独立执行；此前传入含端口的展示地址会使 `streamMatch.hosts` 永远无法命中 SOCKS5 CONNECT 连接。
            targetHost: request.destination.hostString(),
            targetPort: request.destination.port,
        });
    relayBidirectional(
        tunnel.clientStream,
        tunnel.remoteStream,
        (config.relayBufferSize, config.idleTimeout()),
        RelaySession {
            registry: session.client.registry.clone(),
            sessionId: session.sessionId.clone(),
            pluginHost: session.client.pluginHost.clone(),
            pluginConnection,
            accountLease,
        },
    )
    .await?;
    Ok(())
}

/// 尝试目标解析结果直至连接成功；整体过程受同一连接时限约束。
async fn connectTarget(
    destination: &TargetAddress,
    config: &Socks5Config,
    addressOverride: Option<&dyn AddressOverride>,
    outboundConnector: Option<&transport_core::OutboundConnector>,
) -> Result<TcpStream> {
    // 进程内覆盖用于规则服务回环映射和显式 DNS 工具结果，必须先于二级代理连接器生效；
    // 否则客户端规则请求会从服务器再次访问公网地址并依赖 NAT hairpin。
    if let TargetHost::Domain(domain) = &destination.host
        && let Some(address) = addressOverride.and_then(|resolver| resolver.resolveIp(domain))
    {
        return timeout(
            config.connectTimeout(),
            TcpStream::connect(SocketAddr::new(address, destination.port)),
        )
        .await
        .map_err(|_| Socks5Error::Timeout("覆盖目标连接"))?
        .map_err(Socks5Error::Io);
    }
    if let Some(outboundConnector) = outboundConnector {
        return outboundConnector
            .connect(&destination.hostString(), destination.port)
            .await
            .map_err(|error| Socks5Error::RemoteConnect(error.to_string()));
    }
    timeout(config.connectTimeout(), async {
        let addresses = destination.resolveWithOverride(addressOverride).await?;
        let mut finalError = None;
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => finalError = Some(error),
            }
        }
        Err(Socks5Error::Io(finalError.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::HostUnreachable, "没有可连接的目标地址")
        })))
    })
    .await
    .map_err(|_| Socks5Error::Timeout("远端连接"))?
}

/// 创建会话私有 BIND 监听器并发送两阶段响应，接受目标后进入双向转发。
async fn runBind(
    stream: &mut TcpStream,
    request: SocksRequest,
    session: &SessionContext,
    accountLease: Option<AccountTrafficLease>,
) -> Result<()> {
    let config = &session.client.config;
    let controlLocal = stream.local_addr()?;
    let bindAddress = if controlLocal.is_ipv6() {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    };
    let listener = match TcpListener::bind(bindAddress).await {
        Ok(listener) => listener,
        Err(error) => {
            writeReply(stream, mapIoErrorToReply(&error), controlLocal).await?;
            return Err(Socks5Error::Io(error));
        }
    };
    let boundAddress = advertisedAddress(listener.local_addr()?, controlLocal);
    writeReply(stream, replySucceeded, boundAddress).await?;
    let accepted = timeout(config.bindTimeout(), listener.accept()).await;
    let (remoteStream, remotePeer) = match accepted {
        Ok(Ok(accepted)) => accepted,
        Ok(Err(error)) => {
            writeReply(stream, mapIoErrorToReply(&error), boundAddress).await?;
            return Err(Socks5Error::Io(error));
        }
        Err(_) => {
            writeReply(stream, replyTtlExpired, boundAddress).await?;
            return Err(Socks5Error::Timeout("BIND 等待"));
        }
    };
    let targetMatches = match timeout(
        config.readTimeout(),
        targetMatchesPeer(&request.destination, remotePeer),
    )
    .await
    {
        Ok(Ok(targetMatches)) => targetMatches,
        Ok(Err(error)) => {
            writeReply(stream, replyHostUnreachable, remotePeer).await?;
            return Err(error);
        }
        Err(_) => {
            writeReply(stream, replyTtlExpired, remotePeer).await?;
            return Err(Socks5Error::Timeout("BIND 目标校验"));
        }
    };
    if !targetMatches {
        writeReply(stream, replyConnectionNotAllowed, remotePeer).await?;
        return Err(Socks5Error::RemoteConnect(format!(
            "BIND 来源 {remotePeer} 与请求目标不一致"
        )));
    }
    writeReply(stream, replySucceeded, remotePeer).await?;
    session.client.registry.update(
        &session.sessionId,
        SessionUpdate {
            username: None,
            command: None,
            targetAddress: Some(remotePeer.to_string()),
            applicationProtocol: None,
            state: SessionState::Relaying,
        },
    );
    let pluginConnection = session
        .client
        .pluginHost
        .openConnection(ConnectionMetadata {
            transport: TransportKind::Tcp,
            clientAddress: stream.peer_addr()?.to_string(),
            targetHost: remotePeer.ip().to_string(),
            targetPort: remotePeer.port(),
        });
    relayBidirectional(
        stream,
        remoteStream,
        (config.relayBufferSize, config.idleTimeout()),
        RelaySession {
            registry: session.client.registry.clone(),
            sessionId: session.sessionId.clone(),
            pluginHost: session.client.pluginHost.clone(),
            pluginConnection,
            accountLease,
        },
    )
    .await?;
    Ok(())
}

/// 校验 BIND 对端符合请求的主机和可选端口；域名匹配全部解析地址。
async fn targetMatchesPeer(target: &TargetAddress, peer: SocketAddr) -> Result<bool> {
    if target.port != 0 && target.port != peer.port() {
        return Ok(false);
    }
    if target.isUnspecified() {
        return Ok(true);
    }
    match &target.host {
        TargetHost::Ip(address) => Ok(*address == peer.ip()),
        TargetHost::Domain(_) => Ok(target
            .resolve()
            .await?
            .iter()
            .any(|address| address.ip() == peer.ip())),
    }
}

/// 将未指定监听地址替换为控制连接实际本地 IP，使客户端获得可连接端点。
fn advertisedAddress(boundAddress: SocketAddr, controlLocal: SocketAddr) -> SocketAddr {
    if boundAddress.ip().is_unspecified() {
        SocketAddr::new(controlLocal.ip(), boundAddress.port())
    } else {
        boundAddress
    }
}
