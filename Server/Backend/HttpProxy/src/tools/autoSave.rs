//! 提供录制会话的定时、计数触发和手动触发自动保存能力。

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as base64Standard};
use capture_core::{
    BodyResponse, HarExportRequest, MessageSide, RecordingSession, TransactionDetailRecord,
    TransactionSummary,
};
use parking_lot::{Mutex as SynchronousMutex, RwLock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{sync::Mutex as AsyncMutex, time::sleep};

const maximumDirectoryCharacters: usize = 4_096;
const maximumIntervalSeconds: u64 = 86_400;
const maximumEveryNTransactions: usize = 100_000;
const maximumFiles: usize = 1_000;
const autoSavePrefix: &str = "recording-";
static archiveSequence: AtomicU64 = AtomicU64::new(0);

/// 标识自动保存输出格式；Native 为可复原的 JSON 快照，HAR 为标准 1.2 归档。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoSaveFormat {
    Native,
    Har,
}

impl AutoSaveFormat {
    /// 返回输出扩展名，避免运行时按自由文本构造文件路径。
    const fn extension(self) -> &'static str {
        match self {
            Self::Native => "json",
            Self::Har => "har",
        }
    }
}

/// 描述会话自动保存的完整配置；间隔和计数可分别为零，至少一个触发器必须启用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AutoSaveConfiguration {
    pub enabled: bool,
    pub directory: String,
    pub intervalSeconds: u64,
    pub everyNTransactions: usize,
    pub format: AutoSaveFormat,
    pub maxFiles: usize,
    pub includeBodies: bool,
}

impl Default for AutoSaveConfiguration {
    /// 默认关闭自动保存，避免服务首次启动时在用户目录创建会话文件。
    fn default() -> Self {
        Self {
            enabled: false,
            directory: String::new(),
            intervalSeconds: 300,
            everyNTransactions: 0,
            format: AutoSaveFormat::Native,
            maxFiles: 20,
            includeBodies: true,
        }
    }
}

impl AutoSaveConfiguration {
    /// 校验目录、触发条件和轮转边界；禁用时允许保留未完成草稿以便界面再次启用。
    pub fn validate(&self) -> Result<(), AutoSaveError> {
        if self.intervalSeconds > maximumIntervalSeconds {
            return Err(AutoSaveError::InvalidInterval);
        }
        if self.everyNTransactions > maximumEveryNTransactions {
            return Err(AutoSaveError::InvalidTransactionThreshold);
        }
        if !(1..=maximumFiles).contains(&self.maxFiles) {
            return Err(AutoSaveError::InvalidFileLimit);
        }
        if !self.enabled {
            return Ok(());
        }
        if self.intervalSeconds == 0 && self.everyNTransactions == 0 {
            return Err(AutoSaveError::NoTrigger);
        }
        if self.directory.trim().is_empty()
            || self.directory.chars().count() > maximumDirectoryCharacters
            || !Path::new(&self.directory).is_absolute()
        {
            return Err(AutoSaveError::InvalidDirectory);
        }
        Ok(())
    }
}

/// 提供控制面读取的自动保存公开状态；路径是用户主动配置的输出位置，最近文件路径仅用于本机桌面“打开目录”。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSavePublicState {
    #[serde(flatten)]
    pub configuration: AutoSaveConfiguration,
    pub lastSavedAtMilliseconds: Option<u64>,
    pub lastSavedPath: Option<String>,
    pub lastError: Option<String>,
}

/// 保存触发水位和最近一次结果；所有修改由 saveLock 串行化，防止时间和计数触发重复导出同一快照。
struct AutoSaveRuntimeState {
    configuredAtMilliseconds: u64,
    lastSavedAtMilliseconds: Option<u64>,
    lastSavedTransactionCount: usize,
    lastSavedPath: Option<String>,
    lastError: Option<String>,
}

/// 管理自动保存配置和后台调度；导出在独占锁外准备快照，避免长 I/O 阻塞配置读取。
#[derive(Clone)]
pub struct AutoSaveTool {
    configuration: Arc<RwLock<AutoSaveConfiguration>>,
    runtime: Arc<SynchronousMutex<AutoSaveRuntimeState>>,
    saveLock: Arc<AsyncMutex<()>>,
}

