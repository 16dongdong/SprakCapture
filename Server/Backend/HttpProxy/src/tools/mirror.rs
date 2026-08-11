//! 提供按事务方向异步落盘的 HTTP 报文镜像能力。

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use http::{HeaderMap, StatusCode, Uri, Version};
use location_core::{LocationPattern, ResolvedLocation};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    sync::{Notify, mpsc},
    time::timeout,
};

use crate::{
    PipelineContext, PipelineDirective, PipelineError, PipelineTool, ToolId, ToolPhase,
    ToolRegistration,
    tools::locationScope::{matchesLocations, validateLocations},
};

const maximumQueueLength: usize = 4_096;
const internalQueueCapacity: usize = maximumQueueLength;
const maximumRootDirectoryCharacters: usize = 4_096;
const maximumPathSegmentCharacters: usize = 120;

/// 定义镜像文件组织方式；层级模式保留 host/path 目录，扁平模式适合导入其它离线工具。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MirrorLayout {
    Hierarchical,
    Flat,
}

/// 定义写入队列已满时的行为；默认丢弃保证磁盘慢速不会阻塞代理热路径。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MirrorOverflowPolicy {
    Drop,
    Block,
}

/// 描述镜像工具的完整持久化配置；路径和规则只由控制面更新，数据面仅读取快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct MirrorConfiguration {
    pub enabled: bool,
    pub rootDirectory: String,
    pub locations: Vec<LocationPattern>,
    pub mirrorRequest: bool,
    pub mirrorResponse: bool,
    pub layout: MirrorLayout,
    pub onOverflow: MirrorOverflowPolicy,
    pub maxQueueLength: usize,
}

impl Default for MirrorConfiguration {
    /// 默认关闭镜像，避免未经用户配置就在数据目录产生报文副本。
    fn default() -> Self {
        Self {
            enabled: false,
            rootDirectory: String::new(),
            locations: Vec::new(),
            mirrorRequest: true,
            mirrorResponse: true,
            layout: MirrorLayout::Hierarchical,
            onOverflow: MirrorOverflowPolicy::Drop,
            maxQueueLength: 256,
        }
    }
}

impl MirrorConfiguration {
    /// 校验镜像根、作用域和队列边界；启用状态必须至少选择一个方向并使用绝对目录。
    pub fn validate(&self) -> Result<(), MirrorError> {
        validateLocations(&self.locations).map_err(MirrorError::Tool)?;
        if !(1..=maximumQueueLength).contains(&self.maxQueueLength) {
            return Err(MirrorError::InvalidQueueLength);
        }
        if !self.enabled {
            return Ok(());
        }
        if !self.mirrorRequest && !self.mirrorResponse {
            return Err(MirrorError::NoEnabledDirection);
        }
        if self.rootDirectory.trim().is_empty()
            || self.rootDirectory.chars().count() > maximumRootDirectoryCharacters
            || !Path::new(&self.rootDirectory).is_absolute()
        {
            return Err(MirrorError::InvalidRootDirectory);
        }
        Ok(())
    }
}

/// 提供前端和 MCP 可读取的镜像运行状态；不返回写入路径以避免把本机目录泄露给无关调用方。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorPublicState {
    #[serde(flatten)]
    pub configuration: MirrorConfiguration,
    pub writtenFiles: u64,
    pub droppedWrites: u64,
    pub lastError: Option<String>,
}

/// 保存异步写入完成前不可变的报文副本；生成路径只来自受控 Location 与稳定事务标识。
struct MirrorTask {
    rootDirectory: PathBuf,
    layout: MirrorLayout,
    transactionId: String,
    location: ResolvedLocation,
    side: MirrorSide,
    payload: Vec<u8>,
}

/// 标识镜像文件来源方向，文件名固定使用稳定小写名称。
#[derive(Clone, Copy)]
enum MirrorSide {
    Request,
    Response,
}

