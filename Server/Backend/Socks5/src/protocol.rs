use std::{collections::HashMap, future::Future, net::SocketAddr, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

use crate::{
    address::{TargetAddress, decodeTargetAddress, encodeTargetAddress, readTargetAddress},
    config::AuthenticationMode,
    error::{Result, Socks5Error},
};

pub const socksVersion: u8 = 0x05;
pub const usernamePasswordVersion: u8 = 0x01;
pub const methodNoAuth: u8 = 0x00;
pub const methodUsernamePassword: u8 = 0x02;
pub const methodNoAcceptable: u8 = 0xff;
pub const commandConnect: u8 = 0x01;
pub const commandBind: u8 = 0x02;
pub const commandUdpAssociate: u8 = 0x03;
pub const replySucceeded: u8 = 0x00;
pub const replyGeneralFailure: u8 = 0x01;
pub const replyConnectionNotAllowed: u8 = 0x02;
pub const replyNetworkUnreachable: u8 = 0x03;
pub const replyHostUnreachable: u8 = 0x04;
pub const replyConnectionRefused: u8 = 0x05;
pub const replyTtlExpired: u8 = 0x06;
pub const replyCommandNotSupported: u8 = 0x07;
pub const replyAddressTypeNotSupported: u8 = 0x08;

/// 保存一个已解码 SOCKS5 命令请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocksRequest {
    pub command: u8,
    pub destination: TargetAddress,
}

/// 保存 SOCKS5 UDP 数据报的目标和有效负载；当前服务明确拒绝分片。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UdpPacket {
    pub destination: TargetAddress,
    pub payload: Vec<u8>,
}

/// 在读取时限内完成认证方法协商和可选 RFC1929 校验，成功返回用户名。
pub async fn negotiateAuthentication<S>(
    stream: &mut S,
    mode: &AuthenticationMode,
    users: &HashMap<String, String>,
    readTimeout: Duration,
) -> Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(readTimeout, async {
        let version = stream.read_u8().await?;
        if version != socksVersion {
            return Err(Socks5Error::UnsupportedVersion(version));
        }
        let methodCount = stream.read_u8().await? as usize;
        let mut methods = vec![0_u8; methodCount];
        stream.read_exact(&mut methods).await?;
        let selectedMethod = match mode {
            AuthenticationMode::NoAuth if methods.contains(&methodNoAuth) => methodNoAuth,
            AuthenticationMode::UsernamePassword if methods.contains(&methodUsernamePassword) => {
                methodUsernamePassword
            }
            AuthenticationMode::Plugin if methods.contains(&methodUsernamePassword) => {
                methodUsernamePassword
            }
            _ => methodNoAcceptable,
        };
        stream.write_all(&[socksVersion, selectedMethod]).await?;
        stream.flush().await?;
        if selectedMethod == methodNoAcceptable {
            return Err(Socks5Error::NoAcceptableAuthentication);
        }
        if selectedMethod == methodNoAuth {
            return Ok(String::new());
        }
        if *mode == AuthenticationMode::Plugin {
            return Err(Socks5Error::AuthenticationFailed);
        }
        authenticateUsernamePassword(stream, users).await
    })
    .await
    .map_err(|_| Socks5Error::Timeout("认证协商"))?
}

/// 使用 RFC1929 读取用户名密码并把最终判定交给异步插件回调；协议层只回写标准成功或失败状态。
///
/// 运行上下文：仅在配置为插件认证时调用；`verifier` 返回主体 ID 表示接受，返回 `None` 表示拒绝。
/// 失败语义：方法不兼容、字段非法、插件拒绝和读取超时均发送协议允许的失败响应并终止认证。
pub async fn negotiatePluginAuthentication<S, F, Fut>(
    stream: &mut S,
    readTimeout: Duration,
    verifier: F,
) -> Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(String, String) -> Fut,
    Fut: Future<Output = Option<String>>,
{
    timeout(readTimeout, async {
        let version = stream.read_u8().await?;
        if version != socksVersion {
            return Err(Socks5Error::UnsupportedVersion(version));
        }
        let methodCount = stream.read_u8().await? as usize;
        let mut methods = vec![0_u8; methodCount];
        stream.read_exact(&mut methods).await?;
        let selectedMethod = if methods.contains(&methodUsernamePassword) {
            methodUsernamePassword
        } else {
            methodNoAcceptable
        };
        stream.write_all(&[socksVersion, selectedMethod]).await?;
        stream.flush().await?;
        if selectedMethod == methodNoAcceptable {
            return Err(Socks5Error::NoAcceptableAuthentication);
        }
        let (username, password) = readUsernamePassword(stream).await?;
        let principalId = verifier(username, password).await;
        writeAuthenticationStatus(stream, principalId.is_some()).await?;
        principalId.ok_or(Socks5Error::AuthenticationFailed)
    })
    .await
    .map_err(|_| Socks5Error::Timeout("插件认证协商"))?
}

/// 校验 RFC1929 用户名密码子协商；失败响应固定为状态一且不泄露账户存在性。
async fn authenticateUsernamePassword<S>(
    stream: &mut S,
    users: &HashMap<String, String>,
) -> Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (username, password) = readUsernamePassword(stream).await?;
    let valid = users
        .get(&username)
        .is_some_and(|expected| constantTimeEqual(expected.as_bytes(), password.as_bytes()));
    writeAuthenticationStatus(stream, valid).await?;
    if !valid {
        return Err(Socks5Error::AuthenticationFailed);
    }
    Ok(username)
}

