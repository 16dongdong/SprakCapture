use capture_core::HeaderField;
use http::{
    HeaderMap, HeaderValue, Method, Uri, Version,
    header::{
        CONNECTION, CONTENT_ENCODING, CONTENT_TYPE, HOST, HeaderName, PROXY_AUTHENTICATE,
        PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING, UPGRADE,
    },
    uri::Authority,
};
use hyper::{Request, body::Incoming};
use location_core::ResolvedLocation;

use crate::{error::RequestFailure, pipeline::RequestDraft};

/// 保存已解码 HTTP 消息的绝对上游目标和录制 Location。
pub(crate) struct HttpTarget {
    pub upstreamUri: Uri,
    pub hostHeader: HeaderValue,
    pub location: ResolvedLocation,
}

/// 保存 CONNECT 的 authority、连接参数和隧道录制 Location。
#[derive(Clone)]
pub(crate) struct ConnectTarget {
    pub host: String,
    pub port: u16,
    pub location: ResolvedLocation,
}

/// 解析 absolute-form 或 Host+origin-form 请求，输出 Hyper 客户端可连接的绝对 URI。
pub(crate) fn parseHttpTarget(request: &Request<Incoming>) -> Result<HttpTarget, RequestFailure> {
    let incomingUri = request.uri();
    let upstreamUri = if incomingUri.scheme().is_some() {
        incomingUri.clone()
    } else {
        buildAbsoluteUri(incomingUri, request.headers())?
    };
    if upstreamUri.scheme_str() != Some("http") {
        return Err(RequestFailure::UnsupportedScheme);
    }
    let authority = upstreamUri
        .authority()
        .ok_or(RequestFailure::InvalidRequest)?;
    let host = authority.host().to_owned();
    if host.is_empty() {
        return Err(RequestFailure::InvalidRequest);
    }
    let port = authority.port_u16().unwrap_or(80);
    let pathAndQuery = upstreamUri
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    // 明文代理也可能通过 HTTP/2 上游发送；发送前统一 URI authority 与 Host 的默认端口表示，
    // 避免严格虚拟主机把语义相同但文本不同的目标判为冲突请求。
    let upstreamUri = Uri::builder()
        .scheme("http")
        .authority(canonicalAuthority(&host, port, 80)?)
        .path_and_query(pathAndQuery)
        .build()
        .map_err(|_| RequestFailure::InvalidRequest)?;
    let hostHeader = canonicalHostHeader(&host, port, 80)?;
    let path = upstreamUri.path().to_owned();
    if !path.starts_with('/') {
        return Err(RequestFailure::InvalidRequest);
    }
    let query = upstreamUri.query().unwrap_or_default().to_owned();
    Ok(HttpTarget {
        hostHeader,
        location: ResolvedLocation {
            protocol: "http".to_owned(),
            host,
            port,
            path,
            query,
            display: upstreamUri.to_string(),
        },
        upstreamUri,
    })
}

/// 把解密请求绑定回 CONNECT authority，禁止客户端跨主机复用隧道。
///
/// HTTP/1.x 在隧道内使用 origin-form，HTTP/2 则必须携带 `:scheme` 与 `:authority`，Hyper 会把
/// 后两者还原进 URI。过去统一拒绝 URI authority，导致已成功协商 h2 的真实客户端直接收到 400。
/// 这里仅允许 HTTP/2 使用与 CONNECT 完全一致的 HTTPS authority，既保留协议兼容性，也不放宽目标边界。
pub(crate) fn parseHttpsTarget(
    request: &Request<Incoming>,
    connectHost: &str,
    connectPort: u16,
) -> Result<HttpTarget, RequestFailure> {
    validateHttpsRequestTarget(request, connectHost, connectPort)?;
    let pathAndQuery = request
        .uri()
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    // HTTP/2 会同时把 URI authority 编码为 `:authority`，并保留下面生成的 Host。
    // 默认端口只出现在其中一侧时，部分严格 CDN 会把两个 authority 判为冲突并返回 400。
    let authority = canonicalAuthority(connectHost, connectPort, 443)?;
    let upstreamUri = Uri::builder()
        .scheme("https")
        .authority(authority)
        .path_and_query(pathAndQuery.clone())
        .build()
        .map_err(|_| RequestFailure::InvalidRequest)?;
    let hostHeader = canonicalHostHeader(connectHost, connectPort, 443)?;
    Ok(HttpTarget {
        upstreamUri: upstreamUri.clone(),
        hostHeader,
        location: ResolvedLocation {
            protocol: "https".to_owned(),
            host: connectHost.to_owned(),
            port: connectPort,
            path: pathAndQuery.path().to_owned(),
            query: pathAndQuery.query().unwrap_or_default().to_owned(),
            display: upstreamUri.to_string(),
        },
    })
}

