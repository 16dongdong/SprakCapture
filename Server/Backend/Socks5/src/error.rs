use std::io;

use thiserror::Error;

/// 统一库内错误类型；协议拒绝、配置缺陷和传输故障保持可区分。
#[derive(Debug, Error)]
pub enum Socks5Error {
    #[error("配置错误：{0}")]
    Configuration(String),
    #[error("I/O 错误：{0}")]
    Io(#[from] io::Error),
    #[error("{0}超时")]
    Timeout(&'static str),
    #[error("SOCKS 版本不受支持：{0}")]
    UnsupportedVersion(u8),
    #[error("客户端未提供可接受的认证方法")]
    NoAcceptableAuthentication,
    #[error("用户名或密码校验失败")]
    AuthenticationFailed,
    #[error("SOCKS5 请求保留字节无效：{0}")]
    InvalidReservedByte(u8),
    #[error("SOCKS5 命令不受支持：{0}")]
    UnsupportedCommand(u8),
    #[error("SOCKS5 地址类型不受支持：{0}")]
    UnsupportedAddressType(u8),
    #[error("SOCKS5 域名字段无效")]
    InvalidDomain,
    #[error("目标地址解析失败：{0}")]
    Resolve(String),
    #[error("UDP 数据报无效：{0}")]
    InvalidUdpPacket(String),
    #[error("UDP 客户端来源与控制连接不一致")]
    InvalidUdpSource,
    #[error("远端连接失败：{0}")]
    RemoteConnect(String),
    #[error("插件请求关闭连接")]
    PluginClosed,
    #[error("服务任务提前结束：{0}")]
    Runtime(String),
}

/// 库公开结果别名，调用方无需重复声明错误类型。
pub type Result<T> = std::result::Result<T, Socks5Error>;
