use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::error::{Result, Socks5Error};

/// 限制单实例并发连接，避免超过 Tokio Semaphore 边界或放大每连接缓冲区占用。
pub const maximumConnections: usize = 16_384;
/// 限制每个 TCP 转发方向的缓冲区，防止配置触发不受控的按连接分配。
pub const maximumRelayBufferSize: usize = 1_048_576;
/// 原始 TCP/SOCKS 流镜像必须保留完整字节序列；该参数只为统一 append 签名保留。
///
/// 运行上下文：数据面按块增长镜像，投影器再将完整字节交给 Capture 的磁盘 spill 存储。
/// 稳定性边界：任何有限值都会让高流量或长连接变成前缀抓取，所以生产路径使用平台最大值。
pub const capturedStreamPrefixLimit: usize = usize::MAX;
/// 单实例原始流镜像不设生产截断预算；显式小预算仅供底层边界测试注入。
pub const maximumTotalCapturedStreamBytes: usize = usize::MAX;
/// 限制转发缓冲区总预算；它只约束网络 I/O 工作缓冲，不得作为录制正文上限。
pub const maximumTotalRelayBufferSize: usize = 448 * 1024 * 1024;
/// 限制数据面有序关闭时间，使桌面监督器能够给出确定的更大外层期限。
pub const maximumShutdownTimeoutMilliseconds: u64 = 30_000;
/// 限制单个 UDP 关联记忆的远端地址数量；响应队列使用独立固定容量。
pub const maximumUdpRemoteLimit: usize = 4_096;
/// 限制跨运行周期保留的已结束会话数量。
pub const maximumSessionHistoryLimit: usize = 10_000;

/// 声明服务接受的认证方式；每次服务生命周期只启用一种明确策略。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthenticationMode {
    #[default]
    NoAuth,
    UsernamePassword,
    Plugin,
    AccountService,
}

/// 保存 SOCKS5 数据面配置；JSON 字段遵循前后端统一的 camelCase 契约。
#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Socks5Config {
    pub listenHost: IpAddr,
    pub listenPort: u16,
    pub authenticationMode: AuthenticationMode,
    pub users: HashMap<String, String>,
    pub maxConnections: usize,
    pub connectTimeoutMilliseconds: u64,
    pub bindTimeoutMilliseconds: u64,
    pub idleTimeoutMilliseconds: u64,
    pub readTimeoutMilliseconds: u64,
    pub shutdownTimeoutMilliseconds: u64,
    pub relayBufferSize: usize,
    pub udpBindHost: String,
    pub udpMaxPacketSize: usize,
    pub udpRemoteLimit: usize,
    pub sessionHistoryLimit: usize,
}

impl fmt::Debug for Socks5Config {
    /// 输出不含口令的诊断视图；用户名排序保证日志和测试结果稳定。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut authenticationUsernames: Vec<&str> =
            self.users.keys().map(String::as_str).collect();
        authenticationUsernames.sort_unstable();
        formatter
            .debug_struct("Socks5Config")
            .field("listenHost", &self.listenHost)
            .field("listenPort", &self.listenPort)
            .field("authenticationMode", &self.authenticationMode)
            .field("authenticationUsernames", &authenticationUsernames)
            .field("maxConnections", &self.maxConnections)
            .field(
                "connectTimeoutMilliseconds",
                &self.connectTimeoutMilliseconds,
            )
            .field("bindTimeoutMilliseconds", &self.bindTimeoutMilliseconds)
            .field("idleTimeoutMilliseconds", &self.idleTimeoutMilliseconds)
            .field("readTimeoutMilliseconds", &self.readTimeoutMilliseconds)
            .field(
                "shutdownTimeoutMilliseconds",
                &self.shutdownTimeoutMilliseconds,
            )
            .field("relayBufferSize", &self.relayBufferSize)
            .field("udpBindHost", &self.udpBindHost)
            .field("udpMaxPacketSize", &self.udpMaxPacketSize)
            .field("udpRemoteLimit", &self.udpRemoteLimit)
            .field("sessionHistoryLimit", &self.sessionHistoryLimit)
            .finish()
    }
}

impl Default for Socks5Config {
    /// 提供仅监听本机、无认证的开发默认值；外部配置仍须通过 validate 校验。
    fn default() -> Self {
        Self {
            listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
            listenPort: 1080,
            authenticationMode: AuthenticationMode::NoAuth,
            users: HashMap::new(),
            maxConnections: 1_024,
            connectTimeoutMilliseconds: 10_000,
            bindTimeoutMilliseconds: 30_000,
            idleTimeoutMilliseconds: 300_000,
            readTimeoutMilliseconds: 10_000,
            shutdownTimeoutMilliseconds: 5_000,
            relayBufferSize: 65_536,
            udpBindHost: String::new(),
            udpMaxPacketSize: 65_507,
            udpRemoteLimit: 1_024,
            sessionHistoryLimit: 500,
        }
    }
}

