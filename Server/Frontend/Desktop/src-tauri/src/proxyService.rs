use std::{
    ffi::OsString,
    fmt, fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::processJob::ProcessJob;

const proxyServiceFileName: &str = "proxyService.exe";
const proxyServicePathVariable: &str = "PROXY_SERVICE_PATH";
const webAssetsDirectoryVariable: &str = "CAPTURE_WEB_ASSETS_DIR";
const clientPackagerExecutableVariable: &str = "CAPTURE_CLIENT_PACKAGER_EXECUTABLE";
const clientTemplatePathVariable: &str = "CAPTURE_CLIENT_TEMPLATE_PATH";
const clientPackagerFileName: &str = "clientPackager.exe";
const clientTemplateFileName: &str = "clientTemplate.apk";
const windowsCreateNoWindow: u32 = 0x0800_0000;
const defaultHealthInterval: Duration = Duration::from_millis(500);
const defaultRestartDelay: Duration = Duration::from_secs(1);
const gracefulShutdownTimeout: Duration = Duration::from_secs(8);
const shutdownPollInterval: Duration = Duration::from_millis(20);

/// 描述代理服务进程的启动与守护策略；路径由开发环境变量或安装包资源目录明确给出。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyServiceConfig {
    executablePath: PathBuf,
    webAssetsDirectory: PathBuf,
    clientPackagerExecutable: PathBuf,
    clientTemplatePath: PathBuf,
    arguments: Vec<OsString>,
    healthInterval: Duration,
    restartDelay: Duration,
}

impl ProxyServiceConfig {
    /// 从运行环境构造代理服务配置；开发态优先读取 `PROXY_SERVICE_PATH`，安装态使用资源目录内的固定产物名。
    pub fn fromRuntime(resourceDirectory: &Path) -> Self {
        let executablePath = std::env::var_os(proxyServicePathVariable).map_or_else(
            || resourceDirectory.join(proxyServiceFileName),
            PathBuf::from,
        );
        Self::new(executablePath)
            .withWebAssetsDirectory(resourceDirectory.to_path_buf())
            .withClientResources(
                resourceDirectory.join(clientPackagerFileName),
                resourceDirectory.join(clientTemplateFileName),
            )
    }

    /// 使用明确的可执行文件路径构造守护配置。
    ///
    /// 运行上下文：安装器、嵌入式宿主和测试在启动监督器前传入 `executablePath`，其余字段采用
    /// 稳定默认值。该构造过程不执行 I/O、不会失败；保持普通函数可兼容清单声明的 Rust 1.85，
    /// 避免较新编译器把 `PathBuf::new` 的常量化能力误带入最低版本契约。
    #[must_use]
    pub fn new(executablePath: PathBuf) -> Self {
        Self {
            executablePath,
            webAssetsDirectory: PathBuf::new(),
            clientPackagerExecutable: PathBuf::new(),
            clientTemplatePath: PathBuf::new(),
            arguments: Vec::new(),
            healthInterval: defaultHealthInterval,
            restartDelay: defaultRestartDelay,
        }
    }

    /// 设置随桌面安装的 Web 构建目录；该路径只注入子进程环境，不进入命令行或公开配置。
    #[must_use]
    pub fn withWebAssetsDirectory(mut self, webAssetsDirectory: PathBuf) -> Self {
        self.webAssetsDirectory = webAssetsDirectory;
        self
    }

    /// 设置随安装包发布的独立打包器与预编译 APK 模板。
    ///
    /// 运行上下文：桌面资源目录解析完成后调用；两个路径只注入后端子进程环境，不进入控制响应。
    /// 本方法不访问文件系统，资源缺失会由生成任务精确失败，目标机器不需要 Client 源码或编译环境。
    #[must_use]
    pub fn withClientResources(
        mut self,
        clientPackagerExecutable: PathBuf,
        clientTemplatePath: PathBuf,
    ) -> Self {
        self.clientPackagerExecutable = clientPackagerExecutable;
        self.clientTemplatePath = clientTemplatePath;
        self
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
    processJob: Arc<ProcessJob>,
}

impl ProxyServiceSupervisor {
    /// 创建代理服务守护线程；首启失败留在线程内重试，使 Web 能呈现真实的控制连接失败状态。
    ///
    /// # Errors
    ///
    /// 创建守护线程失败时返回包含底层 I/O 原因的错误；子进程首次启动失败由已创建的守护线程按既定间隔重试。
    pub fn start(config: ProxyServiceConfig) -> Result<Self, ProxyServiceError> {
        let processJob = Arc::new(
            ProcessJob::create()
                .map_err(|error| ProxyServiceError::new("创建代理服务进程作业失败", error))?,
        );
        let (commandSender, commandReceiver) = mpsc::channel();
        let workerProcessJob = Arc::clone(&processJob);
        let workerThread = thread::Builder::new()
            .name("proxy-service-supervisor".to_owned())
            .spawn(move || {
                let initialChild = match spawnProxyService(&config, &workerProcessJob) {
                    Ok(childProcess) => Some(childProcess),
                    Err(error) => {
                        eprintln!("代理服务首次启动失败，将在守护间隔后重试：{error}");
                        None
                    }
                };
                superviseProxyService(&config, &workerProcessJob, initialChild, &commandReceiver)
            })
            .map_err(|error| ProxyServiceError::new("创建代理服务守护线程失败", error))?;

        Ok(Self {
            workerHandle: Mutex::new(Some(WorkerHandle {
                commandSender,
                workerThread,
            })),
            processJob,
        })
    }

