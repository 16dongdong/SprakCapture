use std::{future::Future, io, net::IpAddr, pin::Pin, str::FromStr, time::Duration};

use capture_core::RecordingSession;
use http::uri::{Authority, Uri};
use http_proxy_core::{
    HttpProxyConfig, HttpProxyDependencies, HttpProxyError, SocksHttpTarget,
    SocksHttpTunnelHandler, SslMitmManager, ToolPipeline,
};
use location_core::ResolvedLocation;
use plugin_host::PluginHost;
use socks5_core::{
    SessionApplicationProtocol,
    interception::{TcpTunnel, TcpTunnelDisposition, TcpTunnelInterceptor},
};
use tokio::{
    net::TcpStream,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

const transparentHostProbeTimeout: Duration = Duration::from_millis(5);
const retryDelay: Duration = Duration::from_millis(5);
const maximumMethodBytes: usize = 32;

/// 连接分类后的协议处理归属；携带逻辑域名的分支会在交给 HTTP 处理器或原始 TLS 录制前更新透明目标身份。
#[derive(Clone, Debug, Eq, PartialEq)]
enum TunnelClassification {
    Raw,
    RawTls { logicalHost: Option<String> },
    Http { logicalHost: Option<String> },
    Https { logicalHost: Option<String> },
    NeedMoreBytes,
}

/// 将 SOCKS5 CONNECT 的首段字节接入既有 HTTP 录制与 SSL 解密链路，保持原始 TCP/UDP 热路径零额外复制。
///
/// 运行上下文：控制服务构造一个实例并交给 SOCKS5 核心；核心已完成目标连接与成功响应，本模块仅通过 `peek` 分类，绝不消费原始字节。
/// 参数：构造参数与 HTTP 监听器共用同一份配置、录制会话、证书管理器、工具流水线和插件宿主，保证两种入口生成相同事务语义。
/// 失败语义：初始化失败返回 HTTP 配置错误；会话期 I/O、TLS 或协议处理失败返回 `io::Error` 并由 SOCKS5 生命周期收束连接。
#[derive(Clone)]
pub struct SocksHttpInspector {
    handler: SocksHttpTunnelHandler,
    ssl: SslMitmManager,
    probeCapacityBytes: usize,
    classificationTimeout: Duration,
}

impl SocksHttpInspector {
    /// 创建与普通 HTTP 监听器复用转发上下文的 SOCKS5 隧道分类器。
    ///
    /// 运行上下文：服务启动时只创建一次，上游客户端池可以被多个 SOCKS5 会话复用；该对象不持有监听端口或会话状态。
    /// 参数：`config` 决定请求预算和连接超时，其余参数分别提供录制、解密、工具与插件能力。
    /// 失败语义：HTTP 配置或上游 TLS 客户端配置无效时返回 `HttpProxyError`，调用方必须把 SOCKS5 监听器标记为启动失败而非半功能运行。
    pub fn new(
        config: HttpProxyConfig,
        capture: RecordingSession,
        ssl: SslMitmManager,
        pipeline: ToolPipeline,
        pluginHost: PluginHost,
    ) -> Result<Self, HttpProxyError> {
        let probeCapacityBytes = config.maxHeaderBytes;
        let classificationTimeout = config.headerReadTimeout();
        let handler =
            SocksHttpTunnelHandler::new(config, capture, ssl.clone(), pipeline, pluginHost)?;
        Ok(Self {
            handler,
            ssl,
            probeCapacityBytes,
            classificationTimeout,
        })
    }

    /// 创建与 SOCKS5 预连接共享 DNS 映射器的 HTTP/HTTPS 分类处理器。
    ///
    /// 运行上下文：控制服务启动 SOCKS5 监听器时使用；每个接管后的上游连接都读取最新规则。
    /// 失败语义：HTTP 或 TLS 客户端初始化失败时返回错误，监听器不得进入部分可用状态。
    pub fn newWithDns(
        config: HttpProxyConfig,
        dependencies: HttpProxyDependencies,
    ) -> Result<Self, HttpProxyError> {
        let ssl = dependencies.ssl.clone();
        let probeCapacityBytes = config.maxHeaderBytes;
        let classificationTimeout = config.headerReadTimeout();
        let handler = SocksHttpTunnelHandler::newWithDns(config, dependencies)?;
        Ok(Self {
            handler,
            ssl,
            probeCapacityBytes,
            classificationTimeout,
        })
    }

    /// 排空分类器接管后派生的 HTTP/TLS 任务，保证融合监听停止后不再写入录制状态。
    ///
    /// 运行上下文：所有外层 SOCKS5 会话均已退出后调用；实际超时预算由融合监听统一管理。
    pub async fn shutdown(&self) {
        self.handler.shutdown().await;
    }

    /// 强制中止超出停机预算的内部任务并等待析构；仅由融合监听超时路径调用。
    pub async fn abortAndWait(&self) {
        self.handler.abortAndWait().await;
    }

    /// 从透明连接尚未消费的 HTTP Host 或 TLS SNI 恢复逻辑主机名。
    ///
    /// 运行上下文：WinDivert 只能提供原始目标 IP；这里仅做不阻塞建连的快速预探测，完整或延迟到达的头部会在隧道分类阶段再次解析。
    /// 参数：`stream` 是透明连接客户端套接字，`originalIp` 是内核确认并用于固定实际路由的原始目标。
    /// 失败语义：取消、探测超时、格式无效或头部尚未完整时返回 IP 字面量；Host/SNI 永远不会改变真实连接地址。
    pub async fn resolveTransparentHost(
        &self,
        stream: &TcpStream,
        originalIp: IpAddr,
        cancellation: &CancellationToken,
    ) -> io::Result<String> {
        let fallback = originalIp.to_string();
        let mut probe = vec![0_u8; self.probeCapacityBytes];
        let readiness = tokio::select! {
            _ = cancellation.cancelled() => return Ok(fallback),
            result = timeout(transparentHostProbeTimeout, stream.readable()) => result,
        };
        match readiness {
            Ok(ready) => ready?,
            Err(_) => return Ok(fallback),
        }
        let byteCount = stream.peek(&mut probe).await?;
        match extractLogicalHost(&probe[..byteCount], self.probeCapacityBytes) {
            HostExtraction::Host(host) => Ok(host),
            HostExtraction::NeedMoreBytes | HostExtraction::Absent => Ok(fallback),
        }
    }
}

/// 描述透明连接逻辑主机名探测结果，区分尚需等待与已经确定不存在。
#[derive(Debug, Eq, PartialEq)]
enum HostExtraction {
    NeedMoreBytes,
    Absent,
    Host(String),
}

/// 从 HTTP/1 Host 或 TLS ClientHello SNI 中提取透明连接的逻辑主机名。
///
/// 运行上下文：输入来自 `TcpStream::peek`，函数不得消费字节；仅识别能够完整验证边界的两种协议前缀。
/// 参数：`prefix` 是当前连续前缀，`probeCapacityBytes` 是本连接允许等待的最大请求头长度。
/// 失败语义：前缀尚不完整返回 `NeedMoreBytes`，协议或字段无效返回 `Absent`，调用方随后保持 IP 原目标。
fn extractLogicalHost(prefix: &[u8], probeCapacityBytes: usize) -> HostExtraction {
    if prefix.first() == Some(&0x16) {
        return extractTlsServerName(prefix, probeCapacityBytes);
    }
    extractHttpHost(prefix, probeCapacityBytes)
}

/// 从完整 HTTP/1 请求头中读取唯一 Host 字段。
///
/// 运行上下文：透明转发只需要恢复逻辑域名，不在这里接管或改写请求；后续分类仍会执行完整协议验证。
/// 参数：`prefix` 是未消费请求前缀，`probeCapacityBytes` 与 HTTP 服务配置的请求头上限一致。
/// 失败语义：头部不完整返回 `NeedMoreBytes`；重复、缺失或非法 Host 返回 `Absent`。
fn extractHttpHost(prefix: &[u8], probeCapacityBytes: usize) -> HostExtraction {
    let methodLength = match scanHttpMethod(prefix) {
        HttpMethodScan::Complete(methodLength) => methodLength,
        HttpMethodScan::NeedMoreBytes => return HostExtraction::NeedMoreBytes,
        HttpMethodScan::Invalid => return HostExtraction::Absent,
    };
    if prefix.len() <= methodLength || prefix[methodLength] != b' ' {
        return HostExtraction::NeedMoreBytes;
    }
    let Some(headerEnd) = findHeaderEnd(prefix) else {
        return if prefix.len() == probeCapacityBytes {
            HostExtraction::Absent
        } else {
            HostExtraction::NeedMoreBytes
        };
    };
    let Ok(headerText) = std::str::from_utf8(&prefix[..headerEnd]) else {
        return HostExtraction::Absent;
    };
    let mut hostValues = headerText.split("\r\n").skip(1).filter_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("host").then_some(value.trim())
    });
    let Some(hostValue) = hostValues.next() else {
        return HostExtraction::Absent;
    };
    if hostValues.next().is_some() {
        return HostExtraction::Absent;
    }
    let Ok(authority) = Authority::from_str(hostValue) else {
        return HostExtraction::Absent;
    };
    HostExtraction::Host(authority.host().to_owned())
}

