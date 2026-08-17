use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use plugin_host::{
    ConnectionMetadata, DataPlaneActionResult, PluginConnection, PluginHost, StreamDirection,
    TransportKind,
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpStream, UdpSocket},
    sync::mpsc,
    task::JoinSet,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    accountService::AccountTrafficLease,
    address::{AddressOverride, TargetAddress, TargetHost},
    config::Socks5Config,
    error::{Result, Socks5Error},
    model::TrafficDirection,
    protocol::{decodeUdpPacket, encodeUdpPacket},
    registry::{ModifiedTraffic, SessionRegistry},
};

/// 远端 UDP 接收任务交付给关联循环的原始数据报。
pub type RemoteDatagram = (Vec<u8>, SocketAddr);
/// 限制每个 UDP 关联等待转发的远端响应数量，防止慢客户端扩大内存占用。
pub const remoteResponseQueueCapacity: usize = 32;

/// 聚合 UDP 关联的网络与配置上下文，确保控制端点和绑定端点不会因元组顺序写反。
pub struct UdpAssociationContext {
    pub controlPeer: SocketAddr,
    pub controlLocal: SocketAddr,
    pub config: Socks5Config,
    pub addressOverride: Option<Arc<dyn AddressOverride>>,
}

/// 聚合 UDP 关联的会话状态与取消信号，所有转发分支共享同一指标归属。
pub struct UdpAssociationSession {
    pub registry: SessionRegistry,
    pub sessionId: String,
    pub cancellation: CancellationToken,
    pub pluginHost: PluginHost,
    pub clientAddress: String,
    pub accountLease: Option<AccountTrafficLease>,
}

/// 管理按地址族复用的远端 UDP Socket，并把回包汇聚到关联事件循环。
struct RemoteSocketPool {
    sockets: HashMap<bool, Arc<UdpSocket>>,
    receiverSender: mpsc::Sender<RemoteDatagram>,
    registry: SessionRegistry,
    receiverTasks: JoinSet<()>,
}

impl RemoteSocketPool {
    /// 创建空池；固定响应队列独立于远端地址上限，避免大数据报按地址数量放大内存。
    fn new(receiverSender: mpsc::Sender<RemoteDatagram>, registry: SessionRegistry) -> Self {
        Self {
            sockets: HashMap::new(),
            receiverSender,
            registry,
            receiverTasks: JoinSet::new(),
        }
    }

    /// 返回目标地址族的共享 Socket；绑定失败原样上报当前数据报。
    async fn getSocket(&mut self, ipv6: bool) -> Result<Arc<UdpSocket>> {
        if let Some(socket) = self.sockets.get(&ipv6) {
            return Ok(socket.clone());
        }
        let bindAddress = if ipv6 {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        };
        let socket = Arc::new(UdpSocket::bind(bindAddress).await?);
        let receiverSocket = socket.clone();
        let receiverSender = self.receiverSender.clone();
        let registry = self.registry.clone();
        self.receiverTasks.spawn(async move {
            let mut buffer = vec![0_u8; 65_507];
            while let Ok((byteCount, source)) = receiverSocket.recv_from(&mut buffer).await {
                // 满队列直接丢包并记账，网络接收任务不得等待控制循环而继续积压内核 Socket 缓冲区。
                if !queueRemoteDatagram(
                    &receiverSender,
                    &registry,
                    (buffer[..byteCount].to_vec(), source),
                ) {
                    break;
                }
            }
        });
        self.sockets.insert(ipv6, socket.clone());
        Ok(socket)
    }
}