impl AutoSaveTool {
    /// 创建自动保存运行时并启动低频检查器；检查器仅在命中明确触发条件时才读取录制数据。
    pub fn new(
        configuration: AutoSaveConfiguration,
        recording: RecordingSession,
    ) -> Result<Self, AutoSaveError> {
        configuration.validate()?;
        let tool = Self {
            configuration: Arc::new(RwLock::new(configuration)),
            runtime: Arc::new(SynchronousMutex::new(AutoSaveRuntimeState {
                configuredAtMilliseconds: currentTimeMilliseconds(),
                lastSavedAtMilliseconds: None,
                lastSavedTransactionCount: 0,
                lastSavedPath: None,
                lastError: None,
            })),
            saveLock: Arc::new(AsyncMutex::new(())),
        };
        let scheduler = tool.clone();
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(1)).await;
                let _ = scheduler.saveIfDue(&recording).await;
            }
        });
        Ok(tool)
    }

    /// 返回配置副本，避免控制层在无锁状态下读取后续热更新的一部分字段。
    pub fn configuration(&self) -> AutoSaveConfiguration {
        self.configuration.read().clone()
    }

    /// 原子替换自动保存配置；切换目录、格式或触发器只作用于下一次导出。
    pub fn replaceConfiguration(
        &self,
        configuration: AutoSaveConfiguration,
    ) -> Result<(), AutoSaveError> {
        configuration.validate()?;
        *self.configuration.write() = configuration;
        let mut runtime = self.runtime.lock();
        runtime.configuredAtMilliseconds = currentTimeMilliseconds();
        runtime.lastSavedTransactionCount = 0;
        Ok(())
    }

    /// 返回当前公开状态，调用方不会看到内部计数水位或任何临时输出路径。
    pub fn publicState(&self) -> AutoSavePublicState {
        let runtime = self.runtime.lock();
        AutoSavePublicState {
            configuration: self.configuration(),
            lastSavedAtMilliseconds: runtime.lastSavedAtMilliseconds,
            lastSavedPath: runtime.lastSavedPath.clone(),
            lastError: runtime.lastError.clone(),
        }
    }

    /// 根据间隔或新增事务阈值判断是否导出；未命中时保持完全无 I/O。
    pub async fn saveIfDue(&self, recording: &RecordingSession) -> Result<bool, AutoSaveError> {
        let configuration = self.configuration();
        if !configuration.enabled {
            return Ok(false);
        }
        let snapshot = recording
            .snapshot()
            .await
            .map_err(|_| AutoSaveError::ExportFailed)?;
        let now = currentTimeMilliseconds();
        let (dueByInterval, dueByTransactionCount) = {
            let runtime = self.runtime.lock();
            let intervalStart = runtime
                .lastSavedAtMilliseconds
                .unwrap_or(runtime.configuredAtMilliseconds);
            let dueByInterval = configuration.intervalSeconds > 0
                && now.saturating_sub(intervalStart)
                    >= configuration.intervalSeconds.saturating_mul(1_000);
            let dueByTransactionCount = configuration.everyNTransactions > 0
                && snapshot.transactionCount
                    >= runtime
                        .lastSavedTransactionCount
                        .saturating_add(configuration.everyNTransactions);
            (dueByInterval, dueByTransactionCount)
        };
        if !dueByInterval && !dueByTransactionCount {
            return Ok(false);
        }
        self.save(recording, configuration, snapshot.transactionCount)
            .await?;
        Ok(true)
    }

    /// 立即保存当前录制会话；手动调用不受时间或事务阈值限制，但仍复用轮转和错误状态。
    pub async fn saveNow(
        &self,
        recording: &RecordingSession,
    ) -> Result<AutoSavePublicState, AutoSaveError> {
        let configuration = self.configuration();
        if !configuration.enabled {
            return Err(AutoSaveError::Disabled);
        }
        let snapshot = recording
            .snapshot()
            .await
            .map_err(|_| AutoSaveError::ExportFailed)?;
        self.save(recording, configuration, snapshot.transactionCount)
            .await?;
        Ok(self.publicState())
    }

    /// 串行写入一次一致会话快照；失败只写运行状态，调用方可继续代理并在后续触发再次尝试。
    async fn save(
        &self,
        recording: &RecordingSession,
        configuration: AutoSaveConfiguration,
        transactionCount: usize,
    ) -> Result<(), AutoSaveError> {
        let _saveGuard = self.saveLock.lock().await;
        let result = exportRecording(recording, &configuration).await;
        let mut runtime = self.runtime.lock();
        match result {
            Ok(path) => {
                runtime.lastSavedAtMilliseconds = Some(currentTimeMilliseconds());
                runtime.lastSavedTransactionCount = transactionCount;
                runtime.lastSavedPath = Some(path.to_string_lossy().into_owned());
                runtime.lastError = None;
                Ok(())
            }
            Err(error) => {
                runtime.lastError = Some(error.code().to_owned());
                Err(error)
            }
        }
    }
}