impl MirrorSide {
    /// 返回输出文件名和报文开头的稳定方向名称。
    const fn asStr(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

/// 保存队列计数、文件计数和最后失败码；所有字段均可无锁读取以保持代理钩子轻量。
struct MirrorRuntimeState {
    queuedWrites: AtomicUsize,
    writtenFiles: AtomicU64,
    droppedWrites: AtomicU64,
    lastError: RwLock<Option<String>>,
    queueAdvanced: Notify,
}

/// 将配置快照、单写入队列和后台落盘任务组合为可热更新的流水线工具。
#[derive(Clone)]
pub struct MirrorTool {
    configuration: Arc<RwLock<MirrorConfiguration>>,
    sender: mpsc::Sender<MirrorTask>,
    runtime: Arc<MirrorRuntimeState>,
}

impl MirrorTool {
    /// 创建后台单写入器；单消费者保证同名路径不会并发截断，代理线程只提交有界任务。
    pub fn new(configuration: MirrorConfiguration) -> Result<Self, MirrorError> {
        configuration.validate()?;
        let (sender, receiver) = mpsc::channel(internalQueueCapacity);
        let runtime = Arc::new(MirrorRuntimeState {
            queuedWrites: AtomicUsize::new(0),
            writtenFiles: AtomicU64::new(0),
            droppedWrites: AtomicU64::new(0),
            lastError: RwLock::new(None),
            queueAdvanced: Notify::new(),
        });
        tokio::spawn(runMirrorWriter(receiver, runtime.clone()));
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
            sender,
            runtime,
        })
    }

    /// 返回独立配置副本，调用方不能通过快照绕过热更新校验。
    pub fn configuration(&self) -> MirrorConfiguration {
        self.configuration.read().clone()
    }

    /// 原子替换完整配置；新配置只影响后续报文，已入队任务保留提交时的目录与布局。
    pub fn replaceConfiguration(
        &self,
        configuration: MirrorConfiguration,
    ) -> Result<(), MirrorError> {
        configuration.validate()?;
        *self.configuration.write() = configuration;
        Ok(())
    }

    /// 返回配置与累计 I/O 状态；写入失败只暴露稳定错误码，不包含底层目录和操作系统文本。
    pub fn publicState(&self) -> MirrorPublicState {
        MirrorPublicState {
            configuration: self.configuration(),
            writtenFiles: self.runtime.writtenFiles.load(Ordering::Relaxed),
            droppedWrites: self.runtime.droppedWrites.load(Ordering::Relaxed),
            lastError: self.runtime.lastError.read().clone(),
        }
    }

    /// 在监听器停止前等待已提交任务完成；超时保留队列供进程生命周期继续处理，不中断数据面清理。
    pub async fn flush(&self, wait: Duration) -> Result<(), MirrorError> {
        timeout(wait, async {
            while self.runtime.queuedWrites.load(Ordering::Acquire) > 0 {
                self.runtime.queueAdvanced.notified().await;
            }
        })
        .await
        .map_err(|_| MirrorError::FlushTimeout)
    }

