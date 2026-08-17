use std::{collections::HashSet, io, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, lookup_host},
    task::JoinSet,
    time::{Instant, sleep_until, timeout},
};

const maximumHttpConnectHeaderBytes: usize = 16 * 1024;
const addressAttemptDelay: Duration = Duration::from_millis(250);

/// 声明二级代理的线协议；HTTP 使用 CONNECT，因此明文与 TLS 目标共享同一条安全的隧道语义。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpstreamProxyProtocol {
    Http,
    Socks5,
}

/// 保存全数据面共享的二级代理配置；口令只参与建连，不应进入日志或调试格式。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpstreamProxyConfiguration {
    pub enabled: bool,
    pub protocol: UpstreamProxyProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for UpstreamProxyConfiguration {
    /// 输出可诊断但不含口令的配置视图，防止控制面日志泄露认证材料。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UpstreamProxyConfiguration")
            .field("enabled", &self.enabled)
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("hasPassword", &!self.password.is_empty())
            .finish()
    }
}

impl Default for UpstreamProxyConfiguration {
    /// 默认关闭二级代理；其余字段给界面提供确定初值，但关闭时不会产生任何出站连接。
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: UpstreamProxyProtocol::Socks5,
            host: "127.0.0.1".to_owned(),
            port: 1080,
            username: String::new(),
            password: String::new(),
        }
    }
}

/// 表示配置或握手阶段的精确失败；调用层可保留协议错误和网络错误之间的区别。
#[derive(Debug, Error)]
pub enum OutboundConnectError {
    #[error("二级代理配置无效：{0}")]
    Configuration(String),
    #[error("出站连接超时")]
    Timeout,
    #[error("出站连接 I/O 失败：{0}")]
    Io(#[from] io::Error),
    #[error("二级代理拒绝目标连接：{0}")]
    Rejected(String),
    #[error("二级代理返回了无效协议数据：{0}")]
    Protocol(String),
}

/// 校验二级代理端点和认证字段；关闭状态仍校验字段长度，保证重新启用时不会延迟失败。
pub fn validateUpstreamProxy(
    configuration: &UpstreamProxyConfiguration,
) -> Result<(), OutboundConnectError> {
    if configuration.host.trim().is_empty() {
        return Err(OutboundConnectError::Configuration(
            "host 不能为空".to_owned(),
        ));
    }
    if configuration.port == 0 {
        return Err(OutboundConnectError::Configuration(
            "port 必须位于 1..=65535".to_owned(),
        ));
    }
    if configuration.username.len() > u8::MAX as usize
        || configuration.password.len() > u8::MAX as usize
    {
        return Err(OutboundConnectError::Configuration(
            "用户名和密码不能超过 255 字节".to_owned(),
        ));
    }
    if configuration.username.is_empty() && !configuration.password.is_empty() {
        return Err(OutboundConnectError::Configuration(
            "设置密码时必须同时设置用户名".to_owned(),
        ));
    }
    Ok(())
}

/// 为 HTTP、HTTPS、SOCKS5 CONNECT 和透明转发提供同一个出站策略快照。
#[derive(Clone)]
pub struct OutboundConnector {
    upstream: Option<Arc<UpstreamProxyConfiguration>>,
    connectTimeout: Duration,
}

impl OutboundConnector {
    /// 从已校验配置构造不可变连接器；配置更新通过重启数据面切换完整快照。
    pub fn new(
        configuration: UpstreamProxyConfiguration,
        connectTimeout: Duration,
    ) -> Result<Self, OutboundConnectError> {
        validateUpstreamProxy(&configuration)?;
        Ok(Self {
            upstream: configuration.enabled.then(|| Arc::new(configuration)),
            connectTimeout,
        })
    }

    /// 建立到目标的 TCP 字节流；二级代理启用时先连接代理端点并完成对应握手。
    pub async fn connect(
        &self,
        targetHost: &str,
        targetPort: u16,
    ) -> Result<TcpStream, OutboundConnectError> {
        timeout(
            self.connectTimeout,
            self.connectWithinTimeout(targetHost, targetPort),
        )
        .await
        .map_err(|_| OutboundConnectError::Timeout)?
    }

    /// 返回当前快照是否启用了二级代理；解析器据此决定域名应在本地还是代理端解析。
    pub fn usesUpstreamProxy(&self) -> bool {
        self.upstream.is_some()
    }

