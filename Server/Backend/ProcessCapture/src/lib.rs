//! 基于 WinDivert 的指定进程 TCP/UDP 双栈捕获核心。
//!
//! WinDivert 的 `NETWORK` 层不提供进程编号，因此模块先在 `SOCKET` 与 `FLOW`
//! 层关联进程和五元组，再在 `NETWORK` 层完成 TCP 双向地址反射与 UDP 数据报统一处理。
//! TCP 代理监听端接受到的连接
//! 保留原目标地址这一身份，同时可通过 [`ProcessCapture::originalTargetForPeer`] 取得
//! 原始目标端口，供单端口代理选择透明转发路径。

#![allow(non_snake_case, non_upper_case_globals)]

mod flowTable;
mod packetRewrite;
#[doc(hidden)]
pub mod udpFragment;

#[cfg(windows)]
mod connectionReset;

#[cfg(not(windows))]
mod platformStub;
#[cfg(windows)]
mod windowsRuntime;

pub use flowTable::{CaptureFlow, CaptureFlowTable, NetworkInterface, OriginalTarget};
pub(crate) use packetRewrite::isTcpStartPacket;
pub use packetRewrite::{PacketDirection, PacketRewriteError, rewriteTcpPacket};
#[cfg(not(windows))]
pub use platformStub::ProcessCapture;
#[cfg(windows)]
pub use windowsRuntime::ProcessCapture;

use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 定义指定进程捕获的稳定配置；代理端口必须由融合监听器实际占用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessCaptureConfiguration {
    pub enabled: bool,
    pub processIds: BTreeSet<u32>,
    pub proxyPort: u16,
    /// 融合监听器的实际绑定地址仅由服务端控制面注入，不属于公开配置契约。
    ///
    /// 透明改写必须把目标地址改为监听器可接受的地址；否则监听在回环地址时，
    /// 沿用原网卡地址会导致本机 TCP 栈找不到对应监听套接字。
    #[serde(skip, default = "defaultProxyAddress")]
    pub proxyAddress: IpAddr,
}

impl Default for ProcessCaptureConfiguration {
    /// 默认关闭捕获，避免服务启动时在没有明确目标进程的情况下加载驱动。
    fn default() -> Self {
        Self {
            enabled: false,
            processIds: BTreeSet::new(),
            proxyPort: 1080,
            proxyAddress: defaultProxyAddress(),
        }
    }
}

/// 返回进程捕获的默认融合监听地址；公开配置反序列化时由控制面随后覆盖此值。
fn defaultProxyAddress() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

impl ProcessCaptureConfiguration {
    /// 校验控制面输入并排除代理服务自身；失败时不创建任何 WinDivert 句柄。
    pub fn validate(&self, proxyProcessId: u32) -> Result<(), ProcessCaptureError> {
        if self.proxyPort == 0 {
            return Err(ProcessCaptureError::InvalidProxyPort);
        }
        // 进程路径可能已保存但当前实例尚未启动；空 PID 集仍需保持 WinDivert 与内部监听器就绪，
        // 使后台路径监视器能在新实例出现时原子加入，而不是重启整个代理数据面。
        if self.processIds.contains(&0) {
            return Err(ProcessCaptureError::InvalidProcessId);
        }
        if self.processIds.contains(&proxyProcessId) {
            return Err(ProcessCaptureError::ProxyProcessSelected(proxyProcessId));
        }
        Ok(())
    }
}

/// 暴露捕获运行状态；计数只描述已确认的五元组，不包含未命中的系统流量。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessCaptureSnapshot {
    pub running: bool,
    pub configuredProcessIds: Vec<u32>,
    pub trackedFlows: usize,
    pub acceptedConnections: u64,
    pub redirectedPackets: u64,
    pub restoredPackets: u64,
    pub bytesUp: u64,
    pub bytesDown: u64,
    pub lastError: Option<String>,
}

/// 区分 WinDivert 已成功回注的 UDP 数据报方向；事件正文始终是 UDP payload，不包含 IP/UDP 头。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UdpDatagramDirection {
    Up,
    Down,
}

/// 向录制层发布一份已成功转发的 UDP 数据报；地址和 PID 均来自同一精确 FLOW 五元组。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UdpDatagramEvent {
    pub processId: u32,
    pub clientAddress: SocketAddr,
    pub targetAddress: SocketAddr,
    pub direction: UdpDatagramDirection,
    pub payload: Vec<u8>,
    pub capturedAtMilliseconds: u64,
    pub modifications: Vec<UdpDatagramModification>,
}

/// 描述 WinDivert UDP 最终写线正文中的一段插件变化；偏移使用修改后正文坐标。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UdpDatagramModification {
    pub offsetBytes: usize,
    pub originalBytes: Vec<u8>,
    pub modifiedBytes: Vec<u8>,
}

