#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

/// 启动 MCP 二进制入口；stdout 始终只承载 JSON-RPC，启动与传输诊断固定写入 stderr。
#[tokio::main]
async fn main() {
    if let Err(message) = capture_mcp::runStdioServer().await {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
