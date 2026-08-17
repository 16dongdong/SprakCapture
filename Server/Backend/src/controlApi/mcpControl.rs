//! 管理内置 MCP Streamable HTTP 服务的持久化配置与热启停。
//!
//! MCP 与代理数据面是独立生命周期：切换开关或端口不会重启 SOCKS5、HTTP 或 WinDivert，
//! 也不会中断正在录制的事务。监听固定为回环地址，避免把控制能力意外暴露到局域网。

use std::{net::SocketAddr, sync::Arc};

use axum::{Json, Router, extract::State, routing::put};
use capture_mcp::httpServer::HttpMcpRuntime;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{ApiError, ControlSnapshot, ControlState, ErrorCode};

const defaultMcpPort: u16 = 17_891;
const controlBase: &str = "http://127.0.0.1:17890";

/// 定义可跨重启恢复的 MCP 设置；监听主机固定为回环地址，因此配置文件不接受任意网络地址。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpConfiguration {
    pub enabled: bool,
    pub port: u16,
}

impl Default for McpConfiguration {
    /// 返回首次运行设置；默认关闭，避免用户明确启用前占用额外控制端口。
    fn default() -> Self {
        Self {
            enabled: false,
            port: defaultMcpPort,
        }
    }
}

/// 返回前端所需的 MCP 配置和真实运行状态；endpoint 只在监听成功后出现。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPublicState {
    pub configuration: McpConfiguration,
    pub running: bool,
    pub endpoint: Option<String>,
    pub lastError: Option<String>,
}

/// 保存唯一 MCP 运行实例；互斥锁把配置提交与监听切换串行化，防止并发请求争用端口。
#[derive(Clone)]
pub(super) struct McpManager {
    state: Arc<Mutex<McpManagedState>>,
}

struct McpManagedState {
    configuration: McpConfiguration,
    runtime: Option<HttpMcpRuntime>,
    lastError: Option<String>,
}

impl McpManager {
    /// 从持久化配置恢复 MCP；绑定失败会保留 enabled 与诊断，使主代理仍可启动并允许用户修正端口。
    pub(super) async fn new(configuration: McpConfiguration) -> Self {
        let (runtime, lastError) = if configuration.enabled {
            match startRuntime(configuration.port).await {
                Ok(runtime) => (Some(runtime), None),
                Err(error) => (None, Some(error)),
            }
        } else {
            (None, None)
        };
        Self {
            state: Arc::new(Mutex::new(McpManagedState {
                configuration,
                runtime,
                lastError,
            })),
        }
    }

    /// 复制当前公开状态；读取不会推进全局 revision，也不会触发网络操作。
    pub(super) async fn publicState(&self) -> McpPublicState {
        let state = self.state.lock().await;
        McpPublicState {
            configuration: state.configuration.clone(),
            running: state.runtime.is_some(),
            endpoint: state.runtime.as_ref().map(HttpMcpRuntime::endpoint),
            lastError: state.lastError.clone(),
        }
    }

    /// 原子应用新配置；启用或换端口时先成功绑定新端口，再关闭旧实例，避免配置成功但服务不可用。
    ///
    /// 参数 `configuration` 来自严格 JSON；端口 0 被拒绝。返回旧配置供持久化失败时恢复运行状态。
    pub(super) async fn replace(
        &self,
        configuration: McpConfiguration,
    ) -> Result<McpConfiguration, ApiError> {
        if configuration.port == 0 {
            return Err(ApiError::badRequest(ErrorCode::InvalidConfiguration));
        }
        let mut state = self.state.lock().await;
        let previousConfiguration = state.configuration.clone();
        if configuration == previousConfiguration {
            return Ok(previousConfiguration);
        }
        let nextRuntime = if configuration.enabled {
            Some(startRuntime(configuration.port).await.map_err(|detail| {
                ApiError::internal(ErrorCode::ServiceStartFailed).withParam("detail", detail)
            })?)
        } else {
            None
        };
        if let Some(runtime) = state.runtime.take() {
            runtime.stop().await.map_err(|detail| {
                ApiError::internal(ErrorCode::ServiceStopFailed).withParam("detail", detail)
            })?;
        }
        state.configuration = configuration;
        state.runtime = nextRuntime;
        state.lastError = None;
        Ok(previousConfiguration)
    }
}

/// 启动固定回环监听；控制 API 地址与 MCP 端口分离，避免 MCP 客户端递归进入自身监听器。
async fn startRuntime(port: u16) -> Result<HttpMcpRuntime, String> {
    HttpMcpRuntime::start(
        SocketAddr::from(([127, 0, 0, 1], port)),
        controlBase.to_owned(),
        Some("zh-Hans".to_owned()),
    )
    .await
}

/// 注册 MCP 热更新端点；读取状态随完整控制快照返回，因此不增加重复 GET 路由。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router.route("/api/v1/mcp", put(replaceMcpConfiguration))
}

/// 校验、切换并持久化 MCP 配置；成功响应返回完整权威快照，前端无需轮询补读。
async fn replaceMcpConfiguration(
    State(state): State<ControlState>,
    Json(configuration): Json<McpConfiguration>,
) -> Result<Json<ControlSnapshot>, ApiError> {
    let previousConfiguration = state.mcp.replace(configuration.clone()).await?;
    if let Err(error) = state
        .processSelection
        .replaceMcpConfiguration(configuration)
    {
        let _ = state.mcp.replace(previousConfiguration).await;
        return Err(
            ApiError::internal(ErrorCode::ConfigurationPersistenceFailed)
                .withParam("detail", error.to_string()),
        );
    }
    let mcp = state.mcp.publicState().await;
    state.publishProjectionRevisioned(|serverInstanceId, revision| super::EventMessage::Mcp {
        serverInstanceId,
        revision,
        mcp,
    });
    Ok(Json(state.snapshot().await))
}