    /// 把一个已物化报文提交给后台写入器；Drop 策略不等待 I/O，Block 策略只在用户明确选择时施加背压。
    async fn enqueue(&self, task: MirrorTask, configuration: &MirrorConfiguration) {
        match configuration.onOverflow {
            MirrorOverflowPolicy::Drop => {
                if !reserveMirrorSlot(&self.runtime, configuration.maxQueueLength) {
                    self.runtime.droppedWrites.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                if self.sender.try_send(task).is_err() {
                    releaseMirrorSlot(&self.runtime);
                    self.runtime.droppedWrites.fetch_add(1, Ordering::Relaxed);
                }
            }
            MirrorOverflowPolicy::Block => {
                while !reserveMirrorSlot(&self.runtime, configuration.maxQueueLength) {
                    self.runtime.queueAdvanced.notified().await;
                }
                if self.sender.send(task).await.is_err() {
                    releaseMirrorSlot(&self.runtime);
                    self.runtime.droppedWrites.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// 按当前配置与作用域判断请求是否需要镜像；正文需求由 registration 同一快照决定。
    async fn mirrorRequest(
        &self,
        context: &PipelineContext,
        configuration: &MirrorConfiguration,
    ) -> Result<(), PipelineError> {
        let matches =
            matchesLocations(&configuration.locations, &context.location).map_err(|error| {
                PipelineError::ToolFailed {
                    toolId: ToolId::Mirror,
                    code: error.code().to_owned(),
                }
            })?;
        if !configuration.mirrorRequest || !matches {
            return Ok(());
        }
        let body = context.request.body.clone().unwrap_or_default();
        let payload = serializeRequest(
            &context.request.method,
            &context.request.uri,
            context.request.version,
            &context.request.headers,
            &body,
        );
        self.enqueue(
            createTask(context, configuration, MirrorSide::Request, payload),
            configuration,
        )
        .await;
        Ok(())
    }

    /// 按当前配置与作用域判断响应是否需要镜像；响应草稿不存在时说明请求尚未产生可记录响应。
    async fn mirrorResponse(
        &self,
        context: &PipelineContext,
        configuration: &MirrorConfiguration,
    ) -> Result<(), PipelineError> {
        let matches =
            matchesLocations(&configuration.locations, &context.location).map_err(|error| {
                PipelineError::ToolFailed {
                    toolId: ToolId::Mirror,
                    code: error.code().to_owned(),
                }
            })?;
        if !configuration.mirrorResponse || !matches {
            return Ok(());
        }
        let Some(response) = context.response.as_ref() else {
            return Ok(());
        };
        let body = response.body.clone().unwrap_or_default();
        let payload =
            serializeResponse(response.status, response.version, &response.headers, &body);
        self.enqueue(
            createTask(context, configuration, MirrorSide::Response, payload),
            configuration,
        )
        .await;
        Ok(())
    }
}

#[async_trait]
impl PipelineTool for MirrorTool {
    /// 仅在启用且选择方向时要求有界正文副本，关闭状态完全不影响原有流式转发路径。
    fn registration(&self) -> ToolRegistration {
        let configuration = self.configuration();
        let mut registration = ToolRegistration::new(
            ToolId::Mirror,
            vec![ToolPhase::Request, ToolPhase::Response],
            configuration.enabled,
        );
        if configuration.mirrorRequest {
            registration = registration.withRequestBody();
        }
        if configuration.mirrorResponse {
            registration = registration.withResponseBody();
        }
        registration
    }

    /// 在请求工具阶段提交报文快照；写盘始终由后台工作器完成。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let configuration = self.configuration();
        self.mirrorRequest(context, &configuration).await?;
        Ok(PipelineDirective::Continue)
    }

    /// 在响应工具阶段提交报文快照；短路响应同样经过该阶段而保持镜像语义一致。
    async fn onResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let configuration = self.configuration();
        self.mirrorResponse(context, &configuration).await?;
        Ok(PipelineDirective::Continue)
    }
}

/// 为一次镜像任务复制不可变目标信息；录制暂停时使用匿名事务标识，仍保证文件名不会穿越目录。
fn createTask(
    context: &PipelineContext,
    configuration: &MirrorConfiguration,
    side: MirrorSide,
    payload: Vec<u8>,
) -> MirrorTask {
    MirrorTask {
        rootDirectory: PathBuf::from(&configuration.rootDirectory),
        layout: configuration.layout,
        transactionId: context
            .transactionId
            .clone()
            .unwrap_or_else(|| "unrecorded".to_owned()),
        location: context.location.clone(),
        side,
        payload,
    }
}

/// 后台顺序消费写入任务；任何单次失败只更新公开错误状态，不会终止后续报文镜像。
async fn runMirrorWriter(
    mut receiver: mpsc::Receiver<MirrorTask>,
    runtime: Arc<MirrorRuntimeState>,
) {
    while let Some(task) = receiver.recv().await {
        match writeMirrorTask(task).await {
            Ok(()) => {
                runtime.writtenFiles.fetch_add(1, Ordering::Relaxed);
                *runtime.lastError.write() = None;
            }
            Err(error) => *runtime.lastError.write() = Some(error.code().to_owned()),
        }
        releaseMirrorSlot(&runtime);
    }
}

/// 在根目录内创建受控层级并原子写入单个报文文件；动态段先清洗，因而不会生成 `.` 或 `..` 路径。
async fn writeMirrorTask(task: MirrorTask) -> Result<(), MirrorError> {
    let destination = mirrorDestination(&task)?;
    let parent = destination
        .parent()
        .ok_or(MirrorError::InvalidRootDirectory)?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|_| MirrorError::WriteFailed)?;
    tokio::fs::write(destination, task.payload)
        .await
        .map_err(|_| MirrorError::WriteFailed)
}

/// 根据布局生成文件目标；所有外部网络字段均经 segment 清洗，最终路径绝不离开用户指定根目录。
fn mirrorDestination(task: &MirrorTask) -> Result<PathBuf, MirrorError> {
    let fileName = format!(
        "{}_{}_{}.http",
        currentTimestampMilliseconds(),
        safeSegment(&task.transactionId),
        task.side.asStr(),
    );
    match task.layout {
        MirrorLayout::Flat => Ok(task.rootDirectory.join(fileName)),
        MirrorLayout::Hierarchical => {
            let mut destination = task.rootDirectory.join(safeSegment(&task.location.host));
            for segment in task
                .location
                .path
                .split('/')
                .filter(|segment| !segment.is_empty())
            {
                destination.push(safeSegment(segment));
            }
            Ok(destination.join(fileName))
        }
    }
}

/// 把任意协议字段压缩为单个安全文件名段；超过长度后截断，避免路径总长度随 URL 无界增长。
fn safeSegment(value: &str) -> String {
    let mut segment = value
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => character,
            _ => '_',
        })
        .take(maximumPathSegmentCharacters)
        .collect::<String>();
    if segment.is_empty() || segment == "." || segment == ".." {
        segment = "_".to_owned();
    }
    segment
}

