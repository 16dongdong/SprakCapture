use std::{
    future::Future,
    io,
    net::IpAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use http::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use tower_service::Service;
use transport_core::OutboundConnector;

use crate::DnsSpoofingTool;

/// 让 Hyper 连接池复用统一二级代理建连器，并保留连接元数据契约。
pub(crate) struct ProxyIo(TokioIo<tokio::net::TcpStream>);

impl Connection for ProxyIo {
    /// 声明普通 TCP 连接；TLS 包装层会在 HTTPS 请求上继续完成握手。
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

impl hyper::rt::Read for ProxyIo {
    /// 委托 Tokio 适配器读取并保持 Hyper 的未初始化缓冲区契约。
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        hyper::rt::Read::poll_read(Pin::new(&mut self.get_mut().0), context, buffer)
    }
}

impl hyper::rt::Write for ProxyIo {
    /// 委托 Tokio 适配器写入，不在二级代理隧道上增加用户态缓存。
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        hyper::rt::Write::poll_write(Pin::new(&mut self.get_mut().0), context, bytes)
    }

    /// 刷新底层连接并传播原始 I/O 失败。
    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        hyper::rt::Write::poll_flush(Pin::new(&mut self.get_mut().0), context)
    }

    /// 有序关闭写方向，使连接池不会复用半关闭隧道。
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        hyper::rt::Write::poll_shutdown(Pin::new(&mut self.get_mut().0), context)
    }

    /// 暴露底层向量写能力，避免 HTTP 多缓冲写入退化。
    fn is_write_vectored(&self) -> bool {
        hyper::rt::Write::is_write_vectored(&self.0)
    }

    /// 将向量写调用直接转发给 Tokio 适配器。
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        hyper::rt::Write::poll_write_vectored(Pin::new(&mut self.get_mut().0), context, buffers)
    }
}

/// 把 URI 目标转换为直接连接或 HTTP/SOCKS5 二级代理隧道。
#[derive(Clone)]
pub(crate) struct ProxyConnector {
    outbound: OutboundConnector,
    dnsSpoofing: Arc<DnsSpoofingTool>,
    fixedTarget: Option<FixedConnectTarget>,
}

/// 把透明连接的逻辑域名固定到 WinDivert 已确认的原始 IP，同时保留 URI 域名供 TLS SNI 使用。
#[derive(Clone)]
pub(crate) struct FixedConnectTarget {
    pub host: String,
    pub port: u16,
    pub address: IpAddr,
}

impl ProxyConnector {
    /// 绑定不可变出站策略与可热更新 DNS 工具；二级代理启用时域名交给代理端解析。
    pub fn new(outbound: OutboundConnector, dnsSpoofing: Arc<DnsSpoofingTool>) -> Self {
        Self {
            outbound,
            dnsSpoofing,
            fixedTarget: None,
        }
    }

    /// 创建带单一透明目标绑定的连接器；仅完全匹配主机和端口时覆盖建连地址。
    ///
    /// 运行上下文：透明 HTTP/HTTPS 接管会重新建立上游连接，必须保持原始 IP，同时仍用逻辑域名生成 Host、SNI 与证书规则。
    /// 失败语义：不匹配的请求继续遵循普通二级代理与 DNS 规则，不会把固定地址扩散到其他目标。
    pub fn newWithFixedTarget(
        outbound: OutboundConnector,
        dnsSpoofing: Arc<DnsSpoofingTool>,
        fixedTarget: FixedConnectTarget,
    ) -> Self {
        Self {
            outbound,
            dnsSpoofing,
            fixedTarget: Some(fixedTarget),
        }
    }
}

impl Service<Uri> for ProxyConnector {
    type Response = ProxyIo;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    /// 连接器不维护独占容量，始终可由 Hyper 发起下一次建连。
    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    /// 解析 URI authority 并建立完整隧道；缺失主机时返回 InvalidInput 而非发往默认地址。
    fn call(&mut self, uri: Uri) -> Self::Future {
        let outbound = self.outbound.clone();
        let dnsSpoofing = self.dnsSpoofing.clone();
        let fixedTarget = self.fixedTarget.clone();
        Box::pin(async move {
            let host = uri
                .host()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "上游 URI 缺少主机"))?;
            let port = uri.port_u16().unwrap_or_else(|| {
                if uri.scheme_str() == Some("https") {
                    443
                } else {
                    80
                }
            });
            let resolvedHost = if let Some(target) = fixedTarget
                .filter(|target| target.port == port && target.host.eq_ignore_ascii_case(host))
            {
                target.address.to_string()
            } else if outbound.usesUpstreamProxy() {
                host.to_owned()
            } else {
                dnsSpoofing
                    .resolveIp(host)
                    .map_or_else(|| host.to_owned(), |address| address.to_string())
            };
            let stream = outbound
                .connect(&resolvedHost, port)
                .await
                .map_err(io::Error::other)?;
            Ok(ProxyIo(TokioIo::new(stream)))
        })
    }
}
