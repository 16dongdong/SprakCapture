use std::time::Duration;

use plugin_host::{DataPlaneActionResult, PluginConnection, PluginHost, StreamDirection};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

use crate::{
    error::{Result, Socks5Error},
    model::TrafficDirection,
    registry::SessionRegistry,
};

/// 聚合转发会话身份，两个方向共享同一注册表和稳定会话 ID。
pub struct RelaySession {
    pub registry: SessionRegistry,
    pub sessionId: String,
    pub pluginHost: PluginHost,
    pub pluginConnection: PluginConnection,
}

/// 单向复制字节并执行写侧半关闭；每次成功写入后立即更新会话流量。
async fn pumpStream<R, W>(
    mut reader: R,
    mut writer: W,
    options: (usize, Duration),
    traffic: (SessionRegistry, String, TrafficDirection),
    hook: (PluginHost, PluginConnection, StreamDirection),
) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (bufferSize, idleTimeout) = options;
    let (registry, sessionId, direction) = traffic;
    let (pluginHost, pluginConnection, hookDirection) = hook;
    let mut buffer = vec![0_u8; bufferSize];
    let mut totalBytes = 0_u64;
    loop {
        let byteCount = timeout(idleTimeout, reader.read(&mut buffer))
            .await
            .map_err(|_| Socks5Error::Timeout("TCP 转发空闲"))??;
        if byteCount == 0 {
            writer.shutdown().await?;
            return Ok(totalBytes);
        }
        match pluginHost
            .processDataPlaneBytes(
                &pluginConnection,
                hookDirection,
                buffer[..byteCount].to_vec(),
            )
            .await
        {
            DataPlaneActionResult::Forward { bytes } => {
                writer.write_all(&bytes).await?;
                writer.flush().await?;
                totalBytes = totalBytes.saturating_add(bytes.len() as u64);
                registry.addTraffic(&sessionId, direction, &bytes);
            }
            DataPlaneActionResult::Hold | DataPlaneActionResult::Drop => continue,
            DataPlaneActionResult::Close => return Err(Socks5Error::PluginClosed),
        }
    }
}

/// 并行转发 TCP 双向数据；任一方向失败会取消另一方向并由所有权析构关闭两端。
pub async fn relayBidirectional<C, R>(
    clientStream: C,
    remoteStream: R,
    options: (usize, Duration),
    session: RelaySession,
) -> Result<(u64, u64)>
where
    C: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    let RelaySession {
        registry,
        sessionId,
        pluginHost,
        pluginConnection,
    } = session;
    let (clientReader, clientWriter) = tokio::io::split(clientStream);
    let (remoteReader, remoteWriter) = tokio::io::split(remoteStream);
    let upPump = pumpStream(
        clientReader,
        remoteWriter,
        options,
        (registry.clone(), sessionId.clone(), TrafficDirection::Up),
        (
            pluginHost.clone(),
            pluginConnection.clone(),
            StreamDirection::ClientToServer,
        ),
    );
    let downPump = pumpStream(
        remoteReader,
        clientWriter,
        options,
        (registry, sessionId, TrafficDirection::Down),
        (
            pluginHost.clone(),
            pluginConnection.clone(),
            StreamDirection::ServerToClient,
        ),
    );
    let result = tokio::try_join!(upPump, downPump);
    pluginHost.closeDataPlaneConnection(pluginConnection).await;
    result
}
