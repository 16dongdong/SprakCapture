use std::{
    fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::lookup_host,
};

use crate::error::{Result, Socks5Error};

pub const addressTypeIpv4: u8 = 0x01;
pub const addressTypeDomain: u8 = 0x03;
pub const addressTypeIpv6: u8 = 0x04;

/// 表示 SOCKS5 线协议目标地址，域名在实际连接前保持原始文本。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TargetHost {
    Ip(IpAddr),
    Domain(String),
}

/// 为 SOCKS5 TCP/UDP 出站提供可选的进程内域名覆盖；未命中时仍由系统 DNS 完成解析。
pub trait AddressOverride: Send + Sync {
    /// 返回域名对应的强制 IP；IP 字面量不会调用该接口。
    fn resolveIp(&self, host: &str) -> Option<IpAddr>;
}

/// 聚合目标主机与端口，供 CONNECT、BIND 和 UDP 共用同一编解码。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TargetAddress {
    pub host: TargetHost,
    pub port: u16,
}

impl TargetAddress {
    /// 从 SocketAddr 创建无需 DNS 的目标地址。
    pub fn fromSocketAddress(address: SocketAddr) -> Self {
        Self {
            host: TargetHost::Ip(address.ip()),
            port: address.port(),
        }
    }

    /// 判断目标是否为协议允许的未指定地址，UDP 与 BIND 用它表达任意来源。
    pub fn isUnspecified(&self) -> bool {
        matches!(self.host, TargetHost::Ip(address) if address.is_unspecified())
    }

    /// 解析目标并返回全部可用 SocketAddr；空解析结果作为显式错误返回。
    pub async fn resolve(&self) -> Result<Vec<SocketAddr>> {
        self.resolveWithOverride(None).await
    }

    /// 优先应用注入的域名覆盖，再回落系统解析；返回地址始终带有请求原端口。
    ///
    /// 运行上下文：TCP CONNECT 与 UDP 数据报都调用该入口，保证两种传输的规则语义一致。
    /// 失败语义：覆盖命中不触发系统 DNS；未命中的空解析结果仍返回结构化 `Resolve` 错误。
    pub async fn resolveWithOverride(
        &self,
        addressOverride: Option<&dyn AddressOverride>,
    ) -> Result<Vec<SocketAddr>> {
        let addresses = match &self.host {
            TargetHost::Ip(address) => vec![SocketAddr::new(*address, self.port)],
            TargetHost::Domain(domain) => {
                match addressOverride.and_then(|resolver| resolver.resolveIp(domain)) {
                    Some(address) => vec![SocketAddr::new(address, self.port)],
                    None => lookup_host((domain.as_str(), self.port))
                        .await
                        .map_err(|error| Socks5Error::Resolve(error.to_string()))?
                        .collect(),
                }
            }
        };
        if addresses.is_empty() {
            return Err(Socks5Error::Resolve(self.toString()));
        }
        Ok(addresses)
    }

    /// 返回适合会话快照与诊断输出的稳定地址文本。
    pub fn toString(&self) -> String {
        match &self.host {
            TargetHost::Ip(address) => SocketAddr::new(*address, self.port).to_string(),
            TargetHost::Domain(domain) => format!("{domain}:{}", self.port),
        }
    }

    /// 返回不含端口的目标主机文本，供插件匹配与连接元数据使用；端口必须始终单独通过 `port` 传递，避免 `hosts` 规则失效。
    pub fn hostString(&self) -> String {
        match &self.host {
            TargetHost::Ip(address) => address.to_string(),
            TargetHost::Domain(domain) => domain.clone(),
        }
    }
}

impl fmt::Display for TargetAddress {
    /// 使用与 toString 相同的稳定端点格式。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.toString())
    }
}

