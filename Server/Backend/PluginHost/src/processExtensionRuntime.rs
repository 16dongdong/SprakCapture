//! 实现 Python、TypeScript、Go 和独立可执行插件共享的 JSONL 进程运行时。
//!
//! 宿主只负责进程生命周期、并发请求编号和响应归并，不施加调用超时、并发上限或输出配额。
//! 插件进程可以并发返回不同请求；标准输出专用于协议帧，作者日志必须写入标准错误。

use std::{
    collections::HashMap,
    future::Future,
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    pin::Pin,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::oneshot;

use crate::{
    ExtensionAction, ExtensionManifest, ExtensionRuntime, ExtensionRuntimeKind, PluginHostError,
    RuntimeInvocation,
};

const PROCESS_EXTENSION_API_VERSION: u32 = 2;
const PROCESS_JSON_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
const PYTHON_INTERPRETER: &str = "python";
const NODE_INTERPRETER: &str = "node";

type PendingInvocation = oneshot::Sender<Result<ExtensionAction, String>>;

/// 描述宿主写给插件进程的单行消息；serde 标签是跨语言 SDK 的稳定线上字段。
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum HostMessage<'a> {
    Initialize {
        apiVersion: u32,
        manifest: &'a JsonValue,
        configuration: &'a JsonValue,
    },
    Invoke {
        requestId: u64,
        invocation: &'a JsonValue,
    },
    Stop,
}

/// 描述插件进程返回的单行消息；调用错误只影响对应 requestId，不改变其他并发调用。
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WorkerMessage {
    Ready {
        apiVersion: u32,
    },
    Result {
        requestId: u64,
        action: ExtensionAction,
    },
    Error {
        requestId: u64,
        message: String,
    },
}

/// 持有一个开放的进程插件实例；输入写锁只保证 JSONL 帧不交错，不限制插件内部并发。
pub struct ProcessExtensionRuntime {
    input: Mutex<Option<BufWriter<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    readerThread: Mutex<Option<JoinHandle<()>>>,
    pendingInvocations: Arc<Mutex<HashMap<u64, PendingInvocation>>>,
    nextRequestId: AtomicU64,
    protocolFailed: Arc<AtomicBool>,
    stopped: AtomicBool,
}

impl ProcessExtensionRuntime {
    /// 启动 manifest 指定的进程插件并完成初始化握手。
    ///
    /// 运行上下文：插件目录和入口路径已经通过 manifest 校验；`.py` 与 `.js/.mjs/.cjs` sidecar
    /// 分别使用系统 `python` 与 `node`，其他 sidecar 及 `nativeWorker` 直接执行入口文件。
    /// 失败语义：入口缺失、进程创建、首帧读写或 ready 协议不合法时返回精确加载错误，不发布半实例。
    pub fn load(
        manifest: &ExtensionManifest,
        directory: &Path,
        configuration: &JsonValue,
    ) -> Result<Self, PluginHostError> {
        let entryPath = directory.join(&manifest.runtime.entry);
        if !entryPath.is_file() || !entryPath.starts_with(directory) {
            return Err(PluginHostError::MissingEntry);
        }
        let processManifest = serializeProcessInitialization(manifest)?;
        validateProcessJsonNumbers(configuration)?;
        let mut command = processCommand(manifest.runtime.kind, &entryPath);
        command
            .args(&manifest.runtime.arguments)
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().map_err(PluginHostError::Worker)?;
        let (input, output) = match initializeChild(&mut child, &processManifest, configuration) {
            Ok(channels) => channels,
            Err(error) => {
                terminateFailedStartup(&mut child);
                return Err(error);
            }
        };

        let child = Arc::new(Mutex::new(Some(child)));
        let pendingInvocations = Arc::new(Mutex::new(HashMap::new()));
        let protocolFailed = Arc::new(AtomicBool::new(false));
        let readerPending = pendingInvocations.clone();
        let readerFailure = protocolFailed.clone();
        let readerChild = child.clone();
        let readerThread = match thread::Builder::new()
            .name(format!("extension-{}-reader", manifest.id))
            .spawn(move || readWorkerMessages(output, &readerPending, &readerFailure, &readerChild))
        {
            Ok(readerThread) => readerThread,
            Err(error) => {
                if let Some(mut failedChild) = child.lock().take() {
                    terminateFailedStartup(&mut failedChild);
                }
                return Err(PluginHostError::Worker(error));
            }
        };
        Ok(Self {
            input: Mutex::new(Some(input)),
            child,
            readerThread: Mutex::new(Some(readerThread)),
            pendingInvocations,
            nextRequestId: AtomicU64::new(1),
            protocolFailed,
            stopped: AtomicBool::new(false),
        })
    }