    /// 在外层统一时限内完成直接连接或二级代理握手，避免多个阶段分别消耗完整超时预算。
    async fn connectWithinTimeout(
        &self,
        targetHost: &str,
        targetPort: u16,
    ) -> Result<TcpStream, OutboundConnectError> {
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(connectResolvedHost(targetHost, targetPort).await?);
        };
        let mut stream = connectResolvedHost(&upstream.host, upstream.port).await?;
        stream.set_nodelay(true)?;
        match upstream.protocol {
            UpstreamProxyProtocol::Http => {
                negotiateHttpConnect(&mut stream, targetHost, targetPort, upstream).await?
            }
            UpstreamProxyProtocol::Socks5 => {
                negotiateSocks5(&mut stream, targetHost, targetPort, upstream).await?
            }
        }
        Ok(stream)
    }
}

/// 按 Happy Eyeballs 的节奏竞争主机地址，并返回最先成功的字节流。
///
/// 运行上下文：直连目标和二级代理端点共用此入口；后续候选每隔 250ms 才启动，既绕过黑洞地址，
/// 也避免一次业务连接同时向同一站点制造多条成功连接而触发对端限流。
/// 失败语义：解析为空或全部地址失败时返回最后一个网络错误；未完成的竞争连接会随任务集合释放而取消。
async fn connectResolvedHost(targetHost: &str, targetPort: u16) -> io::Result<TcpStream> {
    let addresses = lookup_host((targetHost, targetPort))
        .await?
        .collect::<Vec<_>>();
    let addresses = deduplicateAddresses(addresses);
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "目标主机没有可用地址",
        ));
    }
    let mut addresses = addresses.into_iter();

    let mut attempts = JoinSet::new();
    attempts.spawn(TcpStream::connect(
        addresses.next().expect("地址集合已确认非空"),
    ));
    let mut nextAttemptAt = Instant::now() + addressAttemptDelay;
    let mut lastError = None;
    loop {
        if attempts.is_empty() {
            let Some(address) = addresses.next() else {
                break;
            };
            attempts.spawn(TcpStream::connect(address));
            nextAttemptAt = Instant::now() + addressAttemptDelay;
        }
        tokio::select! {
            result = attempts.join_next() => {
                match result.expect("存在尚未完成的地址连接任务") {
                    Ok(Ok(stream)) => return Ok(stream),
                    Ok(Err(error)) => lastError = Some(error),
                    Err(error) => {
                        lastError = Some(io::Error::other(format!("连接任务异常结束：{error}")))
                    }
                }
            }
            _ = sleep_until(nextAttemptAt), if addresses.len() > 0 => {
                attempts.spawn(TcpStream::connect(
                    addresses.next().expect("分支只在仍有候选地址时执行"),
                ));
                nextAttemptAt = Instant::now() + addressAttemptDelay;
            }
        }
    }
    Err(lastError
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "目标主机没有可用地址")))
}

/// 保留系统解析顺序并线性去重地址，避免排序破坏操作系统针对地址族给出的优先级。
fn deduplicateAddresses(addresses: Vec<std::net::SocketAddr>) -> Vec<std::net::SocketAddr> {
    let mut observedAddresses = HashSet::with_capacity(addresses.len());
    let mut uniqueAddresses = Vec::with_capacity(addresses.len());
    for address in addresses {
        if observedAddresses.insert(address) {
            uniqueAddresses.push(address);
        }
    }
    uniqueAddresses
}

/// 使用 HTTP CONNECT 建立任意 TCP 隧道；响应头设置硬上限以抵御异常上游无限增长。
async fn negotiateHttpConnect(
    stream: &mut TcpStream,
    targetHost: &str,
    targetPort: u16,
    upstream: &UpstreamProxyConfiguration,
) -> Result<(), OutboundConnectError> {
    let authority = formatAuthority(targetHost, targetPort);
    let authorization = if upstream.username.is_empty() {
        String::new()
    } else {
        let credentials = STANDARD.encode(format!("{}:{}", upstream.username, upstream.password));
        format!("Proxy-Authorization: Basic {credentials}\r\n")
    };
    let request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{authorization}Proxy-Connection: Keep-Alive\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    let response = readHttpHeaders(stream).await?;
    let statusLine = response
        .split("\r\n")
        .next()
        .ok_or_else(|| OutboundConnectError::Protocol("缺少状态行".to_owned()))?;
    let statusCode = statusLine
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| OutboundConnectError::Protocol("状态码无效".to_owned()))?;
    if !(200..300).contains(&statusCode) {
        return Err(OutboundConnectError::Rejected(format!(
            "HTTP CONNECT 返回 {statusCode}"
        )));
    }
    Ok(())
}