/// 从异步字节流读取 SOCKS5 地址；截断输入保留原始 I/O 错误。
pub async fn readTargetAddress<R>(reader: &mut R) -> Result<TargetAddress>
where
    R: AsyncRead + Unpin,
{
    let addressType = reader.read_u8().await?;
    let host = match addressType {
        addressTypeIpv4 => {
            let mut octets = [0_u8; 4];
            reader.read_exact(&mut octets).await?;
            TargetHost::Ip(IpAddr::V4(Ipv4Addr::from(octets)))
        }
        addressTypeIpv6 => {
            let mut octets = [0_u8; 16];
            reader.read_exact(&mut octets).await?;
            TargetHost::Ip(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        addressTypeDomain => {
            let domainLength = reader.read_u8().await? as usize;
            if domainLength == 0 {
                return Err(Socks5Error::InvalidDomain);
            }
            let mut domainBytes = vec![0_u8; domainLength];
            reader.read_exact(&mut domainBytes).await?;
            let domain = String::from_utf8(domainBytes).map_err(|_| Socks5Error::InvalidDomain)?;
            TargetHost::Domain(domain)
        }
        other => return Err(Socks5Error::UnsupportedAddressType(other)),
    };
    let port = reader.read_u16().await?;
    Ok(TargetAddress { host, port })
}

/// 向异步字节流写入 SOCKS5 地址；超过255字节的域名返回 InvalidInput。
pub async fn writeTargetAddress<W>(writer: &mut W, address: &TargetAddress) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    match &address.host {
        TargetHost::Ip(IpAddr::V4(ipv4)) => {
            writer.write_u8(addressTypeIpv4).await?;
            writer.write_all(&ipv4.octets()).await?;
        }
        TargetHost::Ip(IpAddr::V6(ipv6)) => {
            writer.write_u8(addressTypeIpv6).await?;
            writer.write_all(&ipv6.octets()).await?;
        }
        TargetHost::Domain(domain) => {
            let domainBytes = domain.as_bytes();
            let domainLength = u8::try_from(domainBytes.len()).map_err(|_| {
                Socks5Error::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SOCKS5 域名超过255字节",
                ))
            })?;
            if domainLength == 0 {
                return Err(Socks5Error::InvalidDomain);
            }
            writer.write_u8(addressTypeDomain).await?;
            writer.write_u8(domainLength).await?;
            writer.write_all(domainBytes).await?;
        }
    }
    writer.write_u16(address.port).await?;
    Ok(())
}

/// 把地址编码到内存缓冲区，UDP 帧无需创建临时异步流。
pub fn encodeTargetAddress(address: &TargetAddress, output: &mut Vec<u8>) -> Result<()> {
    match &address.host {
        TargetHost::Ip(IpAddr::V4(ipv4)) => {
            output.push(addressTypeIpv4);
            output.extend_from_slice(&ipv4.octets());
        }
        TargetHost::Ip(IpAddr::V6(ipv6)) => {
            output.push(addressTypeIpv6);
            output.extend_from_slice(&ipv6.octets());
        }
        TargetHost::Domain(domain) => {
            let length = u8::try_from(domain.len()).map_err(|_| Socks5Error::InvalidDomain)?;
            if length == 0 {
                return Err(Socks5Error::InvalidDomain);
            }
            output.extend_from_slice(&[addressTypeDomain, length]);
            output.extend_from_slice(domain.as_bytes());
        }
    }
    output.extend_from_slice(&address.port.to_be_bytes());
    Ok(())
}

/// 从字节切片解码地址并返回负载起点，所有边界在读取前检查。
pub fn decodeTargetAddress(input: &[u8], offset: usize) -> Result<(TargetAddress, usize)> {
    let addressType = *input
        .get(offset)
        .ok_or_else(|| Socks5Error::InvalidUdpPacket("缺少地址类型".to_owned()))?;
    let mut cursor = offset + 1;
    let host = match addressType {
        addressTypeIpv4 => {
            let bytes = input
                .get(cursor..cursor + 4)
                .ok_or_else(|| Socks5Error::InvalidUdpPacket("IPv4 地址被截断".to_owned()))?;
            cursor += 4;
            TargetHost::Ip(IpAddr::V4(Ipv4Addr::new(
                bytes[0], bytes[1], bytes[2], bytes[3],
            )))
        }
        addressTypeIpv6 => {
            let bytes = input
                .get(cursor..cursor + 16)
                .ok_or_else(|| Socks5Error::InvalidUdpPacket("IPv6 地址被截断".to_owned()))?;
            let mut octets = [0_u8; 16];
            octets.copy_from_slice(bytes);
            cursor += 16;
            TargetHost::Ip(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        addressTypeDomain => {
            let length = *input
                .get(cursor)
                .ok_or_else(|| Socks5Error::InvalidUdpPacket("缺少域名长度".to_owned()))?
                as usize;
            cursor += 1;
            if length == 0 {
                return Err(Socks5Error::InvalidDomain);
            }
            let bytes = input
                .get(cursor..cursor + length)
                .ok_or_else(|| Socks5Error::InvalidUdpPacket("域名被截断".to_owned()))?;
            cursor += length;
            TargetHost::Domain(
                String::from_utf8(bytes.to_vec()).map_err(|_| Socks5Error::InvalidDomain)?,
            )
        }
        other => return Err(Socks5Error::UnsupportedAddressType(other)),
    };
    let portBytes = input
        .get(cursor..cursor + 2)
        .ok_or_else(|| Socks5Error::InvalidUdpPacket("端口被截断".to_owned()))?;
    cursor += 2;
    Ok((
        TargetAddress {
            host,
            port: u16::from_be_bytes([portBytes[0], portBytes[1]]),
        },
        cursor,
    ))
}
