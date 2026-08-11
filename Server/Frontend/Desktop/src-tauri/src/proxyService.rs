use std::{
    ffi::OsString,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const proxyServiceFileName: &str = "proxyService.exe";
const proxyServicePathVariable: &str = "PROXY_SERVICE_PATH";
const proxyServiceShutdownCommand: &[u8] = b"shutdown\n";
const windowsCreateNoWindow: u32 = 0x0800_0000;
const defaultHealthInterval: Duration = Duration::from_millis(500);
const defaultRestartDelay: Duration = Duration::from_secs(1);
// 后端 SOCKS5 有序关闭上限为三十秒，HTTP 控制面另需一秒排空；外层三十五秒为进程退出保留明确余量。
const defaultShutdownTimeout: Duration = Duration::from_secs(35);
const shutdownPollInterval: Duration = Duration::from_millis(50);

/// 描述代理服务进程的启动与守护策略；路径由开发环境变量或安装包资源目录明确给出。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyServiceConfig {
    executablePath: PathBuf,
    arguments: Vec<OsString>,
    healthInterval: Duration,
    restartDelay: Duration,
    shutdownTimeout: Duration,
}

impl ProxyServiceConfig {
    /// 从运行环境构造代理服务配置；开发态优先读取 `PROXY_SERVICE_PATH`，安装态使用资源目录内的固定产物名。
    pub fn fromRuntime(resourceDirectory: &Path) -> Self {
        let executablePath = std::env::var_os(proxyServicePathVariable).map_or_else(
            || resourceDirectory.join(proxyServiceFileName),
            PathBuf::from,
        );
        Self::new(executablePath)
    }

    /// 使用明确的可执行文件路径构造守护配置；供安装器、嵌入式宿主和独立桌面启动流程复用。
    #[must_use]
    pub const fn new(executablePath: PathBuf) -> Self {
        Self {
            executablePath,
            arguments: Vec::new(),
            healthInterval: defaultHealthInterval,
            restartDelay: defaultRestartDelay,
            shutdownTimeout: defaultShutdownTimeout,
        }
    }

    /// 设置传递给代理服务进程的启动参数；参数在启动前固定，避免运行中修改守护进程的进程模型。
    #[must_use]
    pub fn withArguments(mut self, arguments: Vec<OsString>) -> Self {
        self.arguments = arguments;
        self
    }

    /// 返回当前配置的代理服务可执行文件路径，供宿主诊断与安装完整性检查使用。
    #[must_use]
    pub fn executablePath(&self) -> &Path {
        &self.executablePath
    }
}

/// 表示代理子进程启动、通信或回收失败；错误信息保留具体操作与系统原因。
#[derive(Debug)]
pub struct ProxyServiceError {
    operation: &'static str,
    source: io::Error,
}

impl ProxyServiceError {
    /// 为底层 I/O 错误补充生命周期操作名称，避免日志只出现无上下文的系统错误。
    const fn new(operation: &'static str, source: io::Error) -> Self {
        Self { operation, source }
    }
}

impl fmt::Display for ProxyServiceError {
    /// 输出可直接定位生命周期阶段的中文错误描述。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}：{}", self.operation, self.source)
    }
}

impl std::error::Error for ProxyServiceError {
    /// 返回原始系统错误，供上层记录错误链与系统错误码。
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

enum WorkerCommand {
    Stop,
}

struct WorkerHandle {
    commandSender: Sender<WorkerCommand>,
    workerThread: JoinHandle<Result<(), ProxyServiceError>>,
}

/// 独占代理服务子进程及其守护线程；重复停止保持幂等，防止窗口与运行循环同时回收进程。
pub struct ProxyServiceSupervisor {
    workerHandle: Mutex<Option<WorkerHandle>>,
}

impl ProxyServiceSupervisor {
    /// 创建代理服务守护线程；首启失败留在线程内重试，使 Web 能呈现真实的控制连接失败状态。
    ///
    /// # Errors
    ///
    /// 创建守护线程失败时返回包含底层 I/O 原因的错误；子进程首次启动失败由已创建的守护线程按既定间隔重试。
    pub fn start(config: ProxyServiceConfig) -> Result<Self, ProxyServiceError> {
        let (commandSender, commandReceiver) = mpsc::channel();
        let workerThread = thread::Builder::new()
            .name("proxy-service-supervisor".to_owned())
            .spawn(move || {
                let initialChild = match spawnProxyService(&config) {
                    Ok(childProcess) => Some(childProcess),
                    Err(error) => {
                        eprintln!("代理服务首次启动失败，将在守护间隔后重试：{error}");
                        None
                    }
                };
                superviseProxyService(&config, initialChild, &commandReceiver)
            })
            .map_err(|error| ProxyServiceError::new("创建代理服务守护线程失败", error))?;

        Ok(Self {
            workerHandle: Mutex::new(Some(WorkerHandle {
                commandSender,
                workerThread,
            })),
        })
    }

