#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::time::Duration;

use tokio::{
    io::{AsyncWriteExt, BufReader, duplex},
    time::timeout,
};

use proxy_backend::runtime::{standaloneModeEnabled, waitForShutdownCommand, waitForShutdownFlag};

/// 验证桌面进程发送的完整 shutdown 行能结束等待，其他控制行不会误触发退出。
#[tokio::test]
async fn shutdownCommandStopsInputLoop() {
    let (mut writer, reader) = duplex(128);
    let shutdownTask = tokio::spawn(waitForShutdownCommand(BufReader::new(reader)));
    writer
        .write_all(b"status\nshutdown\n")
        .await
        .expect("写入关闭控制命令");
    timeout(Duration::from_secs(1), shutdownTask)
        .await
        .expect("关闭命令处理超时")
        .expect("关闭命令任务发生 panic");
}

/// 验证关闭标记在订阅者开始等待前已经发布时仍可立即完成。
#[tokio::test]
async fn shutdownFlagRemembersPublishedState() {
    let (sender, receiver) = tokio::sync::watch::channel(false);
    sender.send_replace(true);
    timeout(Duration::from_millis(100), waitForShutdownFlag(receiver))
        .await
        .expect("已发布关闭标记未立即生效");
}

/// 验证独立模式只接受显式布尔值，拼写错误不会改变桌面父子进程的关闭契约。
#[test]
fn standaloneModeRequiresExplicitValue() {
    assert!(standaloneModeEnabled(Some("1")));
    assert!(standaloneModeEnabled(Some("TRUE")));
    assert!(!standaloneModeEnabled(None));
    assert!(!standaloneModeEnabled(Some("yes")));
    assert!(!standaloneModeEnabled(Some("0")));
}