/// 将远端数据报压入有界队列；队列满时计入丢包指标并继续接收，队列关闭时返回 false。
pub fn queueRemoteDatagram(
    sender: &mpsc::Sender<RemoteDatagram>,
    registry: &SessionRegistry,
    datagram: RemoteDatagram,
) -> bool {
    match sender.try_send(datagram) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            registry.recordDroppedUdpPacket();
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

/// 校验 UDP ASSOCIATE 请求声明的客户端地址属于控制连接来源。
async fn validateClientTarget(
    requestedClient: &TargetAddress,
    controlPeer: SocketAddr,
) -> Result<()> {
    if requestedClient.isUnspecified() {
        return Ok(());
    }
    let valid = match &requestedClient.host {
        TargetHost::Ip(address) => *address == controlPeer.ip(),
        TargetHost::Domain(_) => requestedClient
            .resolve()
            .await?
            .iter()
            .any(|address| address.ip() == controlPeer.ip()),
    };
    if valid {
        Ok(())
    } else {
        Err(Socks5Error::InvalidUdpSource)
    }
}

/// 返回能被控制连接客户端访问的 UDP 绑定地址，未指定 IP 替换为 TCP 本地 IP。
fn advertisedAddress(boundAddress: SocketAddr, controlLocal: SocketAddr) -> SocketAddr {
    if boundAddress.ip().is_unspecified() {
        SocketAddr::new(controlLocal.ip(), boundAddress.port())
    } else {
        boundAddress
    }
}

/// 校验控制端声明并创建关联私有 UDP 端点；成功回复必须在绑定完成后发送，失败不返回伪造端点。
async fn bindAssociationSocket(
    controlStream: &mut TcpStream,
    requestedClient: &TargetAddress,
    context: &UdpAssociationContext,
) -> Result<UdpSocket> {
    timeout(
        context.config.connectTimeout(),
        validateClientTarget(requestedClient, context.controlPeer),
    )
    .await
    .map_err(|_| Socks5Error::Timeout("UDP 客户端地址校验"))??;
    let bindIp = if context.config.udpBindHost.is_empty() {
        if context.controlPeer.is_ipv6() {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        }
    } else {
        context
            .config
            .udpBindHost
            .parse()
            .map_err(|_| Socks5Error::Configuration("udpBindHost 无效".to_owned()))?
    };
    let clientSocket = UdpSocket::bind(SocketAddr::new(bindIp, 0)).await?;
    let boundAddress = advertisedAddress(clientSocket.local_addr()?, context.controlLocal);
    crate::protocol::writeReply(controlStream, crate::protocol::replySucceeded, boundAddress)
        .await?;
    Ok(clientSocket)
}

/// 关闭关联创建的全部插件连接；逐个等待让插件能按连接维度提交最终状态。
async fn closePluginConnections(
    pluginHost: &PluginHost,
    pluginConnections: HashMap<SocketAddr, PluginConnection>,
) {
    for connection in pluginConnections.into_values() {
        pluginHost.closeDataPlaneConnection(connection).await;
    }
}

/// 运行单个 UDP 关联；控制 TCP EOF、取消信号或传输错误都会释放关联私有资源。
pub async fn runUdpAssociation(
    controlStream: &mut TcpStream,
    requestedClient: TargetAddress,
    context: UdpAssociationContext,
    session: UdpAssociationSession,
) -> Result<()> {
    let clientSocket = bindAssociationSocket(controlStream, &requestedClient, &context).await?;
    let UdpAssociationContext {
        controlPeer,
        controlLocal: _,
        config,
        addressOverride,
    } = context;
    let UdpAssociationSession {
        registry,
        sessionId,
        cancellation,
        pluginHost,
        clientAddress: controlClientAddress,
        accountLease,
    } = session;
    let (remoteSender, mut remoteReceiver) =
        mpsc::channel::<RemoteDatagram>(remoteResponseQueueCapacity);
    let mut remotePool = RemoteSocketPool::new(remoteSender, registry.clone());
    let mut allowedRemoteAddresses = HashSet::<SocketAddr>::new();
    let mut clientAddress: Option<SocketAddr> = None;
    let mut clientBuffer = vec![0_u8; config.udpMaxPacketSize.saturating_add(1)];
    let mut controlBuffer = [0_u8; 1_024];
    let mut pluginConnections = HashMap::<SocketAddr, PluginConnection>::new();

    let result = loop {
        tokio::select! {
            _ = cancellation.cancelled() => break Ok(()),
            controlRead = controlStream.read(&mut controlBuffer) => {
                match controlRead {
                    Ok(0) => break Ok(()),
                    Ok(_) => continue,
                    Err(error) => break Err(Socks5Error::Io(error)),
                }
            }
            received = clientSocket.recv_from(&mut clientBuffer) => {
                let (byteCount, source) = received?;
                if byteCount > config.udpMaxPacketSize {
                    registry.recordDroppedUdpPacket();
                    continue;
                }
                if source.ip() != controlPeer.ip()
                    || (requestedClient.port != 0 && source.port() != requestedClient.port)
                    || clientAddress.is_some_and(|locked| locked != source)
                {
                    registry.recordDroppedUdpPacket();
                    continue;
                }
                let mut packet = match decodeUdpPacket(&clientBuffer[..byteCount]) {
                    Ok(packet) => packet,
                    Err(_) => {
                        registry.recordDroppedUdpPacket();
                        continue;
                    }
                };
                let targetAddresses = match timeout(
                    config.connectTimeout(),
                    packet
                        .destination
                        .resolveWithOverride(addressOverride.as_deref()),
                ).await {
                    Ok(Ok(addresses)) => addresses,
                    Ok(Err(_)) | Err(_) => {
                        registry.recordDroppedUdpPacket();
                        continue;
                    }
                };
                let Some(targetAddress) = targetAddresses.first().copied() else {
                    continue;
                };
                if allowedRemoteAddresses.len() >= config.udpRemoteLimit
                    && !allowedRemoteAddresses.contains(&targetAddress)
                {
                    registry.recordDroppedUdpPacket();
                    continue;
                }
                let connection = pluginConnections.entry(targetAddress).or_insert_with(|| {
                    pluginHost.openConnection(ConnectionMetadata {
                        transport: TransportKind::Udp,
                        clientAddress: controlClientAddress.clone(),
                        // UDP 与 TCP 共用 host/port 分离的插件契约，避免同一清单在 UDP 数据报路径上失配。
                        targetHost: packet.destination.hostString(),
                        targetPort: packet.destination.port,
                    })
                });
                let originalPayload = packet.payload.clone();
                match pluginHost
                    .processDataPlaneBytes(
                        connection,
                        StreamDirection::ClientToServer,
                        packet.payload,
                    )
                    .await
                {
                    DataPlaneActionResult::Forward { bytes } => packet.payload = bytes,
                    DataPlaneActionResult::Drop | DataPlaneActionResult::Hold => {
                        registry.recordDroppedUdpPacket();
                        continue;
                    }
                    DataPlaneActionResult::Close => break Ok(()),
                }
                // 账号限速是流量策略而不是网络建立时限，必须在 connectTimeout 外等待，避免低带宽被误判丢包。
                if let Some(lease) = &accountLease {
                    lease.acquire(TrafficDirection::Up, packet.payload.len()).await?;
                }
                let sendResult = timeout(config.connectTimeout(), async {
                    let remoteSocket = remotePool.getSocket(targetAddress.is_ipv6()).await?;
                    remoteSocket.send_to(&packet.payload, targetAddress).await?;
                    if let Some(lease) = &accountLease {
                        lease.record(TrafficDirection::Up, packet.payload.len());
                    }
                    Ok::<(), Socks5Error>(())
                }).await;
                if !matches!(sendResult, Ok(Ok(()))) {
                    registry.recordDroppedUdpPacket();
                    continue;
                }
                if allowedRemoteAddresses.is_empty() {
                    // UDP ASSOCIATE 控制请求携带的是客户端端点；首个成功数据报才是可用于事务树的真实目标。
                    registry.update(
                        &sessionId,
                        crate::registry::SessionUpdate {
                            username: None,
                            command: None,
                            targetAddress: Some(packet.destination.toString()),
                            applicationProtocol: None,
                            state: crate::model::SessionState::UdpAssociating,
                        },
                    );
                }
                allowedRemoteAddresses.insert(targetAddress);
                clientAddress = Some(source);
                registry.addModifiedTraffic(ModifiedTraffic {
                    sessionId: &sessionId,
                    direction: TrafficDirection::Up,
                    originalPayload: &originalPayload,
                    payload: &packet.payload,
                });
                registry.recordUdpPacket(TrafficDirection::Up);
            }
            remoteDatagram = remoteReceiver.recv() => {
                let Some((payload, source)) = remoteDatagram else {
                    break Err(Socks5Error::Runtime("UDP 远端接收通道关闭".to_owned()));
                };
                let Some(clientAddress) = clientAddress else {
                    continue;
                };
                if !allowedRemoteAddresses.contains(&source) {
                    registry.recordDroppedUdpPacket();
                    continue;
                }
                let Some(connection) = pluginConnections.get(&source) else {
                    continue;
                };
                let originalPayload = payload;
                let payload = match pluginHost
                    .processDataPlaneBytes(
                        connection,
                        StreamDirection::ServerToClient,
                        originalPayload.clone(),
                    )
                    .await
                {
                    DataPlaneActionResult::Forward { bytes } => bytes,
                    DataPlaneActionResult::Drop | DataPlaneActionResult::Hold => {
                        registry.recordDroppedUdpPacket();
                        continue;
                    }
                    DataPlaneActionResult::Close => break Ok(()),
                };
                let responsePacket = encodeUdpPacket(source, &payload)?;
                if responsePacket.len() > config.udpMaxPacketSize {
                    registry.recordDroppedUdpPacket();
                    continue;
                }
                if let Some(lease) = &accountLease {
                    lease.acquire(TrafficDirection::Down, payload.len()).await?;
                }
                clientSocket.send_to(&responsePacket, clientAddress).await?;
                if let Some(lease) = &accountLease {
                    lease.record(TrafficDirection::Down, payload.len());
                }
                registry.addModifiedTraffic(ModifiedTraffic {
                    sessionId: &sessionId,
                    direction: TrafficDirection::Down,
                    originalPayload: &originalPayload,
                    payload: &payload,
                });
                registry.recordUdpPacket(TrafficDirection::Down);
            }
        }
    };
    closePluginConnections(&pluginHost, pluginConnections).await;
    result
}
