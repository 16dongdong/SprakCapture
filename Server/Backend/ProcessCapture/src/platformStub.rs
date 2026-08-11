use std::net::{IpAddr, SocketAddr};

use crate::{
    CaptureFlowTable, OriginalTarget, ProcessCaptureConfiguration, ProcessCaptureError,
    ProcessCaptureSnapshot, SharedUdpDatagramProcessor, SharedUdpDatagramSink,
};

/// 在非 Windows 构建中保留相同控制面类型，启动捕获时返回明确的平台错误。
#[derive(Default)]
pub struct ProcessCapture {
    flowTable: CaptureFlowTable,
}

impl ProcessCapture {
    /// 创建未运行的捕获控制器；非 Windows 平台不会接触驱动或网络状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 非 Windows 不产生 UDP 捕获事件；保留接口让控制面无需平台条件分支。
    pub fn setUdpDatagramSink(&self, _sink: Option<SharedUdpDatagramSink>) {}

    /// 非 Windows 不执行封包处理；保留接口让控制面共享同一装配路径。
    pub fn setUdpDatagramProcessor(&self, _processor: Option<SharedUdpDatagramProcessor>) {}

    /// 非 Windows 平台仅允许保持关闭，防止配置被误报为已经生效。
    pub fn start(
        &self,
        configuration: ProcessCaptureConfiguration,
    ) -> Result<(), ProcessCaptureError> {
        configuration.validate(std::process::id())?;
        if configuration.enabled {
            return Err(ProcessCaptureError::UnsupportedPlatform);
        }
        Ok(())
    }

    /// 停止操作保持幂等，并清除任何测试期间注入的会话流。
    pub fn stop(&self) -> Result<(), ProcessCaptureError> {
        self.flowTable.clear();
        Ok(())
    }

    /// 返回未运行快照，使跨平台控制面协议能够稳定反序列化。
    pub fn snapshot(&self) -> ProcessCaptureSnapshot {
        ProcessCaptureSnapshot::default()
    }

    /// 查询透明连接原目标；非 Windows 正常运行时流表始终为空。
    pub fn originalTargetForPeer(
        &self,
        localAddress: IpAddr,
        peer: SocketAddr,
    ) -> Option<OriginalTarget> {
        self.flowTable.originalTargetForPeer(localAddress, peer)
    }
}

impl Drop for ProcessCapture {
    /// 析构时执行同一停止路径，保证未来加入测试句柄后仍保持资源语义一致。
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