    /// 将一次阶段调用写入插件进程并等待同 requestId 的结果。
    ///
    /// 运行上下文：多个连接可以并发进入；响应线程按 requestId 唤醒对应 future，返回顺序不受限制。
    /// 失败语义：停止态、管道写入失败、进程退出或插件 error 帧均只返回当前调用错误。
    async fn invokeProcess(
        &self,
        invocation: RuntimeInvocation,
    ) -> Result<ExtensionAction, String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("extensionProcessStopped".to_owned());
        }
        if self.protocolFailed.load(Ordering::Acquire) {
            return Err("extensionProcessProtocolFailed".to_owned());
        }
        let requestId = self.nextRequestId.fetch_add(1, Ordering::Relaxed);
        if requestId > PROCESS_JSON_SAFE_INTEGER_MAX {
            return Err("extensionProcessRequestIdExhausted".to_owned());
        }
        let processInvocation = serializeProcessInvocation(&invocation)?;
        let (responseSender, responseReceiver) = oneshot::channel();
        {
            let mut pendingInvocations = self.pendingInvocations.lock();
            if self.stopped.load(Ordering::Acquire) {
                return Err("extensionProcessStopped".to_owned());
            }
            if self.protocolFailed.load(Ordering::Acquire) {
                return Err("extensionProcessProtocolFailed".to_owned());
            }
            pendingInvocations.insert(requestId, responseSender);
        }
        let writeResult = self
            .input
            .lock()
            .as_mut()
            .ok_or_else(|| "extensionProcessInputClosed".to_owned())
            .and_then(|input| {
                writeMessage(
                    input,
                    &HostMessage::Invoke {
                        requestId,
                        invocation: &processInvocation,
                    },
                )
                .map_err(|_| "extensionProcessWriteFailed".to_owned())
            });
        if let Err(error) = writeResult {
            self.pendingInvocations.lock().remove(&requestId);
            return Err(error);
        }
        responseReceiver
            .await
            .map_err(|_| "extensionProcessExited".to_owned())?
    }

    /// 发送停止消息、关闭输入并等待作者进程自行退出。
    ///
    /// 开放模式不注入强制超时；插件作者必须在 stop 后结束后台任务和进程。所有未完成调用会收到停止错误。
    fn stopProcess(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(input) = self.input.lock().as_mut() {
            let _ = writeMessage(input, &HostMessage::Stop);
        }
        self.input.lock().take();
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.wait();
        }
        if let Some(readerThread) = self.readerThread.lock().take() {
            let _ = readerThread.join();
        }
        failPending(&self.pendingInvocations, "extensionProcessStopped");
    }
}

/// 序列化初始化对象并验证进程协议的跨语言数字域。
///
/// 运行上下文：Native 插件不经过这里；Sidecar 与 Native Worker 的 manifest/configuration 会被
/// JavaScript JSON.parse 读取。整数超出安全域时拒绝加载，避免配置或限制字段在插件看到前已被舍入。
fn serializeProcessInitialization<T: Serialize>(value: &T) -> Result<JsonValue, PluginHostError> {
    let serialized = serde_json::to_value(value).map_err(|_| PluginHostError::Initialization)?;
    validateProcessJsonNumbers(&serialized)?;
    Ok(serialized)
}

/// 递归检查 JSON 整数均可由所有官方 SDK 精确表示；浮点数保留 JSON 自身的近似语义。
fn validateProcessJsonNumbers(value: &JsonValue) -> Result<(), PluginHostError> {
    match value {
        JsonValue::Number(number) => {
            let integerIsSafe = number
                .as_u64()
                .is_some_and(|integer| integer <= PROCESS_JSON_SAFE_INTEGER_MAX)
                || number.as_i64().is_some_and(|integer| {
                    integer >= -(PROCESS_JSON_SAFE_INTEGER_MAX as i64)
                        && integer <= PROCESS_JSON_SAFE_INTEGER_MAX as i64
                })
                || number
                    .as_f64()
                    .is_some_and(|_| !number.is_i64() && !number.is_u64());
            integerIsSafe
                .then_some(())
                .ok_or(PluginHostError::Initialization)
        }
        JsonValue::Array(items) => items.iter().try_for_each(validateProcessJsonNumbers),
        JsonValue::Object(fields) => fields.values().try_for_each(validateProcessJsonNumbers),
        _ => Ok(()),
    }
}