/// 解码一帧 RFC1929 用户名密码，不执行认证决策；非法文本统一回写失败且不泄露字段差异。
async fn readUsernamePassword<S>(stream: &mut S) -> Result<(String, String)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let version = stream.read_u8().await?;
    if version != usernamePasswordVersion {
        writeAuthenticationStatus(stream, false).await?;
        return Err(Socks5Error::UnsupportedVersion(version));
    }
    let usernameLength = stream.read_u8().await? as usize;
    if usernameLength == 0 {
        writeAuthenticationStatus(stream, false).await?;
        return Err(Socks5Error::AuthenticationFailed);
    }
    let mut usernameBytes = vec![0_u8; usernameLength];
    stream.read_exact(&mut usernameBytes).await?;
    let passwordLength = stream.read_u8().await? as usize;
    if passwordLength == 0 {
        writeAuthenticationStatus(stream, false).await?;
        return Err(Socks5Error::AuthenticationFailed);
    }
    let mut passwordBytes = vec![0_u8; passwordLength];
    stream.read_exact(&mut passwordBytes).await?;
    // RFC1929 字段先按定长字节串完整读取；文本解码失败也必须先回写认证失败，不能让客户端误判为连接截断。
    let username = match std::str::from_utf8(&usernameBytes) {
        Ok(username) => username,
        Err(_) => {
            writeAuthenticationStatus(stream, false).await?;
            return Err(Socks5Error::AuthenticationFailed);
        }
    };
    let password = match std::str::from_utf8(&passwordBytes) {
        Ok(password) => password,
        Err(_) => {
            writeAuthenticationStatus(stream, false).await?;
            return Err(Socks5Error::AuthenticationFailed);
        }
    };
    Ok((username.to_owned(), password.to_owned()))
}

/// 写入并刷新 RFC1929 认证状态；失败路径统一使用状态一且不得携带账户诊断。
async fn writeAuthenticationStatus<S>(stream: &mut S, authenticated: bool) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream
        .write_all(&[usernamePasswordVersion, u8::from(!authenticated)])
        .await?;
    stream.flush().await?;
    Ok(())
}

/// 以固定循环比较凭据字节，避免账户密码长度相同情况下的提前退出。
fn constantTimeEqual(expected: &[u8], actual: &[u8]) -> bool {
    let mut difference = expected.len() ^ actual.len();
    let maximumLength = expected.len().max(actual.len());
    for index in 0..maximumLength {
        let expectedByte = expected.get(index).copied().unwrap_or(0);
        let actualByte = actual.get(index).copied().unwrap_or(0);
        difference |= usize::from(expectedByte ^ actualByte);
    }
    difference == 0
}

/// 在读取时限内解码命令、保留字节和目标地址。
pub async fn readRequest<S>(stream: &mut S, readTimeout: Duration) -> Result<SocksRequest>
where
    S: AsyncRead + Unpin,
{
    timeout(readTimeout, async {
        let version = stream.read_u8().await?;
        if version != socksVersion {
            return Err(Socks5Error::UnsupportedVersion(version));
        }
        let command = stream.read_u8().await?;
        let reserved = stream.read_u8().await?;
        if reserved != 0 {
            return Err(Socks5Error::InvalidReservedByte(reserved));
        }
        let destination = readTargetAddress(stream).await?;
        Ok(SocksRequest {
            command,
            destination,
        })
    })
    .await
    .map_err(|_| Socks5Error::Timeout("命令读取"))?
}

/// 写入 RFC1928 响应并刷新底层流。
pub async fn writeReply<S>(stream: &mut S, replyCode: u8, boundAddress: SocketAddr) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream.write_all(&[socksVersion, replyCode, 0x00]).await?;
    crate::address::writeTargetAddress(stream, &TargetAddress::fromSocketAddress(boundAddress))
        .await?;
    stream.flush().await?;
    Ok(())
}

/// 解码 SOCKS5 UDP 数据报；RFC1928 分片字段非零时明确拒绝。
pub fn decodeUdpPacket(rawPacket: &[u8]) -> Result<UdpPacket> {
    if rawPacket.len() < 4 {
        return Err(Socks5Error::InvalidUdpPacket("数据报过短".to_owned()));
    }
    if rawPacket[0] != 0 || rawPacket[1] != 0 {
        return Err(Socks5Error::InvalidUdpPacket("保留字段非零".to_owned()));
    }
    if rawPacket[2] != 0 {
        return Err(Socks5Error::InvalidUdpPacket(
            "当前实现不接受 UDP 分片".to_owned(),
        ));
    }
    let (destination, payloadOffset) = decodeTargetAddress(rawPacket, 3)?;
    Ok(UdpPacket {
        destination,
        payload: rawPacket[payloadOffset..].to_vec(),
    })
}

/// 编码 SOCKS5 UDP 响应数据报；payload 不复制到中间协议对象。
pub fn encodeUdpPacket(source: SocketAddr, payload: &[u8]) -> Result<Vec<u8>> {
    let mut packet = vec![0x00, 0x00, 0x00];
    encodeTargetAddress(&TargetAddress::fromSocketAddress(source), &mut packet)?;
    packet.extend_from_slice(payload);
    Ok(packet)
}

/// 把远端连接错误映射为 RFC1928 REP，未知错误使用通用失败。
pub fn mapIoErrorToReply(error: &std::io::Error) -> u8 {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::ConnectionRefused => replyConnectionRefused,
        ErrorKind::HostUnreachable => replyHostUnreachable,
        ErrorKind::NetworkUnreachable => replyNetworkUnreachable,
        ErrorKind::TimedOut => replyTtlExpired,
        ErrorKind::PermissionDenied => replyConnectionNotAllowed,
        _ => replyGeneralFailure,
    }
}