    /// 请求服务有序退出并等待守护线程完成；超时后由工作线程强制回收，避免安装包退出时遗留后台进程。
    ///
    /// # Errors
    ///
    /// 生命周期锁被中毒、守护线程异常或子进程回收失败时返回对应的底层 I/O 错误。
    pub fn stop(&self) -> Result<(), ProxyServiceError> {
        let workerHandle = {
            let mut lockedHandle = self
                .workerHandle
                .lock()
                .map_err(|_| poisonedLockError("锁定代理服务守护状态失败"))?;
            lockedHandle.take()
        };
        let Some(workerHandle) = workerHandle else {
            return Ok(());
        };

        let _ = workerHandle.commandSender.send(WorkerCommand::Stop);
        workerHandle
            .workerThread
            .join()
            .map_err(|_| threadJoinError())?
    }
}

impl Drop for ProxyServiceSupervisor {
    /// 在 Tauri 状态容器异常释放时执行最后一道回收，保证子进程生命周期不超过桌面外壳。
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("代理服务退出失败：{error}");
        }
    }
}

/// 验证配置指向普通文件；缺少构建产物时立即报错，不搜索或猜测其他旧目录。
fn validateExecutable(executablePath: &Path) -> Result<(), ProxyServiceError> {
    let metadata = fs::metadata(executablePath)
        .map_err(|error| ProxyServiceError::new("读取代理服务构建产物失败", error))?;
    if metadata.is_file() {
        return Ok(());
    }

    Err(ProxyServiceError::new(
        "代理服务构建产物不是普通文件",
        io::Error::new(
            io::ErrorKind::InvalidInput,
            executablePath.display().to_string(),
        ),
    ))
}

/// 创建不附加控制台窗口的后台子进程，并保留标准输入作为有序退出控制通道。
fn spawnProxyService(config: &ProxyServiceConfig) -> Result<Child, ProxyServiceError> {
    validateExecutable(&config.executablePath)?;
    let mut command = Command::new(&config.executablePath);
    command
        .args(&config.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    command.creation_flags(windowsCreateNoWindow);

    command
        .spawn()
        .map_err(|error| ProxyServiceError::new("启动代理服务失败", error))
}

/// 轮询服务存活状态并在非预期退出后重启；停止命令优先于健康检查与重启等待。
fn superviseProxyService(
    config: &ProxyServiceConfig,
    initialChild: Option<Child>,
    commandReceiver: &Receiver<WorkerCommand>,
) -> Result<(), ProxyServiceError> {
    let mut childProcess = initialChild;
    loop {
        let waitDuration = if childProcess.is_some() {
            config.healthInterval
        } else {
            config.restartDelay
        };

        match commandReceiver.recv_timeout(waitDuration) {
            Ok(WorkerCommand::Stop) | Err(RecvTimeoutError::Disconnected) => {
                return childProcess
                    .as_mut()
                    .map_or(Ok(()), |child| stopChild(child, config.shutdownTimeout));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if let Some(child) = childProcess.as_mut() {
            let exitStatus = child
                .try_wait()
                .map_err(|error| ProxyServiceError::new("检查代理服务状态失败", error))?;
            if let Some(exitStatus) = exitStatus {
                eprintln!("代理服务意外退出（{exitStatus}），将在守护间隔后重新启动");
                childProcess = None;
            }
            continue;
        }

        match spawnProxyService(config) {
            Ok(child) => childProcess = Some(child),
            Err(error) => eprintln!("代理服务重启失败：{error}"),
        }
    }
}

/// 先通过标准输入发送关闭命令并等待退出；仅在超时后终止进程，确保回收具有确定上界。
fn stopChild(childProcess: &mut Child, shutdownTimeout: Duration) -> Result<(), ProxyServiceError> {
    if childProcess
        .try_wait()
        .map_err(|error| ProxyServiceError::new("检查代理服务退出状态失败", error))?
        .is_some()
    {
        return Ok(());
    }

    if let Some(mut standardInput) = childProcess.stdin.take() {
        if let Err(error) = standardInput
            .write_all(proxyServiceShutdownCommand)
            .and_then(|()| standardInput.flush())
        {
            eprintln!("发送代理服务关闭命令失败，将继续执行有界回收：{error}");
        }
    }

    let deadline = Instant::now() + shutdownTimeout;
    while Instant::now() < deadline {
        if childProcess
            .try_wait()
            .map_err(|error| ProxyServiceError::new("等待代理服务退出失败", error))?
            .is_some()
        {
            return Ok(());
        }
        thread::sleep(shutdownPollInterval);
    }

    childProcess
        .kill()
        .map_err(|error| ProxyServiceError::new("终止超时的代理服务失败", error))?;
    childProcess
        .wait()
        .map_err(|error| ProxyServiceError::new("回收代理服务进程失败", error))?;
    Ok(())
}

/// 构造互斥锁中毒错误；锁中毒表明生命周期状态已经失去一致性，必须显式失败。
fn poisonedLockError(operation: &'static str) -> ProxyServiceError {
    ProxyServiceError::new(operation, io::Error::other("代理服务生命周期互斥锁已中毒"))
}

/// 构造守护线程崩溃错误；线程异常结束时禁止将进程回收误报为成功。
fn threadJoinError() -> ProxyServiceError {
    ProxyServiceError::new(
        "等待代理服务守护线程失败",
        io::Error::other("代理服务守护线程发生 panic"),
    )
}