/// 将统一调用转换为所有官方 JSONL SDK 都能无损读取的进程协议对象。
///
/// JavaScript 数字只能精确表示 53 位整数，因此进程协议把代际限制在安全整数范围，并把“无限截止”
/// 规范化为最大的安全整数。Native ABI 仍保留完整 u64；这里只调整 JSONL 表示，避免 TypeScript
/// 插件在解析阶段静默舍入。代际超出范围时返回稳定错误，不发送已损坏的调用。
fn serializeProcessInvocation(invocation: &RuntimeInvocation) -> Result<JsonValue, String> {
    if invocation.envelope.serviceGeneration > PROCESS_JSON_SAFE_INTEGER_MAX
        || invocation.envelope.recordingGeneration > PROCESS_JSON_SAFE_INTEGER_MAX
    {
        return Err("extensionProcessIntegerOutOfRange".to_owned());
    }
    let mut serialized = serde_json::to_value(invocation)
        .map_err(|_| "extensionProcessSerializationFailed".to_owned())?;
    let envelope = serialized
        .get_mut("envelope")
        .and_then(JsonValue::as_object_mut)
        .ok_or_else(|| "extensionProcessSerializationFailed".to_owned())?;
    envelope.insert(
        "deadlineUnixMs".to_owned(),
        JsonValue::from(
            invocation
                .envelope
                .deadlineUnixMs
                .min(PROCESS_JSON_SAFE_INTEGER_MAX),
        ),
    );
    Ok(serialized)
}

impl ExtensionRuntime for ProcessExtensionRuntime {
    /// 把统一运行时调用映射为 JSONL 请求；宿主不添加调用超时或并发门禁。
    fn invoke<'a>(
        &'a self,
        invocation: RuntimeInvocation,
    ) -> Pin<Box<dyn Future<Output = Result<ExtensionAction, String>> + Send + 'a>> {
        Box::pin(async move { self.invokeProcess(invocation).await })
    }

    /// 通知进程插件停止并等待它完成作者定义的清理。
    fn stop(&self) {
        self.stopProcess();
    }
}

impl Drop for ProcessExtensionRuntime {
    /// 保证最后一个宿主引用释放时完成进程回收，避免插件工作进程成为孤儿。
    fn drop(&mut self) {
        self.stopProcess();
    }
}

/// 按运行时类型和入口扩展名构造进程命令；解释器选择属于 SDK 的跨语言约定。
fn processCommand(runtimeKind: ExtensionRuntimeKind, entryPath: &Path) -> Command {
    if runtimeKind == ExtensionRuntimeKind::NativeWorker {
        return Command::new(entryPath);
    }
    let extension = entryPath
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut command = match extension.as_str() {
        "py" => Command::new(PYTHON_INTERPRETER),
        "js" | "mjs" | "cjs" => Command::new(NODE_INTERPRETER),
        _ => Command::new(entryPath),
    };
    if matches!(extension.as_str(), "py" | "js" | "mjs" | "cjs") {
        command.arg(entryPath);
    }
    command
}

/// 接管子进程管道并完成 ready 握手；返回后输出读取器可以安全移交独立线程。
///
/// 失败语义：缺少管道、初始化写入失败、EOF 或首帧不兼容返回错误；调用方负责终止尚未发布的进程。
fn initializeChild(
    child: &mut Child,
    manifest: &JsonValue,
    configuration: &JsonValue,
) -> Result<(BufWriter<ChildStdin>, BufReader<std::process::ChildStdout>), PluginHostError> {
    let childInput = child.stdin.take().ok_or(PluginHostError::Initialization)?;
    let childOutput = child.stdout.take().ok_or(PluginHostError::Initialization)?;
    let mut input = BufWriter::new(childInput);
    writeMessage(
        &mut input,
        &HostMessage::Initialize {
            apiVersion: PROCESS_EXTENSION_API_VERSION,
            manifest,
            configuration,
        },
    )
    .map_err(PluginHostError::Worker)?;
    let mut output = BufReader::new(childOutput);
    let mut readyLine = String::new();
    if output
        .read_line(&mut readyLine)
        .map_err(PluginHostError::Worker)?
        == 0
    {
        return Err(PluginHostError::Initialization);
    }
    let ready = serde_json::from_str::<WorkerMessage>(readyLine.trim_end())
        .map_err(|_| PluginHostError::Initialization)?;
    if !matches!(
        ready,
        WorkerMessage::Ready {
            apiVersion: PROCESS_EXTENSION_API_VERSION
        }
    ) {
        return Err(PluginHostError::Initialization);
    }
    Ok((input, output))
}

