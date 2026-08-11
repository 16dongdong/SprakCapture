use std::{future::Future, io, net::SocketAddr, pin::Pin};

use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::model::SessionApplicationProtocol;

/// 为融合监听器提供 HTTP 或透明连接的所有权接管接口。
///
/// 运行上下文：接受循环只窥视首字节，处理器收到的流仍包含全部原始协议数据。
/// 失败语义：处理器自行记录连接级失败；future 结束即代表连接生命周期结束。
pub trait PortProtocolHandler: Send + Sync {
    /// 在读取首字节前判断连接是否必须由协议处理器接管。
    ///
    /// 运行上下文：透明转发连接依赖五元组流表恢复原目标，载荷首字节可能恰好等于 SOCKS5
    /// 版本号，因此必须先按连接身份判定。普通 HTTP 处理器保持默认值，仅接受非 SOCKS5 流量。
    fn claimsConnection(&self, _stream: &TcpStream, _clientAddress: SocketAddr) -> bool {
        false
    }

    fn serve(
        &self,
        stream: TcpStream,
        clientAddress: SocketAddr,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;

    /// 排空处理器内部派生任务，确保融合监听停止后不再残留 CONNECT 或 TLS 会话。
    ///
    /// 失败语义：实现可返回资源排空错误；调用方会把它提升为服务器运行时错误。
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send>> {
        Box::pin(async { Ok(()) })
    }

    /// 强制终止处理器内部派生任务并等待其完成析构；仅在优雅排空超过统一停机预算后调用。
    ///
    /// 运行上下文：接收循环已经停止且连接任务已被中止。默认处理器没有内部任务，因此保持空操作。
    fn abortAndWait(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

/// 保存 SOCKS5 CONNECT 建立后的两端套接字与已完成解析的目标信息。
///
/// 运行上下文：SOCKS5 核心已完成认证、目标连接和成功响应；拦截器只能在这个边界决定交给 HTTP/HTTPS 处理器或返回原始 TCP 中继。
/// 参数：clientStream 与 remoteStream 分别代表客户端和目标端；targetHost 是应用层身份，connectHost 是实际路由，routePinned 表示两者能否分离。
/// 失败语义：拦截器返回 I/O 错误时，SOCKS5 会话终止并由调用方发布失败终态。
pub struct TcpTunnel {
    pub clientStream: TcpStream,
    pub remoteStream: TcpStream,
    pub clientAddress: SocketAddr,
    /// 透明捕获连接对应的本机进程名称；显式代理连接没有可靠的进程归属，因此保持为空。
    pub clientProcessName: Option<String>,
    /// 透明捕获连接对应的本机进程编号，与 WinDivert SOCKET/FLOW 记录保持一致。
    pub clientProcessId: Option<u32>,
    pub targetHost: String,
    /// 实际建立 TCP/二级代理隧道的地址；透明连接可与逻辑 Host/SNI 不同。
    pub connectHost: String,
    /// 指示实际路由已由内核原目标固定；此时 Host/SNI 只恢复应用层域名，不能改变 TCP 目的地址。
    pub routePinned: bool,
    pub targetPort: u16,
    /// 复用当前 SOCKS5 会话的停止信号，接管器必须在取消后结束所有协议处理任务。
    pub cancellation: CancellationToken,
}

/// 描述 TCP 隧道分类后的所有权转移结果，禁止 HTTP/HTTPS 路径与原始中继同时读取同一套接字。
pub enum TcpTunnelDisposition {
    Raw {
        // 原始隧道包含两个套接字和取消状态；装箱避免 `Handled` 结果为每次分类都预留大对象栈空间。
        tunnel: Box<TcpTunnel>,
        applicationProtocol: SessionApplicationProtocol,
    },
    Handled(SessionApplicationProtocol),
}

/// 为 SOCKS5 CONNECT 提供可选的应用层分类入口。
///
/// 运行上下文：核心库只认识 SOCKS5 与 TCP；宿主可在首段字节确认 HTTP 或 TLS 后接管连接，保持库不依赖特定应用协议实现。
/// 参数：tunnel 是独占套接字所有权；返回 future 在分类完成前保持该所有权。
/// 失败语义：future 返回错误时连接关闭；Raw 必须原样返还两端套接字并声明已识别协议，避免丢失首段字节或展示错误类型。
pub trait TcpTunnelInterceptor: Send + Sync {
    fn intercept(
        &self,
        tunnel: TcpTunnel,
    ) -> Pin<Box<dyn Future<Output = io::Result<TcpTunnelDisposition>> + Send>>;
}
