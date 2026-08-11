#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

pub mod controlClient;
pub mod localization;
pub mod models;
pub mod server;

use rmcp::{ServiceExt, transport::stdio};
use server::ControlMcpServer;

/// 启动 stdio MCP 服务并等待传输关闭；调用方负责将失败诊断写入其所属的进程日志。
pub async fn runStdioServer() -> Result<(), String> {
    let server = ControlMcpServer::fromEnvironment()?;
    let service = server
        .serve(stdio())
        .await
        .map_err(|error| format!("capture-mcp 传输初始化失败：{error}"))?;
    service
        .waiting()
        .await
        .map(|_| ())
        .map_err(|error| format!("capture-mcp 传输异常停止：{error}"))
}