/// 校验隧道内不同 HTTP 版本允许携带的目标形式；HTTP/2 authority 必须与 CONNECT 目标一致。
fn validateHttpsRequestTarget(
    request: &Request<Incoming>,
    connectHost: &str,
    connectPort: u16,
) -> Result<(), RequestFailure> {
    let uri = request.uri();
    if request.version() != Version::HTTP_2 {
        return if uri.scheme().is_none() && uri.authority().is_none() {
            Ok(())
        } else {
            Err(RequestFailure::InvalidRequest)
        };
    }
    if uri.scheme_str() != Some("https") {
        return Err(RequestFailure::InvalidRequest);
    }
    let authority = uri.authority().ok_or(RequestFailure::InvalidRequest)?;
    let authorityPort = authority.port_u16().unwrap_or(443);
    if !authority.host().eq_ignore_ascii_case(connectHost) || authorityPort != connectPort {
        return Err(RequestFailure::InvalidRequest);
    }
    Ok(())
}

/// 根据工具流水线改写后的绝对 URI 重建上游目标；只接受 HTTP/HTTPS，避免工具把代理带离受支持传输边界。
pub(crate) fn parsePipelineTarget(request: &RequestDraft) -> Result<HttpTarget, RequestFailure> {
    let requestedUri = request.uri.clone();
    let scheme = requestedUri
        .scheme_str()
        .ok_or(RequestFailure::InvalidRequest)?;
    let defaultPort = match scheme {
        "http" => 80,
        "https" => 443,
        _ => return Err(RequestFailure::UnsupportedScheme),
    };
    let authority = requestedUri
        .authority()
        .ok_or(RequestFailure::InvalidRequest)?;
    let host = authority.host().to_owned();
    if host.is_empty() {
        return Err(RequestFailure::InvalidRequest);
    }
    let port = authority.port_u16().unwrap_or(defaultPort);
    let path = requestedUri.path().to_owned();
    if !path.starts_with('/') {
        return Err(RequestFailure::InvalidRequest);
    }
    // Rewrite/Map Remote 可以重新构造绝对 URI；发送前再次规范化默认端口，保证后续 Host 与
    // HTTP/2 `:authority` 使用完全相同的文本表示，而不仅是语义上等价。
    let upstreamUri = Uri::builder()
        .scheme(scheme)
        .authority(canonicalAuthority(&host, port, defaultPort)?)
        .path_and_query(
            requestedUri
                .path_and_query()
                .cloned()
                .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/")),
        )
        .build()
        .map_err(|_| RequestFailure::InvalidRequest)?;
    Ok(HttpTarget {
        hostHeader: canonicalHostHeader(&host, port, defaultPort)?,
        location: ResolvedLocation {
            protocol: scheme.to_owned(),
            host,
            port,
            path,
            query: requestedUri.query().unwrap_or_default().to_owned(),
            display: upstreamUri.to_string(),
        },
        upstreamUri,
    })
}

/// 按 HTTP authority 规则生成上游 authority；协议默认端口省略，IPv6 始终保留方括号。
///
/// 运行上下文：URI `:authority` 与 Host 必须复用此函数，避免 HTTP/2 严格上游把默认端口的
/// 两种文本形式判为不同目标。`host` 或端口无法构成合法 authority 时返回无效请求错误。
pub fn canonicalAuthority(
    host: &str,
    port: u16,
    defaultPort: u16,
) -> Result<Authority, RequestFailure> {
    let authorityHost = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let value = if port == defaultPort {
        authorityHost
    } else {
        format!("{authorityHost}:{port}")
    };
    value
        .parse::<Authority>()
        .map_err(|_| RequestFailure::InvalidRequest)
}

/// 构造标准上游 Host 请求头；默认端口省略，IPv6 主机始终使用方括号。
///
/// 该函数与 `canonicalAuthority` 共用唯一规范化结果；非法主机或端口返回无效请求错误。
pub fn canonicalHostHeader(
    host: &str,
    port: u16,
    defaultPort: u16,
) -> Result<HeaderValue, RequestFailure> {
    let authority = canonicalAuthority(host, port, defaultPort)?;
    HeaderValue::from_str(authority.as_str()).map_err(|_| RequestFailure::InvalidRequest)
}