/// 使用当前 UNIX 毫秒生成碰撞概率极低的时间前缀；同毫秒事务再由 UUID 事务 ID 区分。
fn currentTimestampMilliseconds() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

/// 序列化请求行、头和正文；头按 HTTP 原样字节复制，正文紧随空行便于离线工具读取。
fn serializeRequest(
    method: &http::Method,
    uri: &Uri,
    version: Version,
    headers: &HeaderMap,
    body: &Bytes,
) -> Vec<u8> {
    let mut payload = format!("{method} {uri} {}\r\n", versionText(version)).into_bytes();
    appendHeaders(&mut payload, headers);
    payload.extend_from_slice(b"\r\n");
    payload.extend_from_slice(body);
    payload
}

/// 序列化状态行、头和正文；无理由短语的状态仍保留数字码以保证二进制报文可定位。
fn serializeResponse(
    status: StatusCode,
    version: Version,
    headers: &HeaderMap,
    body: &Bytes,
) -> Vec<u8> {
    let reason = status.canonical_reason().unwrap_or_default();
    let mut payload =
        format!("{} {} {reason}\r\n", versionText(version), status.as_u16()).into_bytes();
    appendHeaders(&mut payload, headers);
    payload.extend_from_slice(b"\r\n");
    payload.extend_from_slice(body);
    payload
}

/// 追加未合并的 HTTP 头字段，重复头的顺序保持 HeaderMap 提供的稳定迭代顺序。
fn appendHeaders(payload: &mut Vec<u8>, headers: &HeaderMap) {
    for (name, value) in headers {
        payload.extend_from_slice(name.as_str().as_bytes());
        payload.extend_from_slice(b": ");
        payload.extend_from_slice(value.as_bytes());
        payload.extend_from_slice(b"\r\n");
    }
}

/// 将 HTTP 版本格式化为状态行使用的稳定文本，未知版本保持显式占位而不是错误伪造 HTTP/1.1。
fn versionText(version: Version) -> &'static str {
    match version {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2.0",
        Version::HTTP_3 => "HTTP/3.0",
        _ => "HTTP/?",
    }
}

/// 在配置上限内预约一个队列槽位；CAS 保证多个 HTTP 任务并发提交时不会超额。
fn reserveMirrorSlot(runtime: &MirrorRuntimeState, maximum: usize) -> bool {
    let mut current = runtime.queuedWrites.load(Ordering::Acquire);
    loop {
        if current >= maximum {
            return false;
        }
        match runtime.queuedWrites.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(updated) => current = updated,
        }
    }
}

/// 在消费或提交失败后释放一个槽位并唤醒所有等待 Block 策略的请求任务。
fn releaseMirrorSlot(runtime: &MirrorRuntimeState) {
    runtime.queuedWrites.fetch_sub(1, Ordering::AcqRel);
    runtime.queueAdvanced.notify_waiters();
}

/// 表示镜像配置和异步写入的稳定失败，不携带路径、报文或操作系统错误细节。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MirrorError {
    #[error("error.mirror.invalidRootDirectory")]
    InvalidRootDirectory,
    #[error("error.mirror.invalidQueueLength")]
    InvalidQueueLength,
    #[error("error.mirror.noEnabledDirection")]
    NoEnabledDirection,
    #[error("error.mirror.writeFailed")]
    WriteFailed,
    #[error("error.mirror.flushTimeout")]
    FlushTimeout,
    #[error(transparent)]
    Tool(#[from] super::ToolError),
}

impl MirrorError {
    /// 返回控制面和测试使用的稳定机器错误码。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidRootDirectory => "mirrorInvalidRootDirectory",
            Self::InvalidQueueLength => "mirrorInvalidQueueLength",
            Self::NoEnabledDirection => "mirrorNoEnabledDirection",
            Self::WriteFailed => "mirrorWriteFailed",
            Self::FlushTimeout => "mirrorFlushTimeout",
            Self::Tool(error) => error.code(),
        }
    }
}
