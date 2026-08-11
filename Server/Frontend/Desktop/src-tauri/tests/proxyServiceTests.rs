#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    ffi::OsString,
    io::{self, BufRead, BufReader},
    path::PathBuf,
    time::{Duration, Instant},
};

use desktop_shell_lib::{ProxyServiceConfig, ProxyServiceSupervisor};

/// 验证公开配置 API 精确保留安装器或宿主指定的可执行文件路径，不再依赖测试专用内部构造器。
#[test]
fn configuredExecutablePathIsPreserved() {
    let executablePath = PathBuf::from(r"D:\build\proxyService.exe");
    let configuration = ProxyServiceConfig::new(executablePath.clone());

    assert_eq!(configuration.executablePath(), executablePath);
}

/// 作为守护进程的独立测试子进程读取有序关闭命令；仅存在于 tests 目录，不参与桌面业务产物。
#[test]
#[ignore = "仅由守护进程生命周期测试作为子进程启动"]
fn proxyServiceHelper() {
    let mut command = String::new();
    BufReader::new(io::stdin())
        .read_line(&mut command)
        .expect("读取守护进程关闭命令失败");
    assert_eq!(command, "shutdown\n");
}

/// 验证守护进程通过稳定的配置 API 启动子进程，并经标准输入完成有序停止而非等待强制回收。
#[test]
fn supervisorStopsChildGracefully() {
    let executablePath = std::env::current_exe().expect("读取集成测试进程路径失败");
    let configuration = ProxyServiceConfig::new(executablePath).withArguments(vec![
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from("proxyServiceHelper"),
        OsString::from("--nocapture"),
    ]);

    let startedAt = Instant::now();
    let supervisor = ProxyServiceSupervisor::start(configuration).expect("启动守护进程失败");
    supervisor.stop().expect("停止守护进程失败");

    assert!(startedAt.elapsed() < Duration::from_secs(3));
}

/// 验证构建产物暂缺时守护线程仍可被创建和停止，桌面外壳不会因首次启动失败而留下后台线程。
#[test]
fn supervisorStopsAfterInitialExecutableFailure() {
    let executablePath = std::env::current_exe()
        .expect("读取集成测试进程路径失败")
        .with_file_name(format!("missing-proxy-service-{}.exe", std::process::id()));
    assert!(!executablePath.exists());

    let supervisor = ProxyServiceSupervisor::start(ProxyServiceConfig::new(executablePath))
        .expect("创建失败重试守护进程失败");
    supervisor.stop().expect("停止失败重试守护进程失败");
}
