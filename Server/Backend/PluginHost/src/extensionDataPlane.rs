//! 把完整扩展内核接入现有 TCP/UDP 字节热路径。
//!
//! legacy Native ABI 与完整 Mod 可以同时工作：legacy 先处理原始缓冲，完整 Mod 再收到统一事件信封。
//! 完整 Mod 使用 JSON 字节数组是跨 Native、Sidecar、Worker 与 Wasm 的稳定交换格式；最终写线前统一还原为
//! `Vec<u8>`，不会让运行时私有内存或指针越过 ABI 生命周期。

use serde_json::{Value as JsonValue, json};

use crate::{
    ActionKind, EventEnvelope, HookActionResult, InterceptionMode, PacketFilterResult,
    PluginConnection, PluginHost, Stage, StageContext, StreamDirection, TransportKind,
};

const EXTENSION_API_VERSION: &str = "2.0.0";

/// 描述 legacy 与完整 Mod 共同处理后的最终数据面动作；转发分支始终拥有完整输出字节。
#[derive(Debug, Eq, PartialEq)]
pub enum DataPlaneActionResult {
    Forward { bytes: Vec<u8> },
    Hold,
    Drop,
    Close,
}

/// 描述最终写线字节相对读取原文的一段变化；偏移以修改后正文为坐标，支持等长替换和变长重封包。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireByteModification {
    pub offsetBytes: usize,
    pub originalBytes: Vec<u8>,
    pub modifiedBytes: Vec<u8>,
}

/// 在线性时间内提取写线差异；等长正文按连续差异拆段，变长正文保留公共前后缀并记录中间替换。
///
/// 运行上下文：调用方已取得读取原文和插件/WPE 最终输出，结果只用于录制元数据，不进入网络热路径。
/// 失败语义：相同正文返回空集合；函数不执行 I/O，也不会修改输入缓冲。
pub fn deriveWireByteModifications(original: &[u8], modified: &[u8]) -> Vec<WireByteModification> {
    if original == modified {
        return Vec::new();
    }
    if original.len() == modified.len() {
        let mut modifications = Vec::new();
        let mut offset = 0;
        while offset < original.len() {
            if original[offset] == modified[offset] {
                offset += 1;
                continue;
            }
            let start = offset;
            while offset < original.len() && original[offset] != modified[offset] {
                offset += 1;
            }
            modifications.push(WireByteModification {
                offsetBytes: start,
                originalBytes: original[start..offset].to_vec(),
                modifiedBytes: modified[start..offset].to_vec(),
            });
        }
        return modifications;
    }
    let prefixBytes = original
        .iter()
        .zip(modified)
        .take_while(|(left, right)| left == right)
        .count();
    let maximumSuffixBytes = original
        .len()
        .saturating_sub(prefixBytes)
        .min(modified.len().saturating_sub(prefixBytes));
    let suffixBytes = original
        .iter()
        .rev()
        .zip(modified.iter().rev())
        .take(maximumSuffixBytes)
        .take_while(|(left, right)| left == right)
        .count();
    vec![WireByteModification {
        offsetBytes: prefixBytes,
        originalBytes: original[prefixBytes..original.len() - suffixBytes].to_vec(),
        modifiedBytes: modified[prefixBytes..modified.len() - suffixBytes].to_vec(),
    }]
}

impl PluginHost {
    /// 顺序执行 legacy Hook 与完整 Mod 阶段，并返回可直接写入对端的独占字节。
    ///
    /// 运行上下文：SOCKS、HTTP CONNECT 与 UDP relay 在每次收到完整线上块后调用；同一连接方向由调用方
    /// 保持原有顺序。`bytes` 是当前真实线上块，插件可以修改、暂存、丢弃或关闭连接。
    /// 失败语义：运行时错误、非法字节输出或失效代际统一返回 `Close`；透明且无匹配插件时返回原字节。
    pub async fn processDataPlaneBytes(
        &self,
        connection: &PluginConnection,
        direction: StreamDirection,
        mut bytes: Vec<u8>,
    ) -> DataPlaneActionResult {
        let legacyResult = self.processStreamData(connection, direction, bytes.as_mut_slice());
        bytes = match legacyResult {
            HookActionResult::Forward { length } => {
                bytes.truncate(length);
                bytes
            }
            HookActionResult::ForwardOwned { bytes } => bytes,
            HookActionResult::Hold => return DataPlaneActionResult::Hold,
            HookActionResult::Drop => return DataPlaneActionResult::Drop,
            HookActionResult::Close => return DataPlaneActionResult::Close,
        };

        let stage = match connection.transport {
            TransportKind::Tcp => Stage::TcpChunk,
            TransportKind::Udp => Stage::UdpDatagram,
        };
        if !self.inner.extensionKernel.hasSubscriptions(stage) {
            return self.processFinalWireBytes(&connection.metadata, direction, bytes);
        }
        let (serviceGeneration, recordingGeneration) =
            self.inner.extensionKernel.currentGenerations();
        let eventSequence = self
            .inner
            .nextExtensionEventId
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let eventId = format!("{}:{eventSequence}", connection.connectionId);
        let payload = json!({
            "bytes": bytes,
            "endOfStream": false,
        });
        let envelope = EventEnvelope {
            apiVersion: EXTENSION_API_VERSION.to_owned(),
            eventId,
            stage,
            serviceGeneration,
            recordingGeneration,
            pluginInstanceId: String::new(),
            connectionId: Some(connection.connectionId.to_string()),
            transactionId: None,
            // 开放可信 Mod 不由宿主施加事件截止时间；最大值明确表示该字段只供插件作者参考。
            deadlineUnixMs: u64::MAX,
            context: stageContext(connection, direction),
            payload,
        };
        let dispatch = match self.inner.extensionKernel.dispatch(envelope).await {
            Ok(dispatch) => dispatch,
            Err(_) => return DataPlaneActionResult::Close,
        };
        match dispatch.terminalAction {
            Some(ActionKind::Hold) => DataPlaneActionResult::Hold,
            Some(ActionKind::Drop | ActionKind::Reject) => DataPlaneActionResult::Drop,
            Some(ActionKind::Close) => DataPlaneActionResult::Close,
            _ => decodeWireBytes(&dispatch.finalPayload)
                .map(|bytes| self.processFinalWireBytes(&connection.metadata, direction, bytes))
                .unwrap_or(DataPlaneActionResult::Close),
        }
    }

