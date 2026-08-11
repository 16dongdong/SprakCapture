//! 提供可由主程序热启停的 Streamable HTTP MCP 传输。
//!
//! 独立 stdio 入口适合命令行客户端；桌面工具需要持久化开关和明确监听状态，因此本模块拥有
//! 回环监听器、会话管理器和关闭令牌，控制面只管理一个运行实例。

use std::net::SocketAddr;

use axum::Router;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, session::local::LocalSessionManager, tower::StreamableHttpService,
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::server::ControlMcpServer;

/// 保存集成 MCP 服务的真实监听结果与关闭资源；析构会立即终止后台任务，避免端口泄漏。
pub struct HttpMcpRuntime {
    localAddress: SocketAddr,
    cancellation: CancellationToken,
    serverTask: Option<JoinHandle<Result<(), String>>>,
}

impl HttpMcpRuntime {
    /// 在指定回环地址启动 MCP 服务，并把每个会话连接到同一控制 API。
    ///
    /// 参数 `listenAddress` 必须由调用方限制为回环地址；`controlBase` 是本机控制 API 根地址，
    /// `locale` 决定工具说明语言。绑定、服务构造或监听失败均返回中文诊断且不留下后台任务。
    pub async fn start(
        listenAddress: SocketAddr,
        controlBase: String,
        locale: Option<String>,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind(listenAddress)
            .await
            .map_err(|error| format!("MCP 监听地址绑定失败：{error}"))?;
        let localAddress = listener
            .local_addr()
            .map_err(|error| format!("MCP 监听地址读取失败：{error}"))?;
        let cancellation = CancellationToken::new();
        let service: StreamableHttpService<ControlMcpServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || {
                    ControlMcpServer::new(&controlBase, locale.as_deref()).map_err(|error| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                    })
                },
                Default::default(),
                StreamableHttpServerConfig::default()
                    .with_sse_keep_alive(None)
                    .with_cancellation_token(cancellation.child_token()),
            );
        let router = Router::new().nest_service("/mcp", service);
        let serverCancellation = cancellation.clone();
        let serverTask = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(serverCancellation.cancelled_owned())
                .await
                .map_err(|error| format!("MCP HTTP 服务异常停止：{error}"))
        });
        Ok(Self {
            localAddress,
            cancellation,
            serverTask: Some(serverTask),
        })
    }

    /// 返回客户端应连接的完整 Streamable HTTP 地址；结果来自真实绑定端口而非配置推断。
    pub fn endpoint(&self) -> String {
        format!("http://{}/mcp", self.localAddress)
    }

    /// 请求所有 MCP 会话与监听器停止并等待任务退出；任务 panic 或传输失败会作为关闭错误返回。
    pub async fn stop(mut self) -> Result<(), String> {
        self.cancellation.cancel();
        self.serverTask
            .take()
            .expect("MCP 运行实例只能停止一次")
            .await
            .map_err(|error| format!("MCP HTTP 服务任务异常：{error}"))?
    }
}

impl Drop for HttpMcpRuntime {
    /// 在控制状态意外析构时同步取消并中止监听任务，保证安装目录内服务重启不会残留端口占用。
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(serverTask) = self.serverTask.as_ref() {
            serverTask.abort();
        }
    }
}