/// 解析 CONNECT authority-form；端口必须显式存在，避免错误默认到明文 HTTP 端口。
pub(crate) fn parseConnectTarget(
    request: &Request<Incoming>,
) -> Result<ConnectTarget, RequestFailure> {
    let authority = request
        .uri()
        .authority()
        .cloned()
        .or_else(|| request.uri().to_string().parse::<Authority>().ok())
        .ok_or(RequestFailure::InvalidRequest)?;
    let port = authority.port_u16().ok_or(RequestFailure::InvalidRequest)?;
    let host = authority.host().to_owned();
    if host.is_empty() {
        return Err(RequestFailure::InvalidRequest);
    }
    let display = authority.to_string();
    Ok(ConnectTarget {
        host: host.clone(),
        port,
        location: ResolvedLocation {
            protocol: "https".to_owned(),
            host,
            port,
            path: String::new(),
            query: String::new(),
            display,
        },
    })
}

/// 将 origin-form URI 与 Host 头组合为绝对 URI；非法 Host 或路径返回 InvalidRequest。
fn buildAbsoluteUri(uri: &Uri, headers: &HeaderMap) -> Result<Uri, RequestFailure> {
    if !uri.path().starts_with('/') {
        return Err(RequestFailure::InvalidRequest);
    }
    let authority = headers
        .get(HOST)
        .ok_or(RequestFailure::InvalidRequest)?
        .to_str()
        .map_err(|_| RequestFailure::InvalidRequest)?
        .parse::<Authority>()
        .map_err(|_| RequestFailure::InvalidRequest)?;
    Uri::builder()
        .scheme("http")
        .authority(authority)
        .path_and_query(
            uri.path_and_query()
                .cloned()
                .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/")),
        )
        .build()
        .map_err(|_| RequestFailure::InvalidRequest)
}

/// 复制 HeaderMap 为保序多值列表；非法 UTF-8 使用无损替换字符供查看器展示。
pub(crate) fn captureHeaders(headers: &HeaderMap) -> Vec<HeaderField> {
    headers
        .iter()
        .map(|(name, value)| HeaderField {
            name: name.as_str().to_owned(),
            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
        })
        .collect()
}

/// 返回响应 Content-Type；缺失或非法值使用空字符串表示未知。
pub(crate) fn contentType(headers: &HeaderMap) -> String {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// 返回正文 Content-Encoding；缺失或非法值使用空字符串表示未声明编码。
pub(crate) fn contentEncoding(headers: &HeaderMap) -> String {
    headers
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// 计算请求行和头字段的线上近似字节数，供事务大小列稳定比较。
pub(crate) fn requestHeaderBytes(
    method: &Method,
    uri: &Uri,
    version: Version,
    headers: &HeaderMap,
) -> u64 {
    method.as_str().len() as u64
        + 1
        + uri.to_string().len() as u64
        + 1
        + versionText(version).len() as u64
        + 2
        + fieldBytes(headers)
        + 2
}

/// 计算状态行和响应头字段的线上近似字节数。
pub(crate) fn responseHeaderBytes(
    status: http::StatusCode,
    version: Version,
    headers: &HeaderMap,
) -> u64 {
    versionText(version).len() as u64
        + 1
        + 3
        + status
            .canonical_reason()
            .map_or(1, |reason| reason.len() as u64 + 1)
        + 2
        + fieldBytes(headers)
        + 2
}

/// 删除 RFC hop-by-hop 字段以及 Connection 声明的扩展字段，防止跨连接错误转发。
pub(crate) fn removeHopByHopHeaders(headers: &mut HeaderMap) {
    let declaredHeaders = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect::<Vec<_>>();
    for headerName in declaredHeaders {
        headers.remove(headerName);
    }
    for headerName in [
        CONNECTION,
        HeaderName::from_static("keep-alive"),
        PROXY_AUTHENTICATE,
        PROXY_AUTHORIZATION,
        TE,
        TRAILER,
        TRANSFER_ENCODING,
        UPGRADE,
        HeaderName::from_static("proxy-connection"),
    ] {
        headers.remove(headerName);
    }
}

/// 汇总多值头字段的序列化字节数，不合并重复字段。
fn fieldBytes(headers: &HeaderMap) -> u64 {
    headers
        .iter()
        .map(|(name, value)| name.as_str().len() as u64 + value.as_bytes().len() as u64 + 4)
        .sum()
}

/// 将 Hyper Version 映射到请求行使用的稳定协议文本。
fn versionText(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2.0",
        Version::HTTP_3 => "HTTP/3.0",
        _ => "HTTP/?",
    }
}