    /// 请求守护线程立即终止直属代理进程，再终止作业内全部派生进程；重复调用保持幂等。
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
        let workerResult = workerHandle
            .workerThread
            .join()
            .map_err(|_| threadJoinError())
            .and_then(|result| result);
        // 直属进程退出不代表插件 sidecar 等后代已退出；作业级终止是托盘“退出”的最终状态边界。
        let jobResult = self
            .processJob
            .terminate()
            .map_err(|error| ProxyServiceError::new("终止代理服务进程树失败", error));
        workerResult.and(jobResult)
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

/// 创建不附加控制台窗口的后台子进程，并在返回前绑定内核作业；绑定失败会立即回收进程。
fn spawnProxyService(
    config: &ProxyServiceConfig,
    processJob: &ProcessJob,
) -> Result<Child, ProxyServiceError> {
    validateExecutable(&config.executablePath)?;
    let mut command = Command::new(&config.executablePath);
    command
        .args(&config.arguments)
        .env(webAssetsDirectoryVariable, &config.webAssetsDirectory)
        .env(
            clientPackagerExecutableVariable,
            &config.clientPackagerExecutable,
        )
        .env(clientTemplatePathVariable, &config.clientTemplatePath)
        // 后端用标准输入 EOF 判断桌面宿主是否仍存活；保持管道打开，但托盘退出直接终止进程而不等待协议握手。
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    command.creation_flags(windowsCreateNoWindow);

    let mut childProcess = command
        .spawn()
        .map_err(|error| ProxyServiceError::new("启动代理服务失败", error))?;
    if let Err(error) = processJob.assign(&childProcess) {
        // 未进入作业的进程会在桌面崩溃后失去所有权，必须在错误返回前同步回收。
        let _ = childProcess.kill();
        let _ = childProcess.wait();
        return Err(ProxyServiceError::new("绑定代理服务进程作业失败", error));
    }
    Ok(childProcess)
}

/// 轮询服务存活状态并在非预期退出后重启；停止命令优先于健康检查与重启等待。
fn superviseProxyService(
    config: &ProxyServiceConfig,
    processJob: &ProcessJob,
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
                return childProcess.as_mut().map_or(Ok(()), stopChild);
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

        match spawnProxyService(config, processJob) {
            Ok(child) => childProcess = Some(child),
            Err(error) => eprintln!("代理服务重启失败：{error}"),
        }
    }
}

/// 请求代理进程有序关闭并有界等待；只有管道失败或超时才强制终止，确保账号统计和 `SQLite` 已刷新。
fn stopChild(childProcess: &mut Child) -> Result<(), ProxyServiceError> {
    if childProcess
        .try_wait()
        .map_err(|error| ProxyServiceError::new("检查代理服务退出状态失败", error))?
        .is_some()
    {
        return Ok(());
    }

    if let Some(stdin) = childProcess.stdin.as_mut() {
        let _ = stdin.write_all(b"shutdown\n");
        let _ = stdin.flush();
    }
    // 关闭父进程持有的写端，使后端即使未完整读取命令也会从 EOF 进入相同的有序退出路径。
    childProcess.stdin.take();
    let deadline = std::time::Instant::now() + gracefulShutdownTimeout;
    while std::time::Instant::now() < deadline {
        if childProcess
            .try_wait()
            .map_err(|error| ProxyServiceError::new("等待代理服务有序退出失败", error))?
            .is_some()
        {
            return Ok(());
        }
        thread::sleep(shutdownPollInterval);
    }
    childProcess
        .kill()
        .map_err(|error| ProxyServiceError::new("代理服务有序退出超时后强制终止失败", error))?;
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