/// 导出完整会话并轮转本工具创建的旧归档；写入使用临时同目录文件后 rename，避免读者看到半截 JSON。
async fn exportRecording(
    recording: &RecordingSession,
    configuration: &AutoSaveConfiguration,
) -> Result<PathBuf, AutoSaveError> {
    let directory = PathBuf::from(&configuration.directory);
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|_| AutoSaveError::WriteFailed)?;
    let timestamp = currentTimeMilliseconds();
    let sequence = archiveSequence.fetch_add(1, Ordering::Relaxed);
    let fileName = format!(
        "{autoSavePrefix}{timestamp}-{sequence}.{}",
        configuration.format.extension()
    );
    let finalPath = directory.join(fileName);
    let temporaryPath = directory.join(format!(".{autoSavePrefix}{timestamp}-{sequence}.partial"));
    let bytes = serializeArchive(recording, configuration).await?;
    tokio::fs::write(&temporaryPath, bytes)
        .await
        .map_err(|_| AutoSaveError::WriteFailed)?;
    if tokio::fs::rename(&temporaryPath, &finalPath).await.is_err() {
        let _ = tokio::fs::remove_file(&temporaryPath).await;
        return Err(AutoSaveError::WriteFailed);
    }
    rotateArchives(&directory, configuration.maxFiles).await?;
    Ok(finalPath)
}

/// 根据配置创建标准 HAR 或原生 JSON；正文仅在显式选中时读取，默认路径保持元数据级 I/O。
async fn serializeArchive(
    recording: &RecordingSession,
    configuration: &AutoSaveConfiguration,
) -> Result<Vec<u8>, AutoSaveError> {
    match configuration.format {
        AutoSaveFormat::Har => recording
            .buildHarExport(HarExportRequest {
                includeBodies: configuration.includeBodies,
                transactionIds: Vec::new(),
            })
            .await
            .map_err(|_| AutoSaveError::ExportFailed)
            .and_then(|archive| {
                serde_json::to_vec(&archive).map_err(|_| AutoSaveError::ExportFailed)
            }),
        AutoSaveFormat::Native => buildNativeArchive(recording, configuration.includeBodies).await,
    }
}

/// 构造自描述原生会话归档；正文为 Base64，禁用正文时只导出正文元信息而不读取 spill 文件。
async fn buildNativeArchive(
    recording: &RecordingSession,
    includeBodies: bool,
) -> Result<Vec<u8>, AutoSaveError> {
    let recordingSnapshot = recording
        .snapshot()
        .await
        .map_err(|_| AutoSaveError::ExportFailed)?;
    let summaries = recording
        .listMetadata()
        .await
        .map_err(|_| AutoSaveError::ExportFailed)?;
    let mut transactions = Vec::with_capacity(summaries.len());
    for summary in summaries {
        transactions.push(buildNativeTransaction(recording, summary, includeBodies).await?);
    }
    serde_json::to_vec(&NativeArchive {
        format: "capture-recording-v1",
        savedAtMilliseconds: currentTimeMilliseconds(),
        recording: recordingSnapshot,
        transactions,
    })
    .map_err(|_| AutoSaveError::ExportFailed)
}

/// 读取一条事务详情；正文读取保持按侧独立，任意读取失败时拒绝半完整归档。
async fn buildNativeTransaction(
    recording: &RecordingSession,
    summary: TransactionSummary,
    includeBodies: bool,
) -> Result<NativeTransaction, AutoSaveError> {
    let TransactionDetailRecord {
        requestHeaders,
        responseHeaders,
        requestBody,
        responseBody,
        ..
    } = recording
        .getTransactionDetail(&summary.transactionId)
        .await
        .map_err(|_| AutoSaveError::ExportFailed)?;
    let requestBodyBase64 = readBody(
        recording,
        &summary.transactionId,
        MessageSide::Request,
        includeBodies,
    )
    .await?;
    let responseBodyBase64 = readBody(
        recording,
        &summary.transactionId,
        MessageSide::Response,
        includeBodies,
    )
    .await?;
    Ok(NativeTransaction {
        summary,
        requestHeaders,
        responseHeaders,
        requestBody,
        responseBody,
        requestBodyBase64,
        responseBodyBase64,
    })
}