/// 读取完整 HTTP 响应头且不越过正文首字节；CONNECT 成功响应不应携带正文。
async fn readHttpHeaders(stream: &mut TcpStream) -> Result<String, OutboundConnectError> {
    let mut bytes = Vec::with_capacity(512);
    while bytes.len() < maximumHttpConnectHeaderBytes {
        let mut next = [0_u8; 1];
        stream.read_exact(&mut next).await?;
        bytes.push(next[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return String::from_utf8(bytes)
                .map_err(|_| OutboundConnectError::Protocol("响应头不是 UTF-8".to_owned()));
        }
    }
    Err(OutboundConnectError::Protocol(
        "响应头超过 16384 字节".to_owned(),
    ))
}

/// 完成 SOCKS5 方法协商、可选用户名密码认证和域名型 CONNECT 请求。
async fn negotiateSocks5(
    stream: &mut TcpStream,
    targetHost: &str,
    targetPort: u16,
    upstream: &UpstreamProxyConfiguration,
) -> Result<(), OutboundConnectError> {
    let authenticated = !upstream.username.is_empty();
    let methods: &[u8] = if authenticated {
        &[0x00, 0x02]
    } else {
        &[0x00]
    };
    stream
        .write_all(&[&[0x05, methods.len() as u8], methods].concat())
        .await?;
    let mut selection = [0_u8; 2];
    stream.read_exact(&mut selection).await?;
    if selection[0] != 0x05 || selection[1] == 0xff {
        return Err(OutboundConnectError::Rejected(
            "SOCKS5 未接受认证方法".to_owned(),
        ));
    }
    match selection[1] {
        0x00 => {}
        0x02 if authenticated => authenticateSocks5(stream, upstream).await?,
        method => {
            return Err(OutboundConnectError::Protocol(format!(
                "SOCKS5 选择了未提供的认证方法 {method:#04x}"
            )));
        }
    }
    let hostBytes = targetHost.as_bytes();
    if hostBytes.is_empty() || hostBytes.len() > u8::MAX as usize {
        return Err(OutboundConnectError::Configuration(
            "目标主机长度必须位于 1..=255 字节".to_owned(),
        ));
    }
    let mut request = Vec::with_capacity(hostBytes.len() + 7);
    request.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, hostBytes.len() as u8]);
    request.extend_from_slice(hostBytes);
    request.extend_from_slice(&targetPort.to_be_bytes());
    stream.write_all(&request).await?;
    readSocks5Reply(stream).await
}

/// 执行 RFC 1929 用户名密码认证；只有服务端明确选择该方法时才发送凭据。
async fn authenticateSocks5(
    stream: &mut TcpStream,
    upstream: &UpstreamProxyConfiguration,
) -> Result<(), OutboundConnectError> {
    let username = upstream.username.as_bytes();
    let password = upstream.password.as_bytes();
    let mut request = Vec::with_capacity(username.len() + password.len() + 3);
    request.extend_from_slice(&[0x01, username.len() as u8]);
    request.extend_from_slice(username);
    request.push(password.len() as u8);
    request.extend_from_slice(password);
    stream.write_all(&request).await?;
    let mut response = [0_u8; 2];
    stream.read_exact(&mut response).await?;
    if response != [0x01, 0x00] {
        return Err(OutboundConnectError::Rejected(
            "SOCKS5 用户名密码认证失败".to_owned(),
        ));
    }
    Ok(())
}

/// 读取 SOCKS5 CONNECT 响应的完整绑定地址，避免残留字节污染后续目标协议。
async fn readSocks5Reply(stream: &mut TcpStream) -> Result<(), OutboundConnectError> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 {
        return Err(OutboundConnectError::Protocol(
            "SOCKS5 响应版本无效".to_owned(),
        ));
    }
    if header[1] != 0x00 {
        return Err(OutboundConnectError::Rejected(format!(
            "SOCKS5 CONNECT 返回 {:#04x}",
            header[1]
        )));
    }
    let addressBytes = match header[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut length = [0_u8; 1];
            stream.read_exact(&mut length).await?;
            length[0] as usize
        }
        addressType => {
            return Err(OutboundConnectError::Protocol(format!(
                "SOCKS5 地址类型无效 {addressType:#04x}"
            )));
        }
    };
    let mut ignored = vec![0_u8; addressBytes + 2];
    stream.read_exact(&mut ignored).await?;
    Ok(())
}

/// 格式化 CONNECT authority；IPv6 字面量必须加方括号，域名和 IPv4 保持原样。
fn formatAuthority(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