/// 从单个完整 ClientHello 记录中读取第一个 host_name 类型的 SNI。
///
/// 运行上下文：仅解析 TLS 记录和握手的公开长度字段，不接受跨记录 ClientHello，以保持探测有界且不复制协议状态机。
/// 参数：`prefix` 是未消费 TLS 前缀，`probeCapacityBytes` 限制声明长度，防止异常记录放大内存等待。
/// 失败语义：记录尚未完整返回 `NeedMoreBytes`；长度越界、非 ClientHello、缺失或非 UTF-8 SNI 返回 `Absent`。
fn extractTlsServerName(prefix: &[u8], probeCapacityBytes: usize) -> HostExtraction {
    if prefix.len() < 5 {
        return HostExtraction::NeedMoreBytes;
    }
    if prefix[0] != 0x16 || prefix[1] != 0x03 || !(0x01..=0x04).contains(&prefix[2]) {
        return HostExtraction::Absent;
    }
    let recordLength = usize::from(u16::from_be_bytes([prefix[3], prefix[4]]));
    let recordEnd = 5_usize.saturating_add(recordLength);
    if recordEnd > probeCapacityBytes {
        return HostExtraction::Absent;
    }
    if prefix.len() < recordEnd {
        return HostExtraction::NeedMoreBytes;
    }
    let record = &prefix[5..recordEnd];
    if record.len() < 4 || record[0] != 0x01 {
        return HostExtraction::Absent;
    }
    let handshakeLength =
        (usize::from(record[1]) << 16) | (usize::from(record[2]) << 8) | usize::from(record[3]);
    if handshakeLength.saturating_add(4) > record.len() {
        return HostExtraction::Absent;
    }
    let hello = &record[4..4 + handshakeLength];
    let Some(mut cursor) = 34_usize.checked_add(1) else {
        return HostExtraction::Absent;
    };
    if hello.len() < cursor {
        return HostExtraction::Absent;
    }
    cursor = cursor.saturating_add(usize::from(hello[34]));
    let Some(cipherLength) = readU16(hello, cursor) else {
        return HostExtraction::Absent;
    };
    cursor = cursor.saturating_add(2).saturating_add(cipherLength);
    let Some(compressionLength) = hello.get(cursor).copied().map(usize::from) else {
        return HostExtraction::Absent;
    };
    cursor = cursor.saturating_add(1).saturating_add(compressionLength);
    let Some(extensionsLength) = readU16(hello, cursor) else {
        return HostExtraction::Absent;
    };
    cursor = cursor.saturating_add(2);
    let extensionsEnd = cursor.saturating_add(extensionsLength);
    if extensionsEnd > hello.len() {
        return HostExtraction::Absent;
    }
    while cursor < extensionsEnd {
        let Some(extensionType) = readU16(hello, cursor) else {
            return HostExtraction::Absent;
        };
        let Some(extensionLength) = readU16(hello, cursor + 2) else {
            return HostExtraction::Absent;
        };
        cursor = cursor.saturating_add(4);
        let extensionEnd = cursor.saturating_add(extensionLength);
        if extensionEnd > extensionsEnd {
            return HostExtraction::Absent;
        }
        if extensionType == 0 {
            return extractServerNameExtension(&hello[cursor..extensionEnd]);
        }
        cursor = extensionEnd;
    }
    HostExtraction::Absent
}

