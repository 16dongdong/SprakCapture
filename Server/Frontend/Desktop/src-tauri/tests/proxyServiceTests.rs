#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    ffi::OsString,
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use desktop_shell_lib::{ProxyServiceConfig, ProxyServiceSupervisor};

#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    Foundation::{CloseHandle, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

const childProcessMarkerVariable: &str = "PROXY_SERVICE_TEST_CHILD_MARKER";
const gracefulMarkerVariable: &str = "PROXY_SERVICE_TEST_GRACEFUL_MARKER";

/// 验证公开配置 API 精确保留安装器或宿主指定的可执行文件路径，不再依赖测试专用内部构造器。
#[test]
fn configuredExecutablePathIsPreserved() {
    let executablePath = PathBuf::from(r"D:\build\proxyService.exe");
    let configuration = ProxyServiceConfig::new(executablePath.clone());

    assert_eq!(configuration.executablePath(), executablePath);
}

/// 作为不响应关闭协议的独立测试子进程持续运行；仅存在于 tests 目录，不参与桌面业务产物。
#[test]
#[ignore = "仅由守护进程生命周期测试作为子进程启动"]
fn proxyServiceHelper() {
    if let Some(markerPath) = std::env::var_os(childProcessMarkerVariable) {
        fs::write(markerPath, std::process::id().to_string())
            .expect("写入代理服务测试进程编号失败");
    }
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

/// 模拟支持 proxyService stdin 契约的子进程；只在收到 shutdown 后写入完成标记并正常退出。
#[test]
#[ignore = "仅由守护进程有序关闭测试作为子进程启动"]
fn proxyServiceGracefulHelper() {
    let markerPath =
        PathBuf::from(std::env::var_os(gracefulMarkerVariable).expect("缺少有序关闭完成标记路径"));
    let mut line = String::new();
    BufReader::new(std::io::stdin())
        .read_line(&mut line)
        .expect("读取桌面关闭命令失败");
    if line.trim() == "shutdown" {
        fs::write(markerPath, "closed").expect("写入有序关闭完成标记失败");
    }
}

/// 验证正常桌面退出先走 stdin 关闭协议，而不是直接终止代理及其账号服务子进程。
#[test]
fn supervisorRequestsGracefulShutdownBeforeTermination() {
    let markerPath = std::env::temp_dir().join(format!(
        "proxy-service-graceful-{}-{}.marker",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间早于 Unix 纪元")
            .as_nanos()
    ));
    let executablePath = std::env::current_exe().expect("读取集成测试进程路径失败");
    let configuration = ProxyServiceConfig::new(executablePath).withArguments(vec![
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from("proxyServiceGracefulHelper"),
        OsString::from("--nocapture"),
    ]);
    unsafe {
        std::env::set_var(gracefulMarkerVariable, &markerPath);
    }
    let supervisor = ProxyServiceSupervisor::start(configuration).expect("启动有序关闭夹具失败");
    supervisor.stop().expect("有序停止代理服务失败");
    unsafe {
        std::env::remove_var(gracefulMarkerVariable);
    }
    assert_eq!(
        fs::read_to_string(&markerPath).expect("代理服务未执行有序关闭协议"),
        "closed"
    );
    let _ = fs::remove_file(markerPath);
}

/// 模拟桌面客户端未经析构直接崩溃；进程退出会关闭作业句柄，不能依赖 Rust `Drop` 回收服务。
#[test]
#[ignore = "仅由 Windows 作业对象生命周期测试作为桌面控制进程启动"]
fn desktopCrashController() {
    let executablePath = std::env::current_exe().expect("读取桌面控制测试进程路径失败");
    let configuration = ProxyServiceConfig::new(executablePath).withArguments(vec![
        OsString::from("--ignored"),
        OsString::from("--exact"),
        OsString::from("proxyServiceHelper"),
        OsString::from("--nocapture"),
    ]);
    let _supervisor =
        ProxyServiceSupervisor::start(configuration).expect("启动代理服务测试进程失败");
    let markerPath = PathBuf::from(
        std::env::var_os(childProcessMarkerVariable).expect("缺少代理服务测试进程编号文件"),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !markerPath.is_file() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(markerPath.is_file(), "代理服务测试进程未写入编号");

    // `exit` 故意跳过所有析构函数，用于证明内核作业句柄能覆盖桌面异常终止路径。
    std::process::exit(0);
}

/// 验证不配合关闭协议的子进程在有界等待后仍会被强制回收。
#[test]
fn supervisorForceStopsUnresponsiveChild() {
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

    assert!(startedAt.elapsed() < Duration::from_secs(10));
}

/// 验证桌面客户端崩溃时 Windows 内核会终止代理服务，避免托盘退出后出现孤立监听进程。
#[cfg(target_os = "windows")]
#[test]
fn operatingSystemJobStopsChildAfterDesktopCrash() {
    let markerPath = std::env::temp_dir().join(format!(
        "proxy-service-job-{}-{}.pid",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间早于 Unix 纪元")
            .as_nanos()
    ));
    let executablePath = std::env::current_exe().expect("读取桌面崩溃控制进程路径失败");
    let controllerStatus = Command::new(executablePath)
        .args([
            "--ignored",
            "--exact",
            "desktopCrashController",
            "--nocapture",
        ])
        .env(childProcessMarkerVariable, &markerPath)
        .status()
        .expect("启动桌面崩溃控制进程失败");
    assert!(controllerStatus.success(), "桌面崩溃控制进程执行失败");

    let childProcessId = fs::read_to_string(&markerPath)
        .expect("读取代理服务测试进程编号失败")
        .parse::<u32>()
        .expect("解析代理服务测试进程编号失败");
    let deadline = Instant::now() + Duration::from_secs(5);
    while processIsRunning(childProcessId) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_file(&markerPath);
    assert!(
        !processIsRunning(childProcessId),
        "桌面控制进程退出后代理服务仍在运行：{childProcessId}"
    );
}

/// 查询指定 Windows 进程是否仍处于运行态；进程已退出或编号不存在时返回 false。
#[cfg(target_os = "windows")]
fn processIsRunning(processId: u32) -> bool {
    let processHandle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, processId) };
    if processHandle.is_null() {
        return false;
    }
    let waitResult = unsafe { WaitForSingleObject(processHandle, 0) };
    unsafe {
        CloseHandle(processHandle);
    }
    waitResult == WAIT_TIMEOUT
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