/// 终止尚未发布的启动失败进程并同步回收句柄；失败清理不得覆盖原始加载错误。
fn terminateFailedStartup(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// 把一条消息序列化为原子 JSONL 帧并立即刷新，避免低流量调用停留在用户态缓冲。
fn writeMessage<T: Serialize>(
    input: &mut BufWriter<ChildStdin>,
    message: &T,
) -> std::io::Result<()> {
    serde_json::to_writer(&mut *input, message).map_err(std::io::Error::other)?;
    input.write_all(b"\n")?;
    input.flush()
}

/// 持续读取插件响应并按 requestId 分发；标准输出出现坏帧时终止协议并失败全部等待者。
fn readWorkerMessages(
    mut output: BufReader<std::process::ChildStdout>,
    pendingInvocations: &Arc<Mutex<HashMap<u64, PendingInvocation>>>,
    protocolFailed: &Arc<AtomicBool>,
    child: &Arc<Mutex<Option<Child>>>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match output.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => match serde_json::from_str::<WorkerMessage>(line.trim_end()) {
                Ok(WorkerMessage::Result { requestId, action }) => {
                    completeInvocation(pendingInvocations, requestId, Ok(action));
                }
                Ok(WorkerMessage::Error { requestId, message }) => {
                    completeInvocation(pendingInvocations, requestId, Err(message))
                }
                Ok(WorkerMessage::Ready { .. }) | Err(_) => {
                    markProtocolFailed(
                        pendingInvocations,
                        protocolFailed,
                        "extensionProcessProtocolInvalid",
                    );
                    terminateProtocolChild(child);
                    return;
                }
            },
            Err(_) => break,
        }
    }
    markProtocolFailed(pendingInvocations, protocolFailed, "extensionProcessExited");
}

/// 原子发布协议终止并失败全部已登记调用；锁内置位避免新调用落在 drain 之后永久等待。
fn markProtocolFailed(
    pendingInvocations: &Arc<Mutex<HashMap<u64, PendingInvocation>>>,
    protocolFailed: &Arc<AtomicBool>,
    errorCode: &str,
) {
    let senders = {
        let mut pendingInvocations = pendingInvocations.lock();
        protocolFailed.store(true, Ordering::Release);
        pendingInvocations
            .drain()
            .map(|(_, sender)| sender)
            .collect::<Vec<_>>()
    };
    for sender in senders {
        let _ = sender.send(Err(errorCode.to_owned()));
    }
}

/// 终止输出协议已经损坏的作者进程；仅请求 kill，统一 stop 路径负责 wait 与线程回收。
fn terminateProtocolChild(child: &Arc<Mutex<Option<Child>>>) {
    if let Some(child) = child.lock().as_mut() {
        let _ = child.kill();
    }
}

/// 完成一个仍在等待的调用；迟到或重复 requestId 被忽略，不影响其他调用。
fn completeInvocation(
    pendingInvocations: &Arc<Mutex<HashMap<u64, PendingInvocation>>>,
    requestId: u64,
    result: Result<ExtensionAction, String>,
) {
    if let Some(sender) = pendingInvocations.lock().remove(&requestId) {
        let _ = sender.send(result);
    }
}

/// 失败全部等待调用并清空映射；先提取 sender 再发送，避免唤醒任务重入同一锁。
fn failPending(pendingInvocations: &Arc<Mutex<HashMap<u64, PendingInvocation>>>, errorCode: &str) {
    let senders = pendingInvocations
        .lock()
        .drain()
        .map(|(_, sender)| sender)
        .collect::<Vec<_>>();
    for sender in senders {
        let _ = sender.send(Err(errorCode.to_owned()));
    }
}