/// 读取 SNI 扩展中的唯一首个 host_name 条目，并拒绝长度不闭合的数据。
fn extractServerNameExtension(extension: &[u8]) -> HostExtraction {
    let Some(listLength) = readU16(extension, 0) else {
        return HostExtraction::Absent;
    };
    if listLength.saturating_add(2) != extension.len() {
        return HostExtraction::Absent;
    }
    let mut cursor = 2;
    while cursor < extension.len() {
        let Some(nameType) = extension.get(cursor).copied() else {
            return HostExtraction::Absent;
        };
        let Some(nameLength) = readU16(extension, cursor + 1) else {
            return HostExtraction::Absent;
        };
        cursor = cursor.saturating_add(3);
        let nameEnd = cursor.saturating_add(nameLength);
        if nameEnd > extension.len() {
            return HostExtraction::Absent;
        }
        if nameType == 0 {
            let Ok(host) = std::str::from_utf8(&extension[cursor..nameEnd]) else {
                return HostExtraction::Absent;
            };
            return Authority::from_str(host)
                .map(|authority| HostExtraction::Host(authority.host().to_owned()))
                .unwrap_or(HostExtraction::Absent);
        }
        cursor = nameEnd;
    }
    HostExtraction::Absent
}

/// 按网络字节序读取长度字段；越界时返回空值供上层拒绝该前缀。
fn readU16(bytes: &[u8], offset: usize) -> Option<usize> {
    let pair = bytes.get(offset..offset.checked_add(2)?)?;
    Some(usize::from(u16::from_be_bytes([pair[0], pair[1]])))
}

