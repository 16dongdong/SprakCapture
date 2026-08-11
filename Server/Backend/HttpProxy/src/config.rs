use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::HttpProxyError;
use transport_core::{UpstreamProxyConfiguration, validateUpstreamProxy};

pub const minimumHeaderBytes: usize = 8 * 1024;
pub const maximumHeaderBytes: usize = 1024 * 1024;
pub const maximumTotalHeaderBufferBytes: usize = 256 * 1024 * 1024;
pub const maximumCaptureBodyBytes: usize = 64 * 1024 * 1024;
pub const maximumTotalCaptureBufferBytes: usize = 512 * 1024 * 1024;
pub const maximumConnections: usize = 16_384;
pub const maximumTimeoutMilliseconds: u64 = 5 * 60 * 1000;

/// 定义 HTTP 监听、工具物化预算和生命周期超时；录制镜像始终保留完整正文，不读取物化上限。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpProxyConfig {
    pub listenHost: IpAddr,
    pub listenPort: u16,
    pub maxConnections: usize,
    pub maxHeaderBytes: usize,
    pub maxCaptureBodyBytes: usize,
    pub connectTimeoutMilliseconds: u64,
    pub requestTimeoutMilliseconds: u64,
    pub headerReadTimeoutMilliseconds: u64,
    pub shutdownTimeoutMilliseconds: u64,
    #[serde(default)]
    pub upstreamProxy: UpstreamProxyConfiguration,
}

impl Default for HttpProxyConfig {
    /// 使用仅本机监听和有界桌面代理资源预算；端口零仅供显式测试配置使用。
    fn default() -> Self {
        Self {
            listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
            listenPort: 8_888,
            maxConnections: 512,
            maxHeaderBytes: 64 * 1024,
            maxCaptureBodyBytes: 256 * 1024,
            connectTimeoutMilliseconds: 10_000,
            requestTimeoutMilliseconds: 60_000,
            headerReadTimeoutMilliseconds: 15_000,
            shutdownTimeoutMilliseconds: 5_000,
            upstreamProxy: UpstreamProxyConfiguration::default(),
        }
    }
}

impl HttpProxyConfig {
    /// 校验所有资源与超时边界；失败返回稳定配置错误，不生成单语言消息。
    pub fn validate(&self) -> Result<(), HttpProxyError> {
        if !(1..=maximumConnections).contains(&self.maxConnections) {
            return Err(HttpProxyError::InvalidConnectionLimit);
        }
        if !(minimumHeaderBytes..=maximumHeaderBytes).contains(&self.maxHeaderBytes) {
            return Err(HttpProxyError::InvalidHeaderLimit);
        }
        let totalHeaderBufferBytes = self
            .maxHeaderBytes
            .checked_mul(self.maxConnections)
            .ok_or(HttpProxyError::HeaderBudgetExceeded)?;
        if totalHeaderBufferBytes > maximumTotalHeaderBufferBytes {
            return Err(HttpProxyError::HeaderBudgetExceeded);
        }
        // 字段名为兼容既有配置继续保留；它只约束需要随机访问正文的工具，不得用于录制裁剪。
        if !(1..=maximumCaptureBodyBytes).contains(&self.maxCaptureBodyBytes) {
            return Err(HttpProxyError::InvalidBodyLimit);
        }
        let totalCaptureBufferBytes = self
            .maxCaptureBodyBytes
            .checked_mul(2)
            .and_then(|perConnectionBytes| perConnectionBytes.checked_mul(self.maxConnections))
            .ok_or(HttpProxyError::CaptureBudgetExceeded)?;
        if totalCaptureBufferBytes > maximumTotalCaptureBufferBytes {
            return Err(HttpProxyError::CaptureBudgetExceeded);
        }
        for timeoutMilliseconds in [
            self.connectTimeoutMilliseconds,
            self.requestTimeoutMilliseconds,
            self.headerReadTimeoutMilliseconds,
            self.shutdownTimeoutMilliseconds,
        ] {
            if !(1..=maximumTimeoutMilliseconds).contains(&timeoutMilliseconds) {
                return Err(HttpProxyError::InvalidTimeout);
            }
        }
        validateUpstreamProxy(&self.upstreamProxy)
            .map_err(|_| HttpProxyError::InvalidUpstreamProxy)?;
        Ok(())
    }

    /// 返回待绑定地址；listenPort=0 时由操作系统分配测试端口。
    pub const fn listenAddress(&self) -> SocketAddr {
        SocketAddr::new(self.listenHost, self.listenPort)
    }

    /// 返回上游 TCP 建连超时。
    pub const fn connectTimeout(&self) -> Duration {
        Duration::from_millis(self.connectTimeoutMilliseconds)
    }

    /// 返回从发送请求到收到响应头的最大等待时间。
    pub const fn requestTimeout(&self) -> Duration {
        Duration::from_millis(self.requestTimeoutMilliseconds)
    }

    /// 返回客户端请求头读取超时。
    pub const fn headerReadTimeout(&self) -> Duration {
        Duration::from_millis(self.headerReadTimeoutMilliseconds)
    }

    /// 返回停止时等待连接、响应泵和隧道退出的最大时间。
    pub const fn shutdownTimeout(&self) -> Duration {
        Duration::from_millis(self.shutdownTimeoutMilliseconds)
    }

    /// 返回单连接优雅排空预算；严格短于服务总停止预算，为强制丢弃连接和任务汇合保留时间。
    pub fn connectionDrainTimeout(&self) -> Duration {
        Duration::from_millis((self.shutdownTimeoutMilliseconds / 4).max(1))
    }
}
