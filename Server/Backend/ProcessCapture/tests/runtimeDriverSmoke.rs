#![allow(non_snake_case, non_upper_case_globals)]

use std::os::windows::fs::OpenOptionsExt;
use std::{
    collections::{BTreeSet, VecDeque},
    fs::OpenOptions,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use process_capture_core::{
    ProcessCapture, ProcessCaptureConfiguration, UdpDatagramDecision, UdpDatagramDirection,
    UdpDatagramEvent, UdpDatagramProcessor, UdpDatagramSink,
};

const helperEnvironment: &str = "SPRAK_PROCESS_CAPTURE_DRIVER_HELPER";
const helperTargetEnvironment: &str = "SPRAK_PROCESS_CAPTURE_DRIVER_TARGET";
const udpHelperEnvironment: &str = "SPRAK_PROCESS_CAPTURE_UDP_HELPER";
const udpPassiveHelperEnvironment: &str = "SPRAK_PROCESS_CAPTURE_UDP_PASSIVE_HELPER";
const udpBurstCountEnvironment: &str = "SPRAK_PROCESS_CAPTURE_UDP_BURST_COUNT";
const udpBurstDatagrams: u32 = 1_024;
const unselectedBurstDatagrams: u32 = 1_000;
const driverServiceName: &str = "WinDivert";
const driverStopTimeout: Duration = Duration::from_secs(5);
const driverStopPollInterval: Duration = Duration::from_millis(20);

/// 真实驱动测试使用的顺序内存落点；测试会立即消费，生产路径始终使用有界磁盘 spool。
#[derive(Default)]
struct CollectingUdpSink {
    events: Mutex<VecDeque<UdpDatagramEvent>>,
}

impl UdpDatagramSink for CollectingUdpSink {
    /// 按驱动调用顺序保存事件；锁中毒作为显式测试失败返回。
    fn append(&self, event: UdpDatagramEvent) -> Result<(), String> {
        self.events
            .lock()
            .map_err(|_| "测试 UDP 事件锁中毒".to_owned())?
            .push_back(event);
        Ok(())
    }

    /// 测试落点只使用固定规模流量，不产生异步故障。
    fn fault(&self) -> Option<String> {
        None
    }
}

impl CollectingUdpSink {
    /// 弹出最早事件，供轮询断言捕获顺序和数量。
    fn pop(&self) -> Option<UdpDatagramEvent> {
        self.events.lock().expect("测试 UDP 事件锁中毒").pop_front()
    }
}

/// 在真实驱动测试中把上行正文等长替换；下行保持不变，便于同时验证服务器收到的线上字节和客户端响应。
struct RewritingUdpProcessor;

impl UdpDatagramProcessor for RewritingUdpProcessor {
    /// 只修改目标测试报文；其他系统 UDP 必须透明转发，避免测试处理器扩大到未选流量。
    fn process(&self, event: &UdpDatagramEvent) -> Result<UdpDatagramDecision, String> {
        let payload = if event.direction == UdpDatagramDirection::Up
            && event.payload == b"windivert-udp-up"
        {
            b"windivert-wpe-up".to_vec()
        } else {
            event.payload.clone()
        };
        Ok(UdpDatagramDecision::Forward {
            payload,
            modifications: Vec::new(),
        })
    }
}

/// 只在父验证进程显式设置环境变量时发起真实连接；独立执行 ignored 测试时立即返回。
#[test]
#[ignore = "仅由管理员真实驱动父测试启动"]
fn driverConnectionHelper() {
    if std::env::var_os(helperEnvironment).is_none() {
        return;
    }
    let target = std::env::var(helperTargetEnvironment)
        .expect("父测试未传入本机直连目标")
        .parse::<SocketAddr>()
        .expect("本机直连目标格式错误");
    let mut trigger = [0_u8; 1];
    io::stdin()
        .read_exact(&mut trigger)
        .expect("父测试未触发辅助连接");
    let _stream =
        TcpStream::connect_timeout(&target, Duration::from_secs(10)).expect("辅助进程连接目标失败");
    thread::sleep(Duration::from_secs(15));
}

/// 由父验证进程触发一轮真实 UDP 请求/响应；正文不经过标准输出，避免测试通道改变数据报边界。
#[test]
#[ignore = "仅由管理员真实驱动父测试启动"]
fn driverUdpHelper() {
    if std::env::var_os(udpHelperEnvironment).is_none() {
        return;
    }
    let target = std::env::var(helperTargetEnvironment)
        .expect("父测试未传入 UDP 目标")
        .parse::<SocketAddr>()
        .expect("UDP 目标格式错误");
    let socket = UdpSocket::bind("0.0.0.0:0").expect("UDP 辅助进程绑定失败");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("设置 UDP 辅助进程读取超时失败");
    io::stdout()
        .write_all(b"R")
        .expect("通知父测试 UDP socket 已完成 BIND");
    if std::env::var_os(udpPassiveHelperEnvironment).is_some() {
        io::stdout()
            .write_all(
                &socket
                    .local_addr()
                    .expect("读取 UDP 监听端口失败")
                    .port()
                    .to_be_bytes(),
            )
            .expect("通知父测试 UDP socket 监听端口");
    }
    io::stdout().flush().expect("刷新 UDP BIND ready 信号");
    let mut trigger = [0_u8; 1];
    io::stdin()
        .read_exact(&mut trigger)
        .expect("父测试未触发 UDP 辅助进程");
    if std::env::var_os(udpPassiveHelperEnvironment).is_some() {
        let mut request = [0_u8; 64];
        let (byteCount, source) = socket
            .recv_from(&mut request)
            .expect("UDP 被动首包接收超时");
        assert_eq!(source, target);
        assert_eq!(&request[..byteCount], b"windivert-udp-first-down");
        // resolver 关联使用系统 owner 表；保持 socket 存活到父测试完成断言，避免退出先于副本解析。
        thread::sleep(Duration::from_secs(2));
        return;
    }
    if let Some(burstCount) = std::env::var_os(udpBurstCountEnvironment) {
        let burstCount = burstCount
            .to_string_lossy()
            .parse::<u32>()
            .expect("UDP 高负载数量格式错误");
        // 先连续发送整窗请求，再集中接收响应；这条路径禁止逐包 ACK 把真实突发退化为串行 ping-pong。
        for sequence in 0..burstCount {
            socket
                .send_to(&sequence.to_be_bytes(), target)
                .expect("UDP 高负载发送失败");
            if sequence % 64 == 63 {
                thread::sleep(Duration::from_millis(1));
            }
        }
        for expectedSequence in 0..burstCount {
            let mut response = [0_u8; 4];
            let (byteCount, source) = socket.recv_from(&mut response).expect("UDP 高负载响应超时");
            assert_eq!(source, target);
            assert_eq!(byteCount, response.len());
            assert_eq!(u32::from_be_bytes(response), expectedSequence);
        }
        return;
    }
    socket
        .send_to(b"windivert-udp-up", target)
        .expect("UDP 辅助进程发送失败");
    let mut response = [0_u8; 64];
    let (byteCount, source) = socket
        .recv_from(&mut response)
        .expect("UDP 辅助进程读取响应失败");
    assert_eq!(source, target);
    assert_eq!(&response[..byteCount], b"windivert-udp-down");
}

/// 启动等待 stdin 触发的确定性连接进程；启动失败以错误返回，使父测试仍能执行驱动卸载。
fn spawnConnectionHelper(target: SocketAddr) -> Result<std::process::Child, String> {
    Command::new(std::env::current_exe().expect("读取当前集成测试程序路径"))
        .args([
            "--ignored",
            "--exact",
            "driverConnectionHelper",
            "--nocapture",
        ])
        .env(helperEnvironment, "1")
        .env(helperTargetEnvironment, target.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动目标子进程失败：{error}"))
}

/// 启动等待 stdin 触发的 UDP 子进程；真实 PID 由父测试热加入捕获集合。
fn spawnUdpHelper(target: SocketAddr) -> Result<std::process::Child, String> {
    Command::new(std::env::current_exe().expect("读取当前集成测试程序路径"))
        .args(["--ignored", "--exact", "driverUdpHelper", "--nocapture"])
        .env(udpHelperEnvironment, "1")
        .env(helperTargetEnvironment, target.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动 UDP 目标子进程失败：{error}"))
}

/// 启动仅接收入站首包的 UDP 子进程；父测试可验证热加入前既有 socket 没有出站建表时的反向关联。
fn spawnPassiveUdpHelper(target: SocketAddr) -> Result<std::process::Child, String> {
    Command::new(std::env::current_exe().expect("读取当前集成测试程序路径"))
        .args(["--ignored", "--exact", "driverUdpHelper", "--nocapture"])
        .env(udpHelperEnvironment, "1")
        .env(udpPassiveHelperEnvironment, "1")
        .env(helperTargetEnvironment, target.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动 UDP 被动首包子进程失败：{error}"))
}

/// 启动无逐包确认的 UDP 突发子进程；完整窗口发出后才接收响应，真实覆盖固定队列峰值。
fn spawnUdpBurstHelper(target: SocketAddr) -> Result<std::process::Child, String> {
    Command::new(std::env::current_exe().expect("读取当前集成测试程序路径"))
        .args(["--ignored", "--exact", "driverUdpHelper", "--nocapture"])
        .env(udpHelperEnvironment, "1")
        .env(helperTargetEnvironment, target.to_string())
        .env(udpBurstCountEnvironment, udpBurstDatagrams.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动 UDP 高负载子进程失败：{error}"))
}

/// 在 capture 完成启动后触发 helper，杜绝依赖固定睡眠猜测驱动就绪时刻。
fn triggerConnectionHelper(child: &mut std::process::Child) -> Result<(), String> {
    child
        .stdin
        .take()
        .ok_or_else(|| "辅助进程 stdin 不可用".to_owned())?
        .write_all(&[1])
        .map_err(|error| format!("触发辅助连接失败：{error}"))
}

/// 等待 UDP helper 明确报告 BIND 已完成；父测试随后热加入 PID，真实覆盖“既有 socket 首包”。
fn waitUdpHelperReady(child: &mut std::process::Child) -> Result<(), String> {
    let output = child
        .stdout
        .as_mut()
        .ok_or_else(|| "UDP 辅助进程 stdout 不可用".to_owned())?;
    let mut byte = [0_u8; 1];
    for _ in 0..4_096 {
        output
            .read_exact(&mut byte)
            .map_err(|error| format!("等待 UDP BIND ready 失败：{error}"))?;
        if byte == *b"R" {
            return Ok(());
        }
    }
    Err("UDP helper 在输出上限内未报告 BIND ready".to_owned())
}

/// 等待被动 helper 完成 BIND 并读取端口；地址由父测试使用的同一 LAN 接口补齐。
fn waitPassiveUdpHelperReady(
    child: &mut std::process::Child,
    lanAddress: std::net::IpAddr,
) -> Result<SocketAddr, String> {
    waitUdpHelperReady(child)?;
    let output = child
        .stdout
        .as_mut()
        .ok_or_else(|| "UDP 被动 helper stdout 不可用".to_owned())?;
    let mut portBytes = [0_u8; 2];
    output
        .read_exact(&mut portBytes)
        .map_err(|error| format!("读取 UDP 被动 helper 端口失败：{error}"))?;
    Ok(SocketAddr::new(lanAddress, u16::from_be_bytes(portBytes)))
}

/// 在管理员 E2E 收尾阶段停止测试启动的 WinDivert 服务；轮询服务状态与 SYS 文件共享锁，超时返回完整 sc.exe 诊断。
fn stopDriverServiceForTest() -> Result<String, String> {
    let stopOutput = Command::new("sc.exe")
        .args(["stop", driverServiceName])
        .output()
        .map_err(|error| format!("启动 sc.exe stop 失败：{error}"))?;
    let stopDiagnostic = formatCommandOutput(&stopOutput);

    let driverPath = std::env::current_exe()
        .map_err(|error| format!("读取测试程序路径失败：{error}"))?
        .parent()
        .expect("集成测试程序缺少父目录")
        .join("WinDivert64.sys");
    let deadline = Instant::now() + driverStopTimeout;
    loop {
        let queryOutput = Command::new("sc.exe")
            .args(["query", driverServiceName])
            .output()
            .map_err(|error| format!("启动 sc.exe query 失败：{error}"))?;
        let queryDiagnostic = formatCommandOutput(&queryOutput);
        let queryText = format!(
            "{}\n{}",
            String::from_utf8_lossy(&queryOutput.stdout),
            String::from_utf8_lossy(&queryOutput.stderr)
        );
        let serviceInactive = !queryOutput.status.success()
            || (!queryText.contains("RUNNING") && !queryText.contains("PENDING"));
        // 以零共享模式短暂打开只读句柄，只检测内核是否已释放 SYS，不修改驱动文件内容。
        let driverUnlocked = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&driverPath)
            .is_ok();
        if serviceInactive && driverUnlocked {
            return Ok(format!(
                "stop=[{stopDiagnostic}], query=[{queryDiagnostic}], driver={} 已解锁",
                driverPath.display()
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "WinDivert 服务或驱动文件在 {:?} 内未解锁：stop=[{stopDiagnostic}], query=[{queryDiagnostic}], driver={}, unlocked={driverUnlocked}",
                driverStopTimeout,
                driverPath.display()
            ));
        }
        thread::sleep(driverStopPollInterval);
    }
}

/// 将 sc.exe 的退出状态、标准输出和错误输出统一纳入 E2E 诊断；失败时不丢失 SCM 根因。
fn formatCommandOutput(output: &std::process::Output) -> String {
    format!(
        "status={:?}, stdout={:?}, stderr={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// 在真实 WinDivert 句柄上验证子进程 SYN 被送入内部回环监听，并可恢复原始目标。
#[test]
#[ignore = "需要管理员权限和真实 WinDivert 驱动"]
fn redirectsSelectedChildProcessThroughRealDriver() {
    let routeProbe = UdpSocket::bind("0.0.0.0:0").expect("创建本机地址探针");
    routeProbe
        .connect("1.1.1.1:80")
        .expect("系统缺少可用 IPv4 路由");
    let lanAddress = routeProbe.local_addr().expect("读取本机 LAN 地址").ip();
    assert!(
        !lanAddress.is_loopback() && !lanAddress.is_unspecified(),
        "未解析到非回环本机地址：{lanAddress}"
    );
    let directListener =
        TcpListener::bind(SocketAddr::new(lanAddress, 0)).expect("绑定确定性直连目标监听器");
    directListener
        .set_nonblocking(true)
        .expect("设置直连目标监听器非阻塞模式");
    let directAddress = directListener.local_addr().expect("读取直连目标地址");
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定内部验证监听器");
    listener
        .set_nonblocking(true)
        .expect("设置验证监听器非阻塞模式");
    let proxyAddress = listener.local_addr().expect("读取验证监听地址");
    let capture = ProcessCapture::new();
    capture
        .start(ProcessCaptureConfiguration {
            enabled: true,
            processIds: BTreeSet::new(),
            proxyPort: proxyAddress.port(),
            proxyAddress: proxyAddress.ip(),
        })
        .expect("先于目标进程启动 WinDivert 捕获器");
    let mut child = spawnConnectionHelper(directAddress).expect("捕获器启动后创建目标子进程");
    let verification = (|| -> Result<(), String> {
        capture
            .updateProcessIds(BTreeSet::from([child.id()]))
            .map_err(|error| format!("目标进程 PID 热加入失败：{error}"))?;
        triggerConnectionHelper(&mut child)?;

        let deadline = Instant::now() + Duration::from_secs(10);
        let accepted = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(format!("内部监听未收到重定向连接：{error}")),
            }
            match directListener.accept() {
                Ok((_, peer)) => {
                    return Err(format!("目标连接旁路到直连监听器：peer={peer}"));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(format!("直连监听器检查失败：{error}")),
            }
        };
        let (_, peerAddress) = accepted;
        let original = capture
            .originalTargetForPeer(proxyAddress.ip(), peerAddress)
            .ok_or_else(|| "反射连接缺少原始目标".to_owned())?;
        if original.processId != child.id() || original.address != directAddress {
            return Err(format!("原始目标不匹配：{original:?}"));
        }
        if capture.snapshot().redirectedPackets == 0 {
            return Err("重定向计数仍为零".to_owned());
        }
        Ok(())
    })();
    let diagnosticSnapshot = capture.snapshot();
    eprintln!("停止前 ProcessCapture 快照：{diagnosticSnapshot:?}");
    let stopStartedAt = Instant::now();
    let stopResult = capture.stop();
    let stopElapsed = stopStartedAt.elapsed();
    let _ = child.kill();
    let childOutput = child.wait_with_output().ok();
    let stoppedSnapshot = capture.snapshot();
    let stoppedStateVerification = if stoppedSnapshot.running || stoppedSnapshot.trackedFlows != 0 {
        Err(format!("停止后状态未清空：{stoppedSnapshot:?}"))
    } else {
        Ok(())
    };

    let (recoveryVerification, recoveryOutput) = match spawnConnectionHelper(directAddress) {
        Ok(mut recoveryChild) => {
            let recoveryVerification = (|| -> Result<(), String> {
                triggerConnectionHelper(&mut recoveryChild)?;
                let recoveryDeadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match directListener.accept() {
                        Ok((_, _)) => return Ok(()),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) => {
                            return Err(format!("停止后直连监听器检查失败：{error}"));
                        }
                    }
                    match listener.accept() {
                        Ok((_, peer)) => {
                            return Err(format!("停止后连接仍进入内部捕获监听器：{peer}"));
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(error) => {
                            return Err(format!("停止后内部监听器检查失败：{error}"));
                        }
                    }
                    if Instant::now() >= recoveryDeadline {
                        return Err("停止后直连监听器未收到连接".to_owned());
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            })();
            let _ = recoveryChild.kill();
            let recoveryOutput = recoveryChild.wait_with_output().ok();
            (recoveryVerification, recoveryOutput)
        }
        Err(error) => (Err(error), None),
    };
    // 测试专用卸载必须位于所有 capture/helper 句柄回收之后，生产 stop 不得卸载共享驱动。
    drop(capture);
    let driverCleanupResult = stopDriverServiceForTest();
    assert!(
        verification.is_ok()
            && stopResult.is_ok()
            && stopElapsed < Duration::from_secs(2)
            && stoppedStateVerification.is_ok()
            && recoveryVerification.is_ok()
            && driverCleanupResult.is_ok(),
        "真实驱动验证失败：verification={verification:?}, stop={stopResult:?}, stopElapsed={stopElapsed:?}, stopped={stoppedStateVerification:?}, recovery={recoveryVerification:?}, driverCleanup={driverCleanupResult:?}, snapshot={diagnosticSnapshot:?}, child={childOutput:?}, recoveryChild={recoveryOutput:?}"
    );
}

/// 在真实驱动上验证热加入进程的 IPv4 UDP 会先经过统一封包处理器，再以有效校验和写线并产生双向录制事件。
#[test]
#[ignore = "需要管理员权限和真实 WinDivert 驱动"]
fn observesSelectedChildUdpThroughRealDriver() {
    let routeProbe = UdpSocket::bind("0.0.0.0:0").expect("创建 UDP 本机地址探针");
    routeProbe
        .connect("1.1.1.1:53")
        .expect("系统缺少可用 IPv4 UDP 路由");
    let lanAddress = routeProbe.local_addr().expect("读取本机 LAN 地址").ip();
    let directSocket = UdpSocket::bind(SocketAddr::new(lanAddress, 0)).expect("绑定真实 UDP 目标");
    directSocket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("设置真实 UDP 目标超时");
    let directAddress = directSocket.local_addr().expect("读取真实 UDP 目标地址");
    let internalPort = TcpListener::bind("127.0.0.1:0")
        .expect("保留捕获配置端口")
        .local_addr()
        .expect("读取捕获配置端口")
        .port();
    let capture = ProcessCapture::new();
    let eventSink = Arc::new(CollectingUdpSink::default());
    capture.setUdpDatagramSink(Some(eventSink.clone()));
    capture.setUdpDatagramProcessor(Some(Arc::new(RewritingUdpProcessor)));
    capture
        .start(ProcessCaptureConfiguration {
            enabled: true,
            processIds: BTreeSet::new(),
            proxyPort: internalPort,
            proxyAddress: "0.0.0.0".parse().unwrap(),
        })
        .expect("启动 UDP WinDivert 捕获器");
    let mut child = spawnUdpHelper(directAddress).expect("启动 UDP 目标子进程");
    waitUdpHelperReady(&mut child).expect("等待 UDP 目标子进程完成 BIND");
    let verification = (|| -> Result<Vec<UdpDatagramEvent>, String> {
        capture
            .updateProcessIds(BTreeSet::from([child.id()]))
            .map_err(|error| format!("UDP 目标进程热加入失败：{error}"))?;
        triggerConnectionHelper(&mut child)?;
        let mut request = [0_u8; 64];
        let (byteCount, clientAddress) = directSocket
            .recv_from(&mut request)
            .map_err(|error| format!("真实 UDP 目标未收到请求：{error}"))?;
        if &request[..byteCount] != b"windivert-wpe-up" {
            return Err(format!("UDP 上行正文损坏：{:?}", &request[..byteCount]));
        }
        directSocket
            .send_to(b"windivert-udp-down", clientAddress)
            .map_err(|error| format!("真实 UDP 目标回复失败：{error}"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut events = Vec::new();
        while events.len() < 2 && Instant::now() < deadline {
            match eventSink.pop() {
                Some(event) => events.push(event),
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
        if !events.iter().any(|event| {
            event.processId == child.id()
                && event.targetAddress == directAddress
                && event.direction == UdpDatagramDirection::Up
                && event.payload == b"windivert-wpe-up"
        }) {
            return Err(format!("缺少完整 UDP 上行事件：{events:?}"));
        }
        if !events.iter().any(|event| {
            event.processId == child.id()
                && event.targetAddress == directAddress
                && event.direction == UdpDatagramDirection::Down
                && event.payload == b"windivert-udp-down"
        }) {
            return Err(format!("缺少完整 UDP 下行事件：{events:?}"));
        }
        Ok(events)
    })();
    let snapshot = capture.snapshot();
    let stopResult = capture.stop();
    let childOutput = child.wait_with_output().ok();
    drop(capture);
    let driverCleanupResult = stopDriverServiceForTest();
    assert!(
        verification.is_ok()
            && stopResult.is_ok()
            && snapshot.bytesUp > 0
            && snapshot.bytesDown > 0
            && driverCleanupResult.is_ok(),
        "真实 UDP 驱动验证失败：verification={verification:?}, stop={stopResult:?}, cleanup={driverCleanupResult:?}, snapshot={snapshot:?}, child={childOutput:?}"
    );
}

/// 验证热加入既有 UDP socket 后，即使首包是同机服务端发出的 outbound 回复，也能反向归属为 Down。
#[test]
#[ignore = "需要管理员权限和真实 WinDivert 驱动"]
fn observesPassiveSelectedChildFirstInboundDatagram() {
    let routeProbe = UdpSocket::bind("0.0.0.0:0").expect("创建 UDP 被动首包路由探针");
    routeProbe
        .connect("1.1.1.1:53")
        .expect("系统缺少可用 IPv4 UDP 路由");
    let lanAddress = routeProbe.local_addr().expect("读取本机 LAN 地址").ip();
    let serverSocket =
        UdpSocket::bind(SocketAddr::new(lanAddress, 0)).expect("绑定 UDP 被动首包服务端");
    let serverAddress = serverSocket.local_addr().expect("读取 UDP 被动服务端地址");
    let internalPort = TcpListener::bind("127.0.0.1:0")
        .expect("保留捕获配置端口")
        .local_addr()
        .expect("读取捕获配置端口")
        .port();
    let capture = ProcessCapture::new();
    let eventSink = Arc::new(CollectingUdpSink::default());
    capture.setUdpDatagramSink(Some(eventSink.clone()));
    capture
        .start(ProcessCaptureConfiguration {
            enabled: true,
            processIds: BTreeSet::new(),
            proxyPort: internalPort,
            proxyAddress: "0.0.0.0".parse().unwrap(),
        })
        .expect("启动 UDP 被动首包捕获器");
    let mut child = spawnPassiveUdpHelper(serverAddress).expect("启动 UDP 被动首包子进程");
    let childAddress =
        waitPassiveUdpHelperReady(&mut child, lanAddress).expect("等待 UDP 被动 helper 完成 BIND");
    let verification = (|| -> Result<UdpDatagramEvent, String> {
        capture
            .updateProcessIds(BTreeSet::from([child.id()]))
            .map_err(|error| format!("热加入 UDP 被动进程失败：{error}"))?;
        // helper 在此之前没有发送过数据；这份同机 LAN 回复是选中 socket 的第一个网络包。
        serverSocket
            .send_to(b"windivert-udp-first-down", childAddress)
            .map_err(|error| format!("发送 UDP 被动首包失败：{error}"))?;
        triggerConnectionHelper(&mut child)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(event) = eventSink.pop()
                && event.processId == child.id()
                && event.targetAddress == serverAddress
                && event.direction == UdpDatagramDirection::Down
                && event.payload == b"windivert-udp-first-down"
            {
                return Ok(event);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(format!(
            "未捕获 UDP 被动首个下行包：snapshot={:?}",
            capture.snapshot()
        ))
    })();
    let childOutput = child.wait_with_output().ok();
    let snapshot = capture.snapshot();
    let stopResult = capture.stop();
    drop(capture);
    let cleanupResult = stopDriverServiceForTest();
    assert!(
        verification.is_ok()
            && snapshot.restoredPackets == 1
            && snapshot.redirectedPackets == 0
            && stopResult.is_ok()
            && cleanupResult.is_ok(),
        "UDP 被动首包真实验证失败：verification={verification:?}, snapshot={snapshot:?}, stop={stopResult:?}, cleanup={cleanupResult:?}, child={childOutput:?}"
    );
}

/// 在真实驱动上验证持续 UDP 往返期间系统 DNS 保持可用，且每份选中进程数据报都产生录制事件。
#[test]
#[ignore = "需要管理员权限、真实 WinDivert 驱动和可用 DNS"]
fn preservesDnsAndEverySelectedDatagramDuringUdpBurst() {
    let routeProbe = UdpSocket::bind("0.0.0.0:0").expect("创建 UDP 本机地址探针");
    routeProbe
        .connect("1.1.1.1:53")
        .expect("系统缺少可用 IPv4 UDP 路由");
    let lanAddress = routeProbe.local_addr().expect("读取本机 LAN 地址").ip();
    let directSocket =
        UdpSocket::bind(SocketAddr::new(lanAddress, 0)).expect("绑定 UDP 高负载目标");
    directSocket
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("设置 UDP 高负载目标超时");
    let directAddress = directSocket.local_addr().expect("读取 UDP 高负载目标地址");
    let internalPort = TcpListener::bind("127.0.0.1:0")
        .expect("保留捕获配置端口")
        .local_addr()
        .expect("读取捕获配置端口")
        .port();
    let capture = ProcessCapture::new();
    let eventSink = Arc::new(CollectingUdpSink::default());
    capture.setUdpDatagramSink(Some(eventSink.clone()));
    capture
        .start(ProcessCaptureConfiguration {
            enabled: true,
            processIds: BTreeSet::new(),
            proxyPort: internalPort,
            proxyAddress: "0.0.0.0".parse().unwrap(),
        })
        .expect("启动 UDP 高负载 WinDivert 捕获器");
    let mut child = spawnUdpBurstHelper(directAddress).expect("启动 UDP 高负载目标子进程");
    waitUdpHelperReady(&mut child).expect("等待 UDP 高负载子进程完成 BIND");
    // 未选父进程同时制造高 pps 数据报，验证 owner 端点索引不会让无关流量拖慢选中突发。
    let noiseReceiver =
        UdpSocket::bind(SocketAddr::new(lanAddress, 0)).expect("绑定未选 UDP 高负载接收端");
    noiseReceiver
        .set_nonblocking(true)
        .expect("设置未选 UDP 高负载接收端非阻塞");
    let noiseTarget = noiseReceiver.local_addr().expect("读取未选 UDP 高负载目标");
    let noiseFinished = Arc::new(AtomicBool::new(false));
    let noiseSenderFinished = Arc::clone(&noiseFinished);
    let noiseSender = thread::spawn(move || -> Result<u32, String> {
        let result = (|| {
            let socket = UdpSocket::bind(SocketAddr::new(lanAddress, 0))
                .map_err(|error| format!("绑定未选 UDP 高负载发送端失败：{error}"))?;
            for sequence in 0..unselectedBurstDatagrams {
                socket
                    .send_to(&sequence.to_be_bytes(), noiseTarget)
                    .map_err(|error| format!("发送未选 UDP 高负载第 {sequence} 包失败：{error}"))?;
                if sequence % 64 == 63 {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Ok(unselectedBurstDatagrams)
        })();
        noiseSenderFinished.store(true, Ordering::Release);
        result
    });
    let noiseReceiverFinished = Arc::clone(&noiseFinished);
    let noiseDrainer = thread::spawn(move || {
        let mut buffer = [0_u8; 4];
        let mut received = 0_u32;
        let mut idleSince = Instant::now();
        loop {
            match noiseReceiver.recv_from(&mut buffer) {
                Ok(_) => {
                    received += 1;
                    idleSince = Instant::now();
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if noiseReceiverFinished.load(Ordering::Acquire)
                        && idleSince.elapsed() >= Duration::from_millis(100)
                    {
                        return received;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("接收未选 UDP 高负载失败：{error}"),
            }
        }
    });
    let verification = (|| -> Result<(), String> {
        capture
            .updateProcessIds(BTreeSet::from([child.id()]))
            .map_err(|error| format!("UDP 高负载进程热加入失败：{error}"))?;
        triggerConnectionHelper(&mut child)?;
        for expectedSequence in 0..udpBurstDatagrams {
            let mut request = [0_u8; 4];
            let (byteCount, clientAddress) = directSocket
                .recv_from(&mut request)
                .map_err(|error| format!("UDP 高负载第 {expectedSequence} 包未送达：{error}"))?;
            if byteCount != request.len() || u32::from_be_bytes(request) != expectedSequence {
                return Err(format!(
                    "UDP 高负载包损坏：expected={expectedSequence}, bytes={byteCount}, payload={request:?}"
                ));
            }
            directSocket
                .send_to(&request, clientAddress)
                .map_err(|error| format!("UDP 高负载第 {expectedSequence} 包回复失败：{error}"))?;
            if expectedSequence == udpBurstDatagrams / 2 {
                let resolved = ("music.163.com", 443)
                    .to_socket_addrs()
                    .map_err(|error| format!("UDP 高负载期间系统 DNS 解析失败：{error}"))?
                    .next();
                if resolved.is_none() {
                    return Err("UDP 高负载期间系统 DNS 未返回地址".to_owned());
                }
            }
        }
        let childStatus = child
            .wait()
            .map_err(|error| format!("等待 UDP 高负载子进程失败：{error}"))?;
        if !childStatus.success() {
            return Err(format!("UDP 高负载子进程失败：status={childStatus}"));
        }
        let expectedEvents = usize::try_from(udpBurstDatagrams).unwrap() * 2;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut observedEvents = 0_usize;
        let mut observedUp = 0_u64;
        let mut observedDown = 0_u64;
        while observedEvents < expectedEvents && Instant::now() < deadline {
            match eventSink.pop() {
                Some(event) => {
                    observedEvents += 1;
                    observedUp += u64::from(event.direction == UdpDatagramDirection::Up);
                    observedDown += u64::from(event.direction == UdpDatagramDirection::Down);
                }
                None => thread::sleep(Duration::from_millis(5)),
            }
        }
        if observedEvents != expectedEvents {
            let snapshot = capture.snapshot();
            return Err(format!(
                "UDP 高负载录制缺包：expected={expectedEvents}, observed={observedEvents}, up={observedUp}, down={observedDown}, snapshot={snapshot:?}"
            ));
        }
        let snapshot = capture.snapshot();
        if snapshot.redirectedPackets != udpBurstDatagrams.into()
            || snapshot.restoredPackets != udpBurstDatagrams.into()
            || snapshot.lastError.is_some()
        {
            return Err(format!("UDP 高负载快照异常：{snapshot:?}"));
        }
        Ok(())
    })();
    let noiseSent = noiseSender.join().expect("未选 UDP 发送线程崩溃");
    let noiseReceived = noiseDrainer.join().expect("未选 UDP 接收线程崩溃");
    let stopResult = capture.stop();
    let _ = child.kill();
    let childOutput = child.wait_with_output().ok();
    drop(capture);
    let driverCleanupResult = stopDriverServiceForTest();
    assert!(
        verification.is_ok()
            && noiseSent == Ok(unselectedBurstDatagrams)
            && noiseReceived > 0
            && stopResult.is_ok()
            && driverCleanupResult.is_ok(),
        "UDP 高负载真实驱动验证失败：verification={verification:?}, noiseSent={noiseSent:?}, noiseReceived={noiseReceived}, stop={stopResult:?}, cleanup={driverCleanupResult:?}, child={childOutput:?}"
    );
}