impl TcpTunnelInterceptor for SocksHttpInspector {
    /// 分类并接管 SOCKS5 CONNECT 隧道；原始流完整返回给核心，HTTP/HTTPS 则交给共享事务处理器。
    ///
    /// 运行上下文：该 future 独占两个套接字；分类只调用 `peek`，因此 `Raw` 分支不会丢失任何首段字节或破坏高频 TCP 数据流。
    /// 参数：`tunnel` 包含 SOCKS5 请求目标、客户端地址、双端套接字和会话取消信号。
    /// 失败语义：已接管的 HTTP/HTTPS I/O 错误直接结束会话；分类超时、非 HTTP 首字节、目标不一致或未命中 SSL 规则均返回原始 TCP。
    fn intercept(
        &self,
        tunnel: TcpTunnel,
    ) -> Pin<Box<dyn Future<Output = io::Result<TcpTunnelDisposition>> + Send>> {
        let handler = self.handler.clone();
        let ssl = self.ssl.clone();
        let probeCapacityBytes = self.probeCapacityBytes;
        let classificationTimeout = self.classificationTimeout;
        Box::pin(async move {
            let classification =
                classifyTunnel(&tunnel, &ssl, probeCapacityBytes, classificationTimeout).await?;
            match classification {
                TunnelClassification::Raw | TunnelClassification::NeedMoreBytes => {
                    Ok(TcpTunnelDisposition::Raw {
                        tunnel: Box::new(tunnel),
                        applicationProtocol: SessionApplicationProtocol::Tcp,
                    })
                }
                // 未命中 SSL 解密规则的 TLS 仍保持零消费原始中继，但必须把已确认协议交给录制投影用于准确展示。
                TunnelClassification::RawTls { logicalHost } => {
                    let mut tunnel = tunnel;
                    if let Some(logicalHost) = logicalHost {
                        tunnel.targetHost = logicalHost;
                    }
                    Ok(TcpTunnelDisposition::Raw {
                        tunnel: Box::new(tunnel),
                        applicationProtocol: SessionApplicationProtocol::Tls,
                    })
                }
                TunnelClassification::Http { logicalHost } => {
                    let mut tunnel = tunnel;
                    if let Some(logicalHost) = logicalHost {
                        tunnel.targetHost = logicalHost;
                    }
                    let TcpTunnel {
                        clientStream,
                        remoteStream,
                        clientAddress,
                        clientProcessName,
                        clientProcessId,
                        cancellation,
                        targetHost,
                        connectHost,
                        targetPort,
                        ..
                    } = tunnel;
                    // HTTP 转发器会按 Host 重新建立受控上游连接，必须先关闭 SOCKS5 预连接，避免留下无读取者的半开套接字。
                    drop(remoteStream);
                    handler
                        .servePlainHttp(
                            clientStream,
                            clientAddress,
                            SocksHttpTarget {
                                host: targetHost,
                                port: targetPort,
                                fixedAddress: connectHost.parse().ok(),
                                clientProcessName,
                                clientProcessId,
                            },
                            cancellation,
                        )
                        .await?;
                    Ok(TcpTunnelDisposition::Handled(
                        SessionApplicationProtocol::Http,
                    ))
                }
                TunnelClassification::Https { logicalHost } => {
                    let mut tunnel = tunnel;
                    if let Some(logicalHost) = logicalHost {
                        tunnel.targetHost = logicalHost;
                    }
                    let TcpTunnel {
                        clientStream,
                        remoteStream,
                        clientAddress,
                        clientProcessName,
                        clientProcessId,
                        targetHost,
                        connectHost,
                        targetPort,
                        cancellation,
                        ..
                    } = tunnel;
                    // TLS 解密器同样自己建立严格验证的上游 TLS；SOCKS5 预连接只用于确认目标可达后再作协议分流。
                    drop(remoteStream);
                    handler
                        .serveInterceptedHttps(
                            clientStream,
                            clientAddress,
                            SocksHttpTarget {
                                host: targetHost,
                                port: targetPort,
                                fixedAddress: connectHost.parse().ok(),
                                clientProcessName,
                                clientProcessId,
                            },
                            cancellation,
                        )
                        .await?;
                    Ok(TcpTunnelDisposition::Handled(
                        SessionApplicationProtocol::Https,
                    ))
                }
            }
        })
    }
}

