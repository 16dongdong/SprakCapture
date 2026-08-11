//! 把 WinDivert 已解析数据报接入与 SOCKS5 相同的最终写线入口。
//!
//! 物理收发仍由各自传输适配器负责：SOCKS5 写 `UdpSocket`，WinDivert 重算校验和后回注。
//! 两条入口在写线前都调用 `PluginHost::processFinalWireBytes`，因此热更新、顺序和动作语义只有一份。

use plugin_host::{
    ConnectionMetadata, DataPlaneActionResult, PluginHost, StreamDirection, TransportKind,
};
use process_capture_core::{
    UdpDatagramDecision, UdpDatagramDirection, UdpDatagramEvent, UdpDatagramProcessor,
};

/// 将进程捕获元数据映射为插件宿主通用连接元数据，并执行共享封包滤镜。
pub(crate) struct UnifiedPacketFilterProcessor {
    pluginHost: PluginHost,
}

impl UnifiedPacketFilterProcessor {
    /// 创建不复制规则内容的处理器；配置更新会由所有 SOCKS5/WinDivert 调用方立即观察到。
    pub(crate) fn new(pluginHost: PluginHost) -> Self {
        Self { pluginHost }
    }
}

impl UdpDatagramProcessor for UnifiedPacketFilterProcessor {
    /// 在 WinDivert 回注前执行与 SOCKS5 完全相同的有序规则，并保留原始目标和业务方向。
    fn process(&self, event: &UdpDatagramEvent) -> Result<UdpDatagramDecision, String> {
        let metadata = ConnectionMetadata {
            transport: TransportKind::Udp,
            clientAddress: event.clientAddress.to_string(),
            targetHost: event.targetAddress.ip().to_string(),
            targetPort: event.targetAddress.port(),
        };
        let direction = match event.direction {
            UdpDatagramDirection::Up => StreamDirection::ClientToServer,
            UdpDatagramDirection::Down => StreamDirection::ServerToClient,
        };
        Ok(
            match self
                .pluginHost
                .processFinalWireBytes(&metadata, direction, event.payload.clone())
            {
                DataPlaneActionResult::Forward { bytes } => UdpDatagramDecision::Forward {
                    modifications: plugin_host::deriveWireByteModifications(&event.payload, &bytes)
                        .into_iter()
                        .map(
                            |modification| process_capture_core::UdpDatagramModification {
                                offsetBytes: modification.offsetBytes,
                                originalBytes: modification.originalBytes,
                                modifiedBytes: modification.modifiedBytes,
                            },
                        )
                        .collect(),
                    payload: bytes,
                },
                DataPlaneActionResult::Drop | DataPlaneActionResult::Hold => {
                    UdpDatagramDecision::Drop
                }
                DataPlaneActionResult::Close => UdpDatagramDecision::Close,
            },
        )
    }
}