    /// 在全部协议处理与插件重封包后执行声明式滤镜；SOCKS5 与 WinDivert 适配器必须共同调用这个最终写线边界。
    ///
    /// 运行上下文：异步代理路径在插件链完成后调用，WinDivert resolver 在校验和重算前同步调用；`metadata` 必须描述真实目标而非内部回环端点。
    /// 失败语义：规则只产生转发、丢弃或关闭决定，不执行网络 I/O；物理适配器负责把决定落实到 Socket 或 WinDivert 回注。
    pub fn processFinalWireBytes(
        &self,
        metadata: &crate::ConnectionMetadata,
        direction: StreamDirection,
        bytes: Vec<u8>,
    ) -> DataPlaneActionResult {
        match self.inner.packetFilters.process(metadata, direction, bytes) {
            PacketFilterResult::Forward { bytes } => DataPlaneActionResult::Forward { bytes },
            PacketFilterResult::Drop => DataPlaneActionResult::Drop,
            PacketFilterResult::Close => DataPlaneActionResult::Close,
        }
    }

    /// 发布连接关闭阶段并清理 legacy 与完整 Mod 的连接级状态。
    ///
    /// 运行上下文：双向 relay 已停止全部网络读写后调用；`connection` 在返回时被消费，禁止再次调度。
    /// 失败语义：关闭通知失败只结束该通知链，不阻止宿主释放 legacy 连接注册和半包缓冲。
    pub async fn closeDataPlaneConnection(&self, connection: PluginConnection) {
        if self
            .inner
            .extensionKernel
            .hasSubscriptions(Stage::ConnectionClosing)
        {
            let (serviceGeneration, recordingGeneration) =
                self.inner.extensionKernel.currentGenerations();
            let eventSequence = self
                .inner
                .nextExtensionEventId
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let envelope = EventEnvelope {
                apiVersion: EXTENSION_API_VERSION.to_owned(),
                eventId: format!("{}:{eventSequence}", connection.connectionId),
                stage: Stage::ConnectionClosing,
                serviceGeneration,
                recordingGeneration,
                pluginInstanceId: String::new(),
                connectionId: Some(connection.connectionId.to_string()),
                transactionId: None,
                deadlineUnixMs: u64::MAX,
                context: stageContext(&connection, StreamDirection::ClientToServer),
                payload: json!({}),
            };
            let _ = self.inner.extensionKernel.dispatch(envelope).await;
        }
        self.closeConnection(connection);
    }
}

/// 构造 TCP/UDP 块共享的连接上下文；目标地址和传输方向来自连接建立时的不可变快照。
///
/// 参数：`connection` 是当前插件连接，`direction` 是本次线上块方向。
/// 失败语义：本函数不执行 I/O；缺少进程信息时保留 `None`，不伪造进程身份。
fn stageContext(connection: &PluginConnection, direction: StreamDirection) -> StageContext {
    StageContext {
        transport: Some(
            match connection.transport {
                TransportKind::Tcp => "tcp",
                TransportKind::Udp => "udp",
            }
            .to_owned(),
        ),
        protocol: Some(
            match connection.transport {
                TransportKind::Tcp => "tcp",
                TransportKind::Udp => "udp",
            }
            .to_owned(),
        ),
        direction: Some(
            match direction {
                StreamDirection::ClientToServer => "up",
                StreamDirection::ServerToClient => "down",
            }
            .to_owned(),
        ),
        host: Some(connection.metadata.targetHost.clone()),
        address: Some(connection.metadata.clientAddress.clone()),
        port: Some(connection.metadata.targetPort),
        interceptionMode: InterceptionMode::Intercept,
        ..StageContext::default()
    }
}

/// 从完整 Mod 的最终阶段视图提取线上字节；所有元素必须是 0..=255 的整数。
///
/// 运行上下文：只在全部插件动作已按顺序应用后调用，因此任何失败都发生在写线之前。
/// 失败语义：缺少 `bytes`、非数组、浮点数或越界整数返回 `None`，调用方关闭当前连接而不发送坏包。
fn decodeWireBytes(payload: &JsonValue) -> Option<Vec<u8>> {
    payload
        .get("bytes")?
        .as_array()?
        .iter()
        .map(|value| value.as_u64().and_then(|byte| u8::try_from(byte).ok()))
        .collect()
}