/// 在不读取任何字节的前提下识别当前 CONNECT 隧道的应用层协议。
///
/// 运行上下文：SOCKS5 已建立目标连接；远端若主动发送字节即视为服务端先发协议并立即保留原始中继。
/// 参数：`tunnel` 提供可 peek 流与目标，`ssl` 决定 TLS 解密，探测容量和超时直接复用 HTTP 头配置。
/// 失败语义：读取就绪或 peek 失败返回 I/O 错误；超时和不完整前缀返回 `Raw`，绝不向客户端注入协议错误。
async fn classifyTunnel(
    tunnel: &TcpTunnel,
    ssl: &SslMitmManager,
    probeCapacityBytes: usize,
    classificationTimeout: Duration,
) -> io::Result<TunnelClassification> {
    let deadline = Instant::now() + classificationTimeout;
    let mut probe = vec![0_u8; probeCapacityBytes];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(TunnelClassification::Raw);
        }
        tokio::select! {
            _ = tunnel.cancellation.cancelled() => {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "隧道协议分类已取消"));
            }
            _ = sleep(remaining) => {
                return Ok(TunnelClassification::Raw);
            }
            remoteReady = tunnel.remoteStream.readable() => {
                remoteReady?;
                return Ok(TunnelClassification::Raw);
            }
            clientReady = tunnel.clientStream.readable() => {
                clientReady?;
                let byteCount = tunnel.clientStream.peek(&mut probe).await?;
                if byteCount == 0 {
                    return Ok(TunnelClassification::Raw);
                }
                match classifyClientPrefix(
                    &probe[..byteCount],
                    &tunnel.targetHost,
                    tunnel.targetPort,
                    tunnel.routePinned,
                    ssl,
                    probeCapacityBytes,
                ) {
                    TunnelClassification::NeedMoreBytes => {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            return Ok(TunnelClassification::Raw);
                        }
                        tokio::select! {
                            remoteReady = tunnel.remoteStream.readable() => {
                                remoteReady?;
                                return Ok(TunnelClassification::Raw);
                            }
                            () = sleep(remaining.min(retryDelay)) => {}
                        }
                    }
                    classification => return Ok(classification),
                }
            }
        }
    }
}