/// 表示统一封包数据面对一个 WinDivert UDP 数据报作出的最终写线决定。
///
/// `Forward` 必须保持正文长度不变，驱动层据此原位更新 UDP payload 并重算校验和；
/// `Drop` 与 `Close` 对无连接 UDP 都表示不回注当前数据报，区别仅保留给上层规则语义。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UdpDatagramDecision {
    Forward {
        payload: Vec<u8>,
        modifications: Vec<UdpDatagramModification>,
    },
    Drop,
    Close,
}

/// 定义 SOCKS5 与 WinDivert 共用的最终封包处理边界。
///
/// 运行上下文：WinDivert resolver 已经完成 PID、方向与原目标解析，但尚未回注真实数据包；
/// 实现只能做内存计算，不得执行阻塞网络或磁盘 I/O。返回变长正文会被驱动层作为契约错误停止捕获，
/// 防止 IP/UDP 长度和分片边界被静默破坏。
pub trait UdpDatagramProcessor: Send + Sync {
    fn process(&self, event: &UdpDatagramEvent) -> Result<UdpDatagramDecision, String>;
}

/// 共享封包处理器由控制面创建，配置热更新时所有 WinDivert 工作线程读取同一规则快照。
pub type SharedUdpDatagramProcessor = Arc<dyn UdpDatagramProcessor>;

/// 接收 WinDivert 已完成统一封包处理并成功回注的 UDP 数据报顺序持久化边界。
///
/// 实现必须使用固定容量 FIFO 接受事件并保证调用顺序稳定；容量不足返回错误且保留当前事件，
/// 驱动线程会显式停止捕获且排空已拦截原包。该契约禁止阻塞 I/O 和无界堆队列进入收包线程。
pub trait UdpDatagramSink: Send + Sync {
    fn append(&self, event: UdpDatagramEvent) -> Result<(), String>;

    /// 返回异步 writer 或录制消费者的首个持久化故障，供运行快照立即暴露。
    fn fault(&self) -> Option<String>;
}

/// 共享录制落点由控制面创建并跨 WinDivert 工作线程复用。
pub type SharedUdpDatagramSink = Arc<dyn UdpDatagramSink>;

/// 汇总配置、驱动加载、事件线程和数据包解析失败，调用方据此返回精确控制面错误。
#[derive(Debug, Error)]
pub enum ProcessCaptureError {
    #[error("WinDivert 进程捕获尚未运行")]
    NotRunning,
    #[error("进程编号必须位于 1..=4294967295")]
    InvalidProcessId,
    #[error("透明代理端口必须位于 1..=65535")]
    InvalidProxyPort,
    #[error("代理服务进程 {0} 不得加入捕获目标")]
    ProxyProcessSelected(u32),
    #[error("当前平台不支持 WinDivert 进程捕获")]
    UnsupportedPlatform,
    #[error("打开 WinDivert {layer} 层失败：{detail}")]
    OpenDriver { layer: &'static str, detail: String },
    #[error("WinDivert {worker} 工作线程失败：{detail}")]
    Worker {
        worker: &'static str,
        detail: String,
    },
    #[error("等待 WinDivert 工作线程退出失败")]
    WorkerPanicked,
    #[error("读取 {addressFamily} TCP 所有者表失败，系统状态码：{status}")]
    EnumerateConnections {
        addressFamily: &'static str,
        status: u32,
    },
    #[error("{0} TCP 所有者表长度无效")]
    InvalidConnectionTable(&'static str),
    #[error("重建进程 {processId} 的既有 {addressFamily} TCP 连接失败，系统状态码：{status}")]
    ResetConnection {
        addressFamily: &'static str,
        processId: u32,
        status: u32,
    },
}

/// 把 IPv4 映射 IPv6 地址规范化为 IPv4，确保 SOCKET/FLOW 与 NETWORK 使用同一键。
pub(crate) fn normalizeIpAddress(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(address)),
        address => address,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessCaptureConfiguration, ProcessCaptureError};
    use std::collections::BTreeSet;

    /// 校验 PID 0 在启用与停用配置中都被拒绝，防止非法目标渗入后续 WinDivert 过滤器。
    #[test]
    fn rejectsZeroProcessId() {
        for enabled in [false, true] {
            let configuration = ProcessCaptureConfiguration {
                enabled,
                processIds: BTreeSet::from([0]),
                ..ProcessCaptureConfiguration::default()
            };
            assert!(matches!(
                configuration.validate(u32::MAX),
                Err(ProcessCaptureError::InvalidProcessId)
            ));
        }
    }
}