impl Socks5Config {
    /// 校验资源和认证边界；失败返回精确配置错误且不会启动监听器。
    pub fn validate(&self) -> Result<()> {
        if !(1..=maximumConnections).contains(&self.maxConnections) {
            return Err(Socks5Error::Configuration(format!(
                "maxConnections 必须位于 1..={maximumConnections}"
            )));
        }
        if !(1_024..=maximumRelayBufferSize).contains(&self.relayBufferSize) {
            return Err(Socks5Error::Configuration(format!(
                "relayBufferSize 必须位于 1024..={maximumRelayBufferSize}"
            )));
        }
        let totalRelayBufferSize = self
            .maxConnections
            .checked_mul(self.relayBufferSize)
            .and_then(|size| size.checked_mul(2))
            .ok_or_else(|| {
                Socks5Error::Configuration("TCP 双向转发缓冲区总预算计算溢出".to_owned())
            })?;
        // 转发缓冲与录制存储是两个独立边界：前者必须受内存预算约束，
        // 后者必须完整保留并由 Capture 的磁盘 spill 承载，不得为通过配置校验而截断。
        if totalRelayBufferSize > maximumTotalRelayBufferSize {
            return Err(Socks5Error::Configuration(format!(
                "TCP 双向转发缓冲区总预算不能超过 {maximumTotalRelayBufferSize} 字节"
            )));
        }
        if !(512..=65_507).contains(&self.udpMaxPacketSize) {
            return Err(Socks5Error::Configuration(
                "udpMaxPacketSize 必须位于 512..=65507".to_owned(),
            ));
        }
        if !(1..=maximumUdpRemoteLimit).contains(&self.udpRemoteLimit) {
            return Err(Socks5Error::Configuration(format!(
                "udpRemoteLimit 必须位于 1..={maximumUdpRemoteLimit}"
            )));
        }
        if self.sessionHistoryLimit > maximumSessionHistoryLimit {
            return Err(Socks5Error::Configuration(format!(
                "sessionHistoryLimit 不能超过 {maximumSessionHistoryLimit}"
            )));
        }
        if !self.udpBindHost.is_empty() {
            let udpBindHost = self.udpBindHost.parse::<IpAddr>().map_err(|_| {
                Socks5Error::Configuration(
                    "udpBindHost 必须是 IPv4、IPv6 地址或空字符串".to_owned(),
                )
            })?;
            if udpBindHost.is_ipv4() != self.listenHost.is_ipv4() {
                return Err(Socks5Error::Configuration(
                    "udpBindHost 与 listenHost 必须使用相同地址族".to_owned(),
                ));
            }
        }
        for (name, value) in [
            (
                "connectTimeoutMilliseconds",
                self.connectTimeoutMilliseconds,
            ),
            ("bindTimeoutMilliseconds", self.bindTimeoutMilliseconds),
            ("idleTimeoutMilliseconds", self.idleTimeoutMilliseconds),
            ("readTimeoutMilliseconds", self.readTimeoutMilliseconds),
        ] {
            if value == 0 {
                return Err(Socks5Error::Configuration(format!("{name} 必须大于零")));
            }
        }
        if !(1..=maximumShutdownTimeoutMilliseconds).contains(&self.shutdownTimeoutMilliseconds) {
            return Err(Socks5Error::Configuration(format!(
                "shutdownTimeoutMilliseconds 必须位于 1..={maximumShutdownTimeoutMilliseconds}"
            )));
        }
        if self.authenticationMode == AuthenticationMode::UsernamePassword && self.users.is_empty()
        {
            return Err(Socks5Error::Configuration(
                "用户名密码认证至少需要一个账户".to_owned(),
            ));
        }
        if self.users.iter().any(|(username, password)| {
            username.is_empty()
                || username.len() > u8::MAX as usize
                || password.is_empty()
                || password.len() > u8::MAX as usize
        }) {
            return Err(Socks5Error::Configuration(
                "用户名和密码长度必须位于 1..=255 字节".to_owned(),
            ));
        }
        Ok(())
    }

    /// 返回数据面监听地址；端口零表示由系统分配测试端口。
    pub fn listenAddress(&self) -> SocketAddr {
        SocketAddr::new(self.listenHost, self.listenPort)
    }

    /// 返回远端连接超时。
    pub fn connectTimeout(&self) -> Duration {
        Duration::from_millis(self.connectTimeoutMilliseconds)
    }

    /// 返回 BIND 等待远端连接超时。
    pub fn bindTimeout(&self) -> Duration {
        Duration::from_millis(self.bindTimeoutMilliseconds)
    }

    /// 返回单向转发空闲超时。
    pub fn idleTimeout(&self) -> Duration {
        Duration::from_millis(self.idleTimeoutMilliseconds)
    }

    /// 返回协议字段读取超时。
    pub fn readTimeout(&self) -> Duration {
        Duration::from_millis(self.readTimeoutMilliseconds)
    }

    /// 返回服务有序关闭等待时限。
    pub fn shutdownTimeout(&self) -> Duration {
        Duration::from_millis(self.shutdownTimeoutMilliseconds)
    }
}