/// 根据首段字节和连接来源分类协议；显式代理严格匹配目标，透明连接则从 Host/SNI 恢复应用层域名。
///
/// 运行上下文：仅由 `classifyTunnel` 调用，输入来自 `TcpStream::peek` 的连续前缀，不会改变内核接收缓冲区。
/// 参数：`prefix` 是已到达字节，目标端点来自连接元数据，`routePinned` 表示真实路由不会被应用层域名改变，`ssl` 提供 TLS 规则。
/// 失败语义：无法确定时返回 `NeedMoreBytes`，格式不匹配或 SSL 规则未命中返回 `Raw`，不产生网络副作用。
fn classifyClientPrefix(
    prefix: &[u8],
    targetHost: &str,
    targetPort: u16,
    routePinned: bool,
    ssl: &SslMitmManager,
    probeCapacityBytes: usize,
) -> TunnelClassification {
    let httpClassification = classifyHttpPrefix(
        prefix,
        targetHost,
        targetPort,
        routePinned,
        probeCapacityBytes,
    );
    if httpClassification != TunnelClassification::Raw {
        return httpClassification;
    }
    classifyTlsPrefix(
        prefix,
        targetHost,
        targetPort,
        routePinned,
        ssl,
        probeCapacityBytes,
    )
}

/// 验证 origin-form 或 absolute-form HTTP/1 请求，并恢复透明连接的逻辑域名。
///
/// 运行上下文：透明连接的实际路由已固定为原目标 IP，可以信任语法正确的 Host 作为应用层身份；显式 SOCKS5 仍严格匹配 CONNECT 目标。
/// 参数：`prefix` 是未消费字节，`routePinned` 区分内核透明连接与显式代理连接，`probeCapacityBytes` 与 HTTP 头预算一致。
/// 失败语义：非 HTTP、权威字段冲突或显式目标不一致返回 `Raw`；头部未完整返回 `NeedMoreBytes`，不消费任何字节。
fn classifyHttpPrefix(
    prefix: &[u8],
    targetHost: &str,
    targetPort: u16,
    routePinned: bool,
    probeCapacityBytes: usize,
) -> TunnelClassification {
    const http2Preface: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    // HTTP/2 prior knowledge 的连接前言不携带 authority；后续伪首部仍由共享目标解析器校验。
    let comparedLength = prefix.len().min(http2Preface.len());
    if prefix[..comparedLength] == http2Preface[..comparedLength] {
        return if prefix.len() >= http2Preface.len() {
            TunnelClassification::Http { logicalHost: None }
        } else {
            TunnelClassification::NeedMoreBytes
        };
    }
    match scanHttpMethod(prefix) {
        HttpMethodScan::Complete(_) => {}
        HttpMethodScan::NeedMoreBytes => return TunnelClassification::NeedMoreBytes,
        HttpMethodScan::Invalid => return TunnelClassification::Raw,
    }
    let Some(headerEnd) = findHeaderEnd(prefix) else {
        return if prefix.len() == probeCapacityBytes {
            TunnelClassification::Raw
        } else {
            TunnelClassification::NeedMoreBytes
        };
    };
    let Ok(headerText) = std::str::from_utf8(&prefix[..headerEnd]) else {
        return TunnelClassification::Raw;
    };
    let mut lines = headerText.split("\r\n");
    let Some(requestLine) = lines.next() else {
        return TunnelClassification::Raw;
    };
    let mut requestParts = requestLine.split_ascii_whitespace();
    if requestParts.next().is_none() {
        return TunnelClassification::Raw;
    }
    let Some(requestTarget) = requestParts.next() else {
        return TunnelClassification::Raw;
    };
    let Some(httpVersion) = requestParts.next() else {
        return TunnelClassification::Raw;
    };
    if !matches!(httpVersion, "HTTP/1.0" | "HTTP/1.1") || requestParts.next().is_some() {
        return TunnelClassification::Raw;
    }
    if !requestTarget.starts_with('/') && !requestTarget.starts_with("http://") {
        return TunnelClassification::Raw;
    }
    let requestAuthority = if requestTarget.starts_with("http://") {
        let Ok(uri) = Uri::from_str(requestTarget) else {
            return TunnelClassification::Raw;
        };
        let Some(authority) = uri.authority() else {
            return TunnelClassification::Raw;
        };
        if uri.scheme_str() != Some("http") {
            return TunnelClassification::Raw;
        }
        Some(authority.clone())
    } else {
        None
    };
    let mut hostValues = lines.filter_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("host").then_some(value.trim())
    });
    let hostAuthority = match hostValues.next() {
        Some(hostValue) => match Authority::from_str(hostValue) {
            Ok(authority) => Some(authority),
            Err(_) => return TunnelClassification::Raw,
        },
        None if httpVersion == "HTTP/1.0" => None,
        None => return TunnelClassification::Raw,
    };
    if hostValues.next().is_some() {
        return TunnelClassification::Raw;
    }
    if let (Some(requestAuthority), Some(hostAuthority)) =
        (requestAuthority.as_ref(), hostAuthority.as_ref())
        && !authoritiesEquivalent(requestAuthority, hostAuthority)
    {
        return TunnelClassification::Raw;
    }
    let logicalAuthority = hostAuthority.as_ref().or(requestAuthority.as_ref());
    if let Some(authority) = logicalAuthority {
        if authority.port_u16().unwrap_or(80) != targetPort {
            return TunnelClassification::Raw;
        }
        if !routePinned && !authorityMatchesTarget(authority, targetHost, targetPort) {
            return TunnelClassification::Raw;
        }
    }
    TunnelClassification::Http {
        logicalHost: routePinned
            .then(|| logicalAuthority.map(|authority| authority.host().to_owned()))
            .flatten(),
    }
}

