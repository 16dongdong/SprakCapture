#![allow(non_snake_case)]

use std::{net::SocketAddr, path::PathBuf};

use account_service::{AccountServerConfig, startAccountService};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// 启动配置只从父进程标准输入读取，内部令牌不得出现在命令行、环境变量或日志中。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartupRequest {
    databasePath: PathBuf,
    publicAddress: SocketAddr,
    internalAddress: SocketAddr,
    internalToken: String,
    controlBaseUrl: String,
    webAssetsDirectory: Option<PathBuf>,
}

/// 绑定完成后把实际端点和实例标识回传给监督器，stdout 不承载其它日志。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupResponse {
    publicAddress: SocketAddr,
    internalAddress: SocketAddr,
    serviceInstanceId: String,
}

/// 从匿名管道完成启动握手并保持运行直到父进程关闭管道、发送 shutdown 或系统中断。
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("账号服务运行失败：{error}");
        std::process::exit(1);
    }
}

/// 执行可测试的主流程；配置、绑定或关闭失败均返回错误并让进程以非零状态退出。
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut inputLines = BufReader::new(tokio::io::stdin()).lines();
    let startupLine = inputLines
        .next_line()
        .await?
        .ok_or("父进程未提供账号服务启动配置")?;
    let request: StartupRequest = serde_json::from_str(&startupLine)?;
    let running = startAccountService(AccountServerConfig {
        databasePath: request.databasePath,
        publicAddress: request.publicAddress,
        internalAddress: request.internalAddress,
        internalToken: request.internalToken,
        controlBaseUrl: request.controlBaseUrl,
        webAssetsDirectory: request.webAssetsDirectory,
    })
    .await?;
    let response = StartupResponse {
        publicAddress: running.publicAddress,
        internalAddress: running.internalAddress,
        serviceInstanceId: running.serviceInstanceId.clone(),
    };
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
        .await?;
    stdout.flush().await?;
    let mut shutdownReceiver = running.subscribeShutdown();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = shutdownReceiver.changed() => {}
        line = inputLines.next_line() => {
            match line? {
                Some(command) if command.trim() == "shutdown" => {}
                Some(_) => return Err("账号服务收到未知父进程命令".into()),
                None => {}
            }
        }
    }
    running.stop().await?;
    Ok(())
}