/// 在需要正文时按事务侧读取并编码；未存正文与禁用正文统一为 null，不伪造空字节内容。
async fn readBody(
    recording: &RecordingSession,
    transactionId: &str,
    side: MessageSide,
    includeBodies: bool,
) -> Result<Option<String>, AutoSaveError> {
    if !includeBodies {
        return Ok(None);
    }
    match recording.getBody(transactionId, side).await {
        Ok(BodyResponse { bytes, .. }) => Ok(Some(base64Standard.encode(bytes))),
        Err(capture_core::CaptureError::BodyNotFound) => Ok(None),
        Err(_) => Err(AutoSaveError::ExportFailed),
    }
}

/// 删除超出保留数量的本工具归档；只匹配固定前缀与 json/har 扩展，绝不触碰用户其它文件。
async fn rotateArchives(directory: &Path, maximum: usize) -> Result<(), AutoSaveError> {
    let mut entries = tokio::fs::read_dir(directory)
        .await
        .map_err(|_| AutoSaveError::WriteFailed)?;
    let mut archives = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| AutoSaveError::WriteFailed)?
    {
        let path = entry.path();
        if isAutoSaveArchive(&path) {
            archives.push(path);
        }
    }
    archives.sort();
    let excess = archives.len().saturating_sub(maximum);
    for archive in archives.into_iter().take(excess) {
        tokio::fs::remove_file(archive)
            .await
            .map_err(|_| AutoSaveError::WriteFailed)?;
    }
    Ok(())
}

/// 判断文件是否由自动保存工具创建；名称和扩展均固定，拒绝接受任意相邻临时或用户文档。
fn isAutoSaveArchive(path: &Path) -> bool {
    let Some(fileName) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    fileName.starts_with(autoSavePrefix)
        && (fileName.ends_with(".json") || fileName.ends_with(".har"))
}

/// 返回当前 UNIX 毫秒；系统时钟异常时使用零，文件名仍会包含固定前缀并受轮转控制。
fn currentTimeMilliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

/// 表示原生归档外层结构；版本字段防止未来导入器把不同序列化契约混读。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeArchive {
    format: &'static str,
    savedAtMilliseconds: u64,
    recording: capture_core::RecordingSnapshot,
    transactions: Vec<NativeTransaction>,
}

/// 表示原生归档中的一条完整事务；正文元信息始终保留，正文内容遵循 includeBodies 开关。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeTransaction {
    summary: TransactionSummary,
    requestHeaders: Vec<capture_core::HeaderField>,
    responseHeaders: Vec<capture_core::HeaderField>,
    requestBody: Option<capture_core::BodyHandleMeta>,
    responseBody: Option<capture_core::BodyHandleMeta>,
    requestBodyBase64: Option<String>,
    responseBodyBase64: Option<String>,
}

/// 定义自动保存配置、导出和轮转的稳定失败码；目录、底层错误与报文内容不会进入公开错误。
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AutoSaveError {
    #[error("error.autoSave.invalidDirectory")]
    InvalidDirectory,
    #[error("error.autoSave.invalidInterval")]
    InvalidInterval,
    #[error("error.autoSave.invalidTransactionThreshold")]
    InvalidTransactionThreshold,
    #[error("error.autoSave.invalidFileLimit")]
    InvalidFileLimit,
    #[error("error.autoSave.noTrigger")]
    NoTrigger,
    #[error("error.autoSave.disabled")]
    Disabled,
    #[error("error.autoSave.exportFailed")]
    ExportFailed,
    #[error("error.autoSave.writeFailed")]
    WriteFailed,
}

impl AutoSaveError {
    /// 返回控制 API 状态与测试可稳定识别的机器码。
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidDirectory => "autoSaveInvalidDirectory",
            Self::InvalidInterval => "autoSaveInvalidInterval",
            Self::InvalidTransactionThreshold => "autoSaveInvalidTransactionThreshold",
            Self::InvalidFileLimit => "autoSaveInvalidFileLimit",
            Self::NoTrigger => "autoSaveNoTrigger",
            Self::Disabled => "autoSaveDisabled",
            Self::ExportFailed => "autoSaveExportFailed",
            Self::WriteFailed => "autoSaveWriteFailed",
        }
    }
}
