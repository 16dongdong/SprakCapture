use std::time::Duration;

use plugin_host::{DataPlaneActionResult, PluginConnection, PluginHost, StreamDirection};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

use crate::{
    accountService::AccountTrafficLease,
    error::{Result, Socks5Error},
    model::TrafficDirection,
    registry::{ModifiedTraffic, SessionRegistry},
};

/// 聚合转发会话身份，两个方向共享同一注册表和稳定会话 ID。
pub struct RelaySession {
    pub registry: SessionRegistry,
    pub sessionId: String,
    pub pluginHost: PluginHost,
    pub pluginConnection: PluginConnection,
    pub accountLease: Option<AccountTrafficLease>,
}

/// 收拢单向转发所需的限额、会话、插件与账号租约，确保上下行构造时不会交换同类型位置参数。
struct PumpContext {
    bufferSize: usize,
    idleTimeout: Duration,
    registry: SessionRegistry,
    sessionId: String,
    direction: TrafficDirection,
    pluginHost: PluginHost,
    pluginConnection: PluginConnection,
    hookDirection: StreamDirection,
    accountLease: Option<AccountTrafficLease>,
}

/// 单向复制字节并执行写侧半关闭；每次成功写入后立即更新会话流量。
async fn pumpStream<R, W>(mut reader: R, mut writer: W, context: PumpContext) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let PumpContext {
        bufferSize,
        idleTimeout,
        registry,
        sessionId,
        direction,
        pluginHost,
        pluginConnection,
        hookDirection,
        accountLease,
    } = context;
    let mut buffer = vec![0_u8; bufferSize];
    let mut totalBytes = 0_u64;
    loop {
        let byteCount = timeout(idleTimeout, reader.read(&mut buffer))
            .await
            // 空闲超时发生在 SOCKS 成功响应之后，只表示回收长期无流量的已建立连接；
            // 使用独立错误类型，避免与认证、建连和协议读取超时混为同一个失败终态。
            .map_err(|_| Socks5Error::RelayIdleTimeout)??;
        if byteCount == 0 {
            writer.shutdown().await?;
            return Ok(totalBytes);
        }
        let originalBytes = buffer[..byteCount].to_vec();
        match pluginHost
            .processDataPlaneBytes(
                &pluginConnection,
                hookDirection,
                buffer[..byteCount].to_vec(),
            )
            .await
        {
            DataPlaneActionResult::Forward { bytes } => {
                if let Some(lease) = &accountLease {
                    lease.acquire(direction, bytes.len()).await?;
                }
                writer.write_all(&bytes).await?;
                writer.flush().await?;
                if let Some(lease) = &accountLease {
                    lease.record(direction, bytes.len());
                }
                totalBytes = totalBytes.saturating_add(bytes.len() as u64);
                registry.addModifiedTraffic(ModifiedTraffic {
                    sessionId: &sessionId,
                    direction,
                    originalPayload: &originalBytes,
                    payload: &bytes,
                });
            }
            DataPlaneActionResult::Hold | DataPlaneActionResult::Drop => continue,
            DataPlaneActionResult::Close => return Err(Socks5Error::PluginClosed),
        }
    }
}

/// 并行转发 TCP 双向数据；协议级错误保持失败，而已建立连接常见的半关闭或复位按正常结束处理。
/// Windows 移动客户端经常在响应完成后返回 10053，若继续记为失败会让已传输完整响应的事务产生红色假告警。
pub async fn relayBidirectional<C, R>(
    clientStream: C,
    remoteStream: R,
    options: (usize, Duration),
    session: RelaySession,
) -> Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    let RelaySession {
        registry,
        sessionId,
        pluginHost,
        pluginConnection,
        accountLease,
    } = session;
    let (clientReader, clientWriter) = tokio::io::split(clientStream);
    let (remoteReader, remoteWriter) = tokio::io::split(remoteStream);
    let (bufferSize, idleTimeout) = options;
    let upPump = pumpStream(
        clientReader,
        remoteWriter,
        PumpContext {
            bufferSize,
            idleTimeout,
            registry: registry.clone(),
            sessionId: sessionId.clone(),
            direction: TrafficDirection::Up,
            pluginHost: pluginHost.clone(),
            pluginConnection: pluginConnection.clone(),
            hookDirection: StreamDirection::ClientToServer,
            accountLease: accountLease.clone(),
        },
    );
    let downPump = pumpStream(
        remoteReader,
        clientWriter,
        PumpContext {
            bufferSize,
            idleTimeout,
            registry,
            sessionId,
            direction: TrafficDirection::Down,
            pluginHost: pluginHost.clone(),
            pluginConnection: pluginConnection.clone(),
            hookDirection: StreamDirection::ServerToClient,
            accountLease,
        },
    );
    let result = tokio::try_join!(upPump, downPump);
    pluginHost.closeDataPlaneConnection(pluginConnection).await;
    match result {
        Ok(_) => Ok(()),
        Err(error) if error.isNormalRelayTermination() => Ok(()),
        Err(error) => Err(error),
    }
}