/// 描述 HTTP 方法扫描结果，区分完整 token、仍可继续的分片以及确定无效的二进制前缀。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HttpMethodScan {
    Complete(usize),
    NeedMoreBytes,
    Invalid,
}

/// 扫描任意符合 RFC token 语法的 HTTP 方法，避免只维护少量方法白名单而漏掉 CONNECT、WebDAV 或扩展方法。
///
/// 运行上下文：首段尚未消费；方法长度上限阻止无限等待不含空格的普通文本流，同时覆盖标准和常见扩展方法。
/// 参数：`prefix` 是客户端已到达的连续前缀。
/// 失败语义：出现非法 token 或超过方法上限返回 `Invalid`；尚未出现分隔空格返回 `NeedMoreBytes`。
fn scanHttpMethod(prefix: &[u8]) -> HttpMethodScan {
    for (index, byte) in prefix.iter().copied().enumerate() {
        if byte == b' ' {
            return if index == 0 {
                HttpMethodScan::Invalid
            } else {
                HttpMethodScan::Complete(index)
            };
        }
        if index >= maximumMethodBytes || !isHttpTokenByte(byte) {
            return HttpMethodScan::Invalid;
        }
    }
    HttpMethodScan::NeedMoreBytes
}

/// 判断字节能否出现在 HTTP token 中；该规则同时适用于标准与扩展方法名。
fn isHttpTokenByte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// 在有限探测缓冲区中寻找 HTTP/1 头部终止符。
///
/// 运行上下文：请求头达到终止符后才能安全验证 Host，避免把只包含 `GET /` 的普通 TCP 消息错误接管。
/// 参数：`prefix` 为未消费字节前缀。
/// 失败语义：头部尚不完整时返回 `None`，不会对输入做任何修改。
fn findHeaderEnd(prefix: &[u8]) -> Option<usize> {
    prefix
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

/// 校验已解析 authority 与 SOCKS5 CONNECT 端点的主机和端口是否相同。
///
/// 运行上下文：Host 头和 absolute-form URI 复用同一比较，防止两种请求格式在默认端口或 IPv6 括号上产生不一致结果。
/// 参数：`authority` 为 HTTP 语法已验证的权威端点，目标 host/port 来自 SOCKS5 CONNECT。
/// 失败语义：主机名大小写无关但端口严格相等；任何不一致均返回 `false`。
fn authorityMatchesTarget(authority: &Authority, targetHost: &str, targetPort: u16) -> bool {
    let expectedHost = targetHost.trim_start_matches('[').trim_end_matches(']');
    authority.host().eq_ignore_ascii_case(expectedHost)
        && authority.port_u16().unwrap_or(80) == targetPort
}

/// 比较请求行 absolute-form authority 与 Host 头，防止同一请求声明两个相互冲突的应用层目标。
///
/// 运行上下文：透明路由虽然固定到内核原 IP，但 HTTP 转发和录制仍只能接受一个确定域名。
/// 参数：两个 authority 均已通过 `http` 语法解析；缺省端口按明文 HTTP 的 80 处理。
/// 失败语义：主机名或有效端口不一致返回 `false`，调用方将该连接保留为原始 TCP 而不改写请求。
fn authoritiesEquivalent(left: &Authority, right: &Authority) -> bool {
    left.host().eq_ignore_ascii_case(right.host())
        && left.port_u16().unwrap_or(80) == right.port_u16().unwrap_or(80)
}

/// 判断首段字节是否为 TLS ClientHello，并按连接来源恢复 SNI 后决定是否进入解密链路。
///
/// 运行上下文：显式 SOCKS5 继续使用 CONNECT authority；透明连接的 TCP 目的 IP 已固定，因此 SNI 只作为规则和界面中的应用层域名。
/// 参数：`prefix` 是未消费 TLS 首段，目标端点构造 SSL 规则位置，`probeCapacityBytes` 限制完整 ClientHello 的探测内存。
/// 失败语义：记录头不完整返回 `NeedMoreBytes`；非 TLS 返回 `Raw`，未命中解密规则的有效 TLS 返回 `RawTls`。
fn classifyTlsPrefix(
    prefix: &[u8],
    targetHost: &str,
    targetPort: u16,
    routePinned: bool,
    ssl: &SslMitmManager,
    probeCapacityBytes: usize,
) -> TunnelClassification {
    if prefix.first() != Some(&0x16) {
        return TunnelClassification::Raw;
    }
    if prefix.len() < 3 {
        return TunnelClassification::NeedMoreBytes;
    }
    if prefix[1] != 0x03 || !(0x01..=0x04).contains(&prefix[2]) {
        return TunnelClassification::Raw;
    }
    if prefix.len() < 5 {
        return TunnelClassification::NeedMoreBytes;
    }
    let recordLength = u16::from_be_bytes([prefix[3], prefix[4]]);
    if recordLength == 0 {
        return TunnelClassification::Raw;
    }
    if prefix.len() < 6 {
        return TunnelClassification::NeedMoreBytes;
    }
    if prefix[5] != 0x01 {
        return TunnelClassification::Raw;
    }
    let logicalHost = if routePinned {
        match extractTlsServerName(prefix, probeCapacityBytes) {
            HostExtraction::NeedMoreBytes => return TunnelClassification::NeedMoreBytes,
            HostExtraction::Host(host) => Some(host),
            HostExtraction::Absent => None,
        }
    } else {
        None
    };
    let classifiedHost = logicalHost.as_deref().unwrap_or(targetHost);
    let hostDisplay = if classifiedHost.contains(':') {
        format!("[{classifiedHost}]")
    } else {
        classifiedHost.to_owned()
    };
    let location = ResolvedLocation {
        protocol: "https".to_owned(),
        host: classifiedHost.to_owned(),
        port: targetPort,
        path: String::new(),
        query: String::new(),
        display: format!("https://{hostDisplay}:{targetPort}"),
    };
    if ssl.shouldIntercept(&location) {
        TunnelClassification::Https { logicalHost }
    } else {
        TunnelClassification::RawTls { logicalHost }
    }
}
