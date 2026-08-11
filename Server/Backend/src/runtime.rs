use std::{future::IntoFuture, net::SocketAddr, time::Duration};

use crate::controlApi::{ControlState, createControlRouter};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    net::TcpListener,
    sync::watch,
    time::timeout,
};

const defaultControlPort: u16 = 17_890;
const controlDrainTimeout: Duration = Duration::from_secs(1);
const standaloneModeVariable: &str = "PROXY_STANDALONE";

/// 解析本机控制地址；拒绝非回环地址，防止控制面意外暴露到公网。
fn resolveControlAddress() -> Result<SocketAddr, String> {
    let configuredAddress = std::env::var("PROXY_CONTROL_ADDRESS")
        .unwrap_or_else(|_| format!("127.0.0.1:{defaultControlPort}"));
    let socketAddress = configuredAddress
        .parse::<SocketAddr>()
        .map_err(|error| format!("控制地址无效：{error}"))?;
    if !socketAddress.ip().is_loopback() {
        return Err("控制接口只允许绑定本机回环地址".to_owned());
    }
    Ok(socketAddress)
}

/// 运行控制服务并协调 SOCKS5 生命周期；监听或状态初始化失败时以非零状态结束进程。
pub async fn runControlService() {
    let controlAddress = match resolveControlAddress() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("后端启动失败：{error}");
            std::process::exit(1);
        }
    };
    let listener = match TcpListener::bind(controlAddress).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("控制接口绑定失败：{error}");
            std::process::exit(1);
        }
    };
    let actualAddress = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("读取控制接口地址失败：{error}");
            std::process::exit(1);
        }
    };
    let state = match ControlState::new().await {
        Ok(state) => state,
        Err(error) => {
            eprintln!("初始化录制会话失败：{error}");
            std::process::exit(1);
        }
    };
    println!("Sprak Capture 控制接口已监听：http://{actualAddress}");
    let (shutdownSender, shutdownReceiver) = watch::channel(false);
    let shutdownState = state.clone();
    let shutdownTask = tokio::spawn(async move {
        shutdownSignal().await;
        // 先终止 WebSocket，再通知 Axum 停止接收；慢控制连接最多占用一秒排空窗口。
        shutdownState.beginShutdown();
        shutdownSender.send_replace(true);
    });
    let serveResult = {
        let gracefulReceiver = shutdownReceiver.clone();
        let server = axum::serve(listener, createControlRouter(state.clone()))
            .with_graceful_shutdown(waitForShutdownFlag(gracefulReceiver))
            .into_future();
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => Some(result),
            _ = waitForShutdownFlag(shutdownReceiver) => {
                timeout(controlDrainTimeout, &mut server).await.ok()
            }
        }
    };
    shutdownTask.abort();
    let _ = shutdownTask.await;
    let stopResult = state.stopService().await;
    if serveResult.is_none() {
        eprintln!("控制接口排空超过 1 秒，已关闭残留控制连接");
    }
    if let Some(Err(error)) = serveResult {
        eprintln!("控制接口运行失败：{error}");
        std::process::exit(1);
    }
    if let Err(error) = stopResult {
        eprintln!("SOCKS5 服务关闭失败：{}", error.message());
        std::process::exit(1);
    }
}

/// 等待可记忆的关闭标记；订阅晚于通知时也必须立即完成。
pub async fn waitForShutdownFlag(mut shutdownReceiver: watch::Receiver<bool>) {
    if *shutdownReceiver.borrow() {
        return;
    }
    while shutdownReceiver.changed().await.is_ok() {
        if *shutdownReceiver.borrow() {
            return;
        }
    }
}

/// 判断是否运行独立后台进程；仅显式 1 或 true 启用，桌面托管模式继续依赖标准输入生命周期。
pub fn standaloneModeEnabled(configuredValue: Option<&str>) -> bool {
    configuredValue.is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

/// 独立模式只响应系统终止信号；桌面托管模式继续响应 Ctrl+C、shutdown 行或父进程管道关闭。
async fn shutdownSignal() {
    if standaloneModeEnabled(std::env::var(standaloneModeVariable).ok().as_deref()) {
        let _ = tokio::signal::ctrl_c().await;
        return;
    }
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = waitForShutdownLine() => {}
    }
}

/// 逐行读取桌面父进程命令；收到 shutdown 或标准输入关闭时结束等待。
async fn waitForShutdownLine() {
    waitForShutdownCommand(BufReader::new(tokio::io::stdin())).await;
}

/// 从异步缓冲输入读取关闭命令；未知行被忽略，shutdown、EOF 或读取错误均结束等待。
pub async fn waitForShutdownCommand<R>(reader: R)
where
    R: AsyncBufRead + Unpin,
{
    let mut lines = reader.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if line.trim() == "shutdown" => return,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return,
        }
    }
}
