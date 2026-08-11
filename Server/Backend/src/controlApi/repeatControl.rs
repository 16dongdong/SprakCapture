use std::{collections::BTreeMap, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD as base64Standard};
use bytes::Bytes;
use capture_core::{
    BeginTransaction, BodyWrite, HeaderField, MessageSide, RecordingSession, TransactionCompletion,
    TransactionError, TransactionProgressUpdate, TransactionProtocol, TransactionUpdate,
    TransactionUserUpdate, currentTimeMilliseconds,
};
use http::{
    HeaderMap, HeaderValue, Method, Uri, Version,
    header::{CONTENT_ENCODING, CONTENT_TYPE, HOST, HeaderName},
};
use http_proxy_core::{
    HttpProxyConfig, PipelineContext, PipelineRequestOutcome, RequestDraft, ResponseDraft,
    ToolPipeline, canonicalHostHeader,
};
use location_core::ResolvedLocation;
use reqwest::{Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, RwLock, Semaphore, watch},
    task::JoinSet,
    time::{Instant, sleep},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{ApiError, ControlState, ErrorCode, LocalizedApiError};
use crate::localization::RequestLocale;

const maximumReplayHeaders: usize = 256;
const maximumReplayMethodCharacters: usize = 32;
const maximumReplayUrlCharacters: usize = 8_192;
const maximumReplayBodyBytes: usize = 8 * 1024 * 1024;
const maximumReplayBodyCharacters: usize = maximumReplayBodyBytes.div_ceil(3) * 4;
const maximumAdvancedRepeatConcurrency: usize = 256;
const maximumAdvancedRepeatIterations: usize = 10_000;
const maximumAdvancedRepeatIntervalMilliseconds: u64 = 60_000;
const maximumAdvancedRepeatNameCharacters: usize = 128;
const maximumRetainedLoadTests: usize = 64;
const replayClientAddress: &str = "127.0.0.1:0";

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComposeRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<HeaderField>,
    pub bodyBase64: String,
    #[serde(default = "defaultViaProxy")]
    pub viaProxy: bool,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComposeRequestOverrides {
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<Vec<HeaderField>>,
    #[serde(default)]
    pub bodyBase64: Option<String>,
    #[serde(default)]
    pub viaProxy: Option<bool>,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepeatRequest {
    pub transactionId: String,
    #[serde(default)]
    pub overrides: Option<ComposeRequestOverrides>,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposeResult {
    pub transactionId: String,
    pub revision: u64,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdvancedRepeatPlan {
    pub name: String,
    pub base: ComposeRequest,
    pub concurrency: usize,
    pub totalIterations: usize,
    pub intervalMilliseconds: u64,
    pub recordEach: bool,
    pub stopOnError: bool,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdvancedRepeatStartRequest {
    #[serde(flatten)]
    pub plan: AdvancedRepeatPlan,
    pub confirmed: bool,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedRepeatJob {
    pub jobId: String,
    pub state: AdvancedRepeatState,
    pub plan: AdvancedRepeatPlan,
    pub startedAtMilliseconds: u64,
    pub finishedAtMilliseconds: Option<u64>,
    pub completedIterations: usize,
    pub successCount: usize,
    pub failureCount: usize,
    pub latencyMilliseconds: LatencyStatistics,
    pub lastError: Option<String>,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AdvancedRepeatState {
    Queued,
    Running,
    Completed,
    Cancelled,
    Failed,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStatistics {
    pub min: u64,
    pub max: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone)]
struct PreparedReplayRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    originalLocation: ResolvedLocation,
    viaProxy: bool,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone)]
struct ReplayCapture {
    transactionId: String,
    recordingSessionId: String,
}

/// 定义重复请求和高级重复作业使用的协议状态。
#[derive(Clone)]
struct ReplayDependencies {
    recording: RecordingSession,
    pipeline: ToolPipeline,
    httpConfiguration: HttpProxyConfig,
}

/// 定义重复请求和高级重复作业使用的协议状态。
struct ManagedLoadTest {
    snapshot: AdvancedRepeatJob,
    latencySamples: Vec<u64>,
    cancellation: CancellationToken,
}

/// 创建空的高级重复作业注册表；运行时首次使用时才分配共享状态。
#[derive(Clone)]
pub(super) struct RepeatRuntime {
    jobs: Arc<RwLock<BTreeMap<String, Arc<Mutex<ManagedLoadTest>>>>>,
    connectionSlots: Arc<Semaphore>,
    changeSender: watch::Sender<u64>,
}

impl Default for RepeatRuntime {
    /// 创建空的高级重复作业注册表；运行时首次使用时才分配共享状态。
    fn default() -> Self {
        let (changeSender, _) = watch::channel(0);
        Self {
            jobs: Arc::new(RwLock::new(BTreeMap::new())),
            connectionSlots: Arc::new(Semaphore::new(maximumAdvancedRepeatConcurrency)),
            changeSender,
        }
    }
}

/// 定义重复请求和高级重复作业使用的协议状态。
const fn defaultViaProxy() -> bool {
    true
}

impl RepeatRuntime {
    /// 订阅高级重复作业版本；控制层据此推送权威作业集合，前端不再定时查询单项进度。
    pub(super) fn subscribeChanges(&self) -> watch::Receiver<u64> {
        self.changeSender.subscribe()
    }

    /// 发布一次作业状态变化；版本只用于唤醒合并器，公开顺序仍由控制层全局 revision 提供。
    fn publishChange(&self) {
        self.changeSender
            .send_modify(|revision| *revision = revision.saturating_add(1));
    }

    /// 校验并登记高级重复作业，随后在独立任务中执行；失败返回结构化参数错误。
    async fn start(
        &self,
        plan: AdvancedRepeatPlan,
        confirmed: bool,
        dependencies: ReplayDependencies,
    ) -> Result<AdvancedRepeatJob, ApiError> {
        validateAdvancedPlan(&plan, confirmed)?;
        let jobId = Uuid::new_v4().to_string();
        let startedAtMilliseconds = currentTimeMilliseconds();
        let job = Arc::new(Mutex::new(ManagedLoadTest {
            snapshot: AdvancedRepeatJob {
                jobId: jobId.clone(),
                state: AdvancedRepeatState::Queued,
                plan,
                startedAtMilliseconds,
                finishedAtMilliseconds: None,
                completedIterations: 0,
                successCount: 0,
                failureCount: 0,
                latencyMilliseconds: LatencyStatistics::default(),
                lastError: None,
            },
            latencySamples: Vec::new(),
            cancellation: CancellationToken::new(),
        }));
        self.jobs.write().await.insert(jobId, job.clone());
        self.pruneCompleted().await;
        self.publishChange();
        let snapshot = job.lock().await.snapshot.clone();
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime.run(job, dependencies).await;
        });
        Ok(snapshot)
    }

    /// 读取指定高级重复作业的最新快照；未知标识返回稳定的未找到错误。
    async fn get(&self, jobId: &str) -> Result<AdvancedRepeatJob, ApiError> {
        let jobs = self.jobs.read().await;
        let job = jobs
            .get(jobId)
            .ok_or_else(|| ApiError::notFound(ErrorCode::LoadTestNotFound))?;
        Ok(job.lock().await.snapshot.clone())
    }

    /// 按创建顺序返回受保留上限约束的高级重复作业快照。
    pub(super) async fn list(&self) -> Vec<AdvancedRepeatJob> {
        let jobs = self.jobs.read().await;
        let mut snapshots = Vec::with_capacity(jobs.len());
        for job in jobs.values() {
            snapshots.push(job.lock().await.snapshot.clone());
        }
        snapshots.sort_by_key(|job| std::cmp::Reverse(job.startedAtMilliseconds));
        snapshots
    }

    /// 发出协作式取消信号并返回最新快照；不会中断已经开始的网络读写。
    async fn cancel(&self, jobId: &str) -> Result<AdvancedRepeatJob, ApiError> {
        let jobs = self.jobs.read().await;
        let job = jobs
            .get(jobId)
            .ok_or_else(|| ApiError::notFound(ErrorCode::LoadTestNotFound))?;
        let mut managed = job.lock().await;
        if matches!(
            managed.snapshot.state,
            AdvancedRepeatState::Queued | AdvancedRepeatState::Running
        ) {
            managed.cancellation.cancel();
            managed.snapshot.state = AdvancedRepeatState::Cancelled;
            managed.snapshot.finishedAtMilliseconds = Some(currentTimeMilliseconds());
        }
        let snapshot = managed.snapshot.clone();
        drop(managed);
        self.publishChange();
        Ok(snapshot)
    }

    /// 在有界并发和取消信号约束下调度作业迭代，并持续更新统计结果。
    async fn run(&self, job: Arc<Mutex<ManagedLoadTest>>, dependencies: ReplayDependencies) {
        {
            let mut managed = job.lock().await;
            if managed.cancellation.is_cancelled() {
                return;
            }
            managed.snapshot.state = AdvancedRepeatState::Running;
        }
        self.publishChange();
        let plan = job.lock().await.snapshot.plan.clone();
        let cancellation = job.lock().await.cancellation.clone();
        let mut nextIteration = 0_usize;
        let mut active = JoinSet::new();
        while nextIteration < plan.totalIterations || !active.is_empty() {
            if cancellation.is_cancelled() {
                break;
            }
            while active.len() < plan.concurrency && nextIteration < plan.totalIterations {
                if cancellation.is_cancelled() {
                    break;
                }
                if nextIteration > 0 && plan.intervalMilliseconds > 0 {
                    tokio::select! {
                        () = cancellation.cancelled() => break,
                        () = sleep(Duration::from_millis(plan.intervalMilliseconds)) => {}
                    }
                    if cancellation.is_cancelled() {
                        break;
                    }
                }
                let permit = tokio::select! {
                    () = cancellation.cancelled() => break,
                    result = self.connectionSlots.clone().acquire_owned() => match result {
                        Ok(permit) => permit,
                        Err(_) => break,
                    },
                };
                let iterationDependencies = dependencies.clone();
                let iterationPlan = plan.clone();
                let iterationCancellation = cancellation.clone();
                active.spawn(async move {
                    let _permit = permit;
                    runAdvancedIteration(
                        iterationPlan,
                        iterationDependencies,
                        iterationCancellation,
                    )
                    .await
                });
                nextIteration = nextIteration.saturating_add(1);
            }
            let Some(result) = active.join_next().await else {
                break;
            };
            let outcome =
                result.unwrap_or_else(|_| ReplayIterationResult::failed("repeatTaskJoinFailed"));
            updateLoadTestProgress(self, &job, outcome, plan.stopOnError).await;
            if plan.stopOnError && job.lock().await.snapshot.failureCount > 0 {
                cancellation.cancel();
            }
        }
        while let Some(result) = active.join_next().await {
            let outcome =
                result.unwrap_or_else(|_| ReplayIterationResult::failed("repeatTaskJoinFailed"));
            updateLoadTestProgress(self, &job, outcome, false).await;
        }
        let mut managed = job.lock().await;
        if managed.snapshot.state != AdvancedRepeatState::Cancelled {
            managed.snapshot.state = if managed.snapshot.failureCount > 0 && plan.stopOnError {
                AdvancedRepeatState::Failed
            } else {
                AdvancedRepeatState::Completed
            };
            managed.snapshot.finishedAtMilliseconds = Some(currentTimeMilliseconds());
        }
        drop(managed);
        self.publishChange();
    }

    /// 回收超过保留上限的已终态作业，避免历史任务无限增长。
    async fn pruneCompleted(&self) {
        let mut jobs = self.jobs.write().await;
        if jobs.len() <= maximumRetainedLoadTests {
            return;
        }
        let mut completed = Vec::new();
        for (jobId, job) in jobs.iter() {
            let snapshot = job.lock().await.snapshot.clone();
            if snapshot.finishedAtMilliseconds.is_some() {
                completed.push((snapshot.startedAtMilliseconds, jobId.clone()));
            }
        }
        completed.sort();
        let removable = jobs.len().saturating_sub(maximumRetainedLoadTests);
        for (_, jobId) in completed.into_iter().take(removable) {
            jobs.remove(&jobId);
        }
    }
}

/// 构造一次失败迭代的标准化结果，供进度与终态统计复用。
struct ReplayIterationResult {
    succeeded: bool,
    latencyMilliseconds: u64,
    errorCode: Option<String>,
}

impl ReplayIterationResult {
    /// 构造一次失败迭代的标准化结果，供进度与终态统计复用。
    fn failed(errorCode: &str) -> Self {
        Self {
            succeeded: false,
            latencyMilliseconds: 0,
            errorCode: Some(errorCode.to_owned()),
        }
    }
}

/// 执行一次高级重复迭代，记录耗时并将失败映射为稳定错误码。
async fn runAdvancedIteration(
    plan: AdvancedRepeatPlan,
    dependencies: ReplayDependencies,
    cancellation: CancellationToken,
) -> ReplayIterationResult {
    let startedAt = Instant::now();
    let prepared = match prepareReplayRequest(&plan.base) {
        Ok(prepared) => prepared,
        Err(error) => return ReplayIterationResult::failed(error.code.messageKey()),
    };
    let capture = if plan.recordEach {
        match beginReplayCapture(&dependencies.recording, &prepared).await {
            Ok(capture) => Some(capture),
            Err(error) => return ReplayIterationResult::failed(error.code.messageKey()),
        }
    } else {
        None
    };
    let result = executeReplay(prepared, capture, dependencies, cancellation).await;
    match result {
        Ok(()) => ReplayIterationResult {
            succeeded: true,
            latencyMilliseconds: startedAt.elapsed().as_millis() as u64,
            errorCode: None,
        },
        Err(errorCode) => ReplayIterationResult {
            succeeded: false,
            latencyMilliseconds: startedAt.elapsed().as_millis() as u64,
            errorCode: Some(errorCode.to_owned()),
        },
    }
}

/// 在共享作业锁内原子更新完成次数、成功失败计数与延迟统计。
async fn updateLoadTestProgress(
    runtime: &RepeatRuntime,
    job: &Arc<Mutex<ManagedLoadTest>>,
    outcome: ReplayIterationResult,
    _stopOnError: bool,
) {
    let mut managed = job.lock().await;
    managed.snapshot.completedIterations = managed.snapshot.completedIterations.saturating_add(1);
    if outcome.succeeded {
        managed.snapshot.successCount = managed.snapshot.successCount.saturating_add(1);
    } else {
        managed.snapshot.failureCount = managed.snapshot.failureCount.saturating_add(1);
        managed.snapshot.lastError = outcome.errorCode;
    }
    managed.latencySamples.push(outcome.latencyMilliseconds);
    managed.snapshot.latencyMilliseconds = summarizeLatency(&managed.latencySamples);
    drop(managed);
    runtime.publishChange();
}

/// 基于已完成迭代的毫秒样本计算最小、最大和分位数。
fn summarizeLatency(samples: &[u64]) -> LatencyStatistics {
    let Some((&minimum, &maximum)) = samples.iter().min().zip(samples.iter().max()) else {
        return LatencyStatistics::default();
    };
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    LatencyStatistics {
        min: minimum,
        max: maximum,
        p50: percentile(&ordered, 50),
        p95: percentile(&ordered, 95),
        p99: percentile(&ordered, 99),
    }
}

/// 使用 nearest-rank 规则读取已排序样本的分位数。
fn percentile(values: &[u64], percent: usize) -> u64 {
    let index = values
        .len()
        .saturating_mul(percent)
        .div_ceil(100)
        .saturating_sub(1);
    values[index]
}

/// 校验并规范化 Compose 请求，生成可安全发送的 HTTP 请求草稿。
fn prepareReplayRequest(request: &ComposeRequest) -> Result<PreparedReplayRequest, ApiError> {
    if request.method.is_empty() || request.method.len() > maximumReplayMethodCharacters {
        return Err(ApiError::badRequest(ErrorCode::InvalidRepeatRequest));
    }
    let method = request
        .method
        .parse::<Method>()
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidRepeatRequest))?;
    if request.url.len() > maximumReplayUrlCharacters {
        return Err(ApiError::badRequest(ErrorCode::InvalidRepeatRequest));
    }
    let uri = request
        .url
        .parse::<Uri>()
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidRepeatRequest))?;
    let originalLocation = composeLocation(&uri)?;
    let headers = composeHeaders(&request.headers, &originalLocation)?;
    let body = composeBody(&request.bodyBase64)?;
    Ok(PreparedReplayRequest {
        method,
        uri,
        headers,
        body,
        originalLocation,
        viaProxy: request.viaProxy,
    })
}

/// 从绝对 HTTP/HTTPS URI 构造流水线所需的位置匹配信息。
fn composeLocation(uri: &Uri) -> Result<ResolvedLocation, ApiError> {
    let scheme = uri
        .scheme_str()
        .filter(|scheme| matches!(*scheme, "http" | "https"))
        .ok_or_else(|| ApiError::badRequest(ErrorCode::InvalidRepeatRequest))?;
    let authority = uri
        .authority()
        .ok_or_else(|| ApiError::badRequest(ErrorCode::InvalidRepeatRequest))?;
    let host = authority.host();
    if host.is_empty() || !uri.path().starts_with('/') {
        return Err(ApiError::badRequest(ErrorCode::InvalidRepeatRequest));
    }
    let port = authority
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    Ok(ResolvedLocation {
        protocol: scheme.to_owned(),
        host: host.to_owned(),
        port,
        path: uri.path().to_owned(),
        query: uri.query().unwrap_or_default().to_owned(),
        display: uri.to_string(),
    })
}

/// 将协议头字段转换为 HTTP HeaderMap，并拒绝非法名称或值。
fn composeHeaders(
    fields: &[HeaderField],
    location: &ResolvedLocation,
) -> Result<HeaderMap, ApiError> {
    if fields.len() > maximumReplayHeaders {
        return Err(ApiError::badRequest(ErrorCode::InvalidRepeatRequest));
    }
    let mut headers = HeaderMap::new();
    for field in fields {
        let name = field
            .name
            .parse::<HeaderName>()
            .map_err(|_| ApiError::badRequest(ErrorCode::InvalidRepeatRequest))?;
        let value = HeaderValue::from_str(&field.value)
            .map_err(|_| ApiError::badRequest(ErrorCode::InvalidRepeatRequest))?;
        headers.append(name, value);
    }
    let defaultPort = if location.protocol == "https" {
        443
    } else {
        80
    };
    headers.insert(
        HOST,
        canonicalHostHeader(&location.host, location.port, defaultPort)
            .map_err(|_| ApiError::badRequest(ErrorCode::InvalidRepeatRequest))?,
    );
    Ok(headers)
}

/// 解码 Base64 正文并限制大小，解码失败返回请求参数错误。
fn composeBody(encoded: &str) -> Result<Bytes, ApiError> {
    if encoded.len() > maximumReplayBodyCharacters {
        return Err(ApiError::badRequest(ErrorCode::InvalidRepeatRequest));
    }
    let body = base64Standard
        .decode(encoded)
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidRepeatRequest))?;
    if body.len() > maximumReplayBodyBytes {
        return Err(ApiError::badRequest(ErrorCode::InvalidRepeatRequest));
    }
    Ok(Bytes::from(body))
}

/// 按录制开关创建新事务；关闭逐条录制时返回空捕获句柄。
async fn beginReplayCapture(
    recording: &RecordingSession,
    request: &PreparedReplayRequest,
) -> Result<ReplayCapture, ApiError> {
    let recordingSessionId = recording
        .snapshot()
        .await
        .map_err(|_| ApiError::internal(ErrorCode::RecordingOperationFailed))?
        .recordingSessionId;
    let transactionId = recording
        .beginTransaction(BeginTransaction {
            protocol: if request.originalLocation.protocol == "https" {
                TransactionProtocol::Https
            } else {
                TransactionProtocol::Http
            },
            method: request.method.as_str().to_owned(),
            location: request.originalLocation.clone(),
            clientAddress: replayClientAddress.to_owned(),
            clientProcessName: None,
            clientProcessId: None,
            contentType: contentType(&request.headers),
            startAtMilliseconds: currentTimeMilliseconds(),
        })
        .await
        .map_err(|_| ApiError::internal(ErrorCode::RecordingOperationFailed))?
        .ok_or_else(|| ApiError::conflict(ErrorCode::RepeatRecordingUnavailable))?;
    Ok(ReplayCapture {
        transactionId,
        recordingSessionId,
    })
}

/// 执行一次重复请求，并确保失败路径将已创建事务收敛为终态。
async fn executeReplay(
    request: PreparedReplayRequest,
    capture: Option<ReplayCapture>,
    dependencies: ReplayDependencies,
    cancellation: CancellationToken,
) -> Result<(), &'static str> {
    let result =
        executeReplayInner(request, capture.clone(), dependencies.clone(), cancellation).await;
    if let Err(errorCode) = result {
        finishReplayFailure(&dependencies.recording, capture.as_ref(), errorCode).await;
        return Err(errorCode);
    }
    Ok(())
}

/// 运行工具流水线、转发请求并在需要时提交新事务。
async fn executeReplayInner(
    request: PreparedReplayRequest,
    capture: Option<ReplayCapture>,
    dependencies: ReplayDependencies,
    cancellation: CancellationToken,
) -> Result<(), &'static str> {
    let viaProxy = request.viaProxy;
    let mut context = PipelineContext::new(
        replayClientAddress.to_owned(),
        request.originalLocation.clone(),
        RequestDraft {
            method: request.method,
            uri: request.uri,
            version: Version::HTTP_11,
            headers: request.headers,
            body: Some(request.body),
        },
    );
    if let Some(capture) = &capture {
        context.bindTransaction(
            capture.transactionId.clone(),
            capture.recordingSessionId.clone(),
        );
    }
    let outcome = if viaProxy {
        dependencies
            .pipeline
            .runRequest(&mut context)
            .await
            .map_err(|_| "repeatPipelineFailed")?
    } else {
        PipelineRequestOutcome::Forward
    };
    storeReplayRequest(&dependencies.recording, capture.as_ref(), &context).await?;
    match outcome {
        PipelineRequestOutcome::Forward => {
            forwardReplayRequest(
                &mut context,
                capture.as_ref(),
                &dependencies,
                &cancellation,
                viaProxy,
            )
            .await
        }
        PipelineRequestOutcome::Synthetic | PipelineRequestOutcome::Blocked => {
            completeSyntheticReplay(
                &mut context,
                capture.as_ref(),
                &dependencies,
                outcome == PipelineRequestOutcome::Blocked,
            )
            .await
        }
    }
}

/// 把中途失败的捕获事务标记为失败，防止遗留 pending 事务。
async fn finishReplayFailure(
    recording: &RecordingSession,
    capture: Option<&ReplayCapture>,
    errorCode: &str,
) {
    if errorCode == "repeatCancelled" {
        return;
    }
    let Some(capture) = capture else {
        return;
    };
    let _ = recording
        .fail(
            &capture.transactionId,
            TransactionError {
                code: errorCode.to_owned(),
                messageKey: "error.repeatRequestFailed".to_owned(),
                params: BTreeMap::new(),
            },
            currentTimeMilliseconds(),
        )
        .await;
}

/// 把规范化请求头和正文写入录制存储，保留原始字节统计。
async fn storeReplayRequest(
    recording: &RecordingSession,
    capture: Option<&ReplayCapture>,
    context: &PipelineContext,
) -> Result<(), &'static str> {
    let Some(capture) = capture else {
        return Ok(());
    };
    recording
        .update(
            &capture.transactionId,
            TransactionUpdate {
                flags: Some(context.flags.clone()),
                ..TransactionUpdate::default()
            },
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    recording
        .updateUserFields(
            &capture.transactionId,
            TransactionUserUpdate {
                appliedTools: Some(context.appliedTools.clone()),
                ..TransactionUserUpdate::default()
            },
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    recording
        .storeHeaders(
            &capture.transactionId,
            MessageSide::Request,
            headerFields(&context.request.headers),
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    recording
        .updateProgress(
            &capture.transactionId,
            TransactionProgressUpdate {
                requestHeaderBytes: Some(requestHeaderBytes(&context.request)),
                requestSentAtMilliseconds: Some(currentTimeMilliseconds()),
                ..TransactionProgressUpdate::default()
            },
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    recording
        .storeBody(
            &capture.transactionId,
            MessageSide::Request,
            bodyWrite(
                context.request.body.as_deref().unwrap_or_default(),
                contentType(&context.request.headers),
                contentEncoding(&context.request.headers),
            ),
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    Ok(())
}

/// 经已启用工具流水线处理请求，再将允许的草稿发送至上游。
async fn forwardReplayRequest(
    context: &mut PipelineContext,
    capture: Option<&ReplayCapture>,
    dependencies: &ReplayDependencies,
    cancellation: &CancellationToken,
    runPipeline: bool,
) -> Result<(), &'static str> {
    let client = replayClient(&dependencies.httpConfiguration).map_err(|_| "repeatClientFailed")?;
    let request = client
        .request(
            context.request.method.clone(),
            context.request.uri.to_string(),
        )
        .headers(context.request.headers.clone())
        .body(context.request.body.clone().unwrap_or_default());
    let response = tokio::select! {
        () = cancellation.cancelled() => return finishCancelled(&dependencies.recording, capture).await,
        result = request.send() => result.map_err(|_| "repeatRequestFailed")?,
    };
    let status = response.status();
    let headers = response.headers().clone();
    let body = readResponseBody(response, cancellation).await?;
    context.response = Some(ResponseDraft {
        status,
        version: Version::HTTP_11,
        headers,
        body: Some(body),
    });
    if context.requestThrottlePlan.is_some() || context.responseThrottlePlan.is_some() {
        // 该分支保证重复请求失败或取消时不会遗留未完成事务。
        // 该分支保证重复请求失败或取消时不会遗留未完成事务。
    }
    if runPipeline {
        dependencies
            .pipeline
            .runResponse(context)
            .await
            .map_err(|_| "repeatPipelineFailed")?;
    }
    completeReplayResponse(&dependencies.recording, capture, context, false).await
}

/// 将工具生成的本地响应提交为完成事务，无需访问上游。
async fn completeSyntheticReplay(
    context: &mut PipelineContext,
    capture: Option<&ReplayCapture>,
    dependencies: &ReplayDependencies,
    blocked: bool,
) -> Result<(), &'static str> {
    dependencies
        .pipeline
        .runResponse(context)
        .await
        .map_err(|_| "repeatPipelineFailed")?;
    completeReplayResponse(&dependencies.recording, capture, context, blocked).await
}

/// 写入上游响应的头、正文和状态，并提交事务完成事件。
async fn completeReplayResponse(
    recording: &RecordingSession,
    capture: Option<&ReplayCapture>,
    context: &PipelineContext,
    blocked: bool,
) -> Result<(), &'static str> {
    let Some(response) = context.response.as_ref() else {
        return Err("repeatResponseMissing");
    };
    let Some(capture) = capture else {
        return Ok(());
    };
    recording
        .update(
            &capture.transactionId,
            TransactionUpdate {
                flags: Some(context.flags.clone()),
                statusCode: Some(response.status.as_u16()),
                ..TransactionUpdate::default()
            },
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    recording
        .updateUserFields(
            &capture.transactionId,
            TransactionUserUpdate {
                appliedTools: Some(context.appliedTools.clone()),
                ..TransactionUserUpdate::default()
            },
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    recording
        .storeHeaders(
            &capture.transactionId,
            MessageSide::Response,
            headerFields(&response.headers),
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    recording
        .updateProgress(
            &capture.transactionId,
            TransactionProgressUpdate {
                responseHeaderBytes: Some(responseHeaderBytes(response)),
                responseStartAtMilliseconds: Some(currentTimeMilliseconds()),
                ..TransactionProgressUpdate::default()
            },
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    let body = response.body.as_deref().unwrap_or_default();
    let contentType = contentType(&response.headers);
    recording
        .storeBody(
            &capture.transactionId,
            MessageSide::Response,
            bodyWrite(
                body,
                contentType.clone(),
                contentEncoding(&response.headers),
            ),
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    let completion = TransactionCompletion {
        statusCode: response.status.as_u16(),
        endAtMilliseconds: currentTimeMilliseconds(),
        contentType,
    };
    if blocked {
        recording
            .block(&capture.transactionId, completion)
            .await
            .map_err(|_| "repeatRecordingFailed")
    } else {
        recording
            .commit(&capture.transactionId, completion)
            .await
            .map_err(|_| "repeatRecordingFailed")
    }
}

/// 创建不自动跟随重定向的请求客户端，保留原请求语义。
fn replayClient(configuration: &HttpProxyConfig) -> Result<Client, reqwest::Error> {
    Client::builder()
        .redirect(Policy::none())
        .connect_timeout(configuration.connectTimeout())
        .timeout(configuration.requestTimeout())
        .build()
}

/// 在最大正文边界内读取响应流；超过限制时返回可观察的失败。
async fn readResponseBody(
    mut response: reqwest::Response,
    cancellation: &CancellationToken,
) -> Result<Bytes, &'static str> {
    let mut body = Vec::new();
    loop {
        let next = tokio::select! {
            () = cancellation.cancelled() => return Err("repeatCancelled"),
            chunk = response.chunk() => chunk.map_err(|_| "repeatResponseReadFailed")?,
        };
        let Some(chunk) = next else {
            break;
        };
        let remaining = maximumReplayBodyBytes.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    Ok(Bytes::from(body))
}

/// 将已开始捕获的任务收敛为取消终态，并保留已完成计数。
async fn finishCancelled(
    recording: &RecordingSession,
    capture: Option<&ReplayCapture>,
) -> Result<(), &'static str> {
    let Some(capture) = capture else {
        return Err("repeatCancelled");
    };
    recording
        .cancel(
            &capture.transactionId,
            TransactionError {
                code: "repeatCancelled".to_owned(),
                messageKey: "error.repeatCancelled".to_owned(),
                params: BTreeMap::new(),
            },
            currentTimeMilliseconds(),
        )
        .await
        .map_err(|_| "repeatRecordingFailed")?;
    Err("repeatCancelled")
}

/// 根据正文、内容类型和编码生成存储层写入对象。
fn bodyWrite(bytes: &[u8], contentType: String, encoding: String) -> BodyWrite {
    BodyWrite {
        bytes: bytes.to_vec(),
        originalBytes: bytes.len() as u64,
        contentType,
        encoding,
    }
}

/// 将 HeaderMap 稳定转换为录制协议使用的头字段列表。
fn headerFields(headers: &HeaderMap) -> Vec<HeaderField> {
    headers
        .iter()
        .map(|(name, value)| HeaderField {
            name: name.as_str().to_owned(),
            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
        })
        .collect()
}

/// 提取响应内容类型；缺失时使用空字符串避免伪造默认类型。
fn contentType(headers: &HeaderMap) -> String {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// 提取响应内容编码；缺失时按 identity 写入录制元数据。
fn contentEncoding(headers: &HeaderMap) -> String {
    headers
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// 计算请求行和请求头的近似字节数，供事务统计使用。
fn requestHeaderBytes(request: &RequestDraft) -> u64 {
    request.method.as_str().len() as u64
        + request.uri.to_string().len() as u64
        + 12
        + headerBytes(&request.headers)
}

/// 计算响应行和响应头的近似字节数，供事务统计使用。
fn responseHeaderBytes(response: &ResponseDraft) -> u64 {
    15 + response.status.canonical_reason().map_or(0, str::len) as u64
        + headerBytes(&response.headers)
}

/// 计算头字段序列化后的字节数，统一请求和响应统计口径。
fn headerBytes(headers: &HeaderMap) -> u64 {
    headers
        .iter()
        .map(|(name, value)| name.as_str().len() as u64 + value.as_bytes().len() as u64 + 4)
        .sum()
}

/// 以显式覆盖字段派生新请求；原始事务内容始终保持只读。
fn mergeRepeatRequest(
    transactionMethod: String,
    transactionUrl: String,
    transactionHeaders: Vec<HeaderField>,
    transactionBodyBase64: String,
    overrides: Option<ComposeRequestOverrides>,
) -> ComposeRequest {
    let overrides = overrides.unwrap_or_default();
    ComposeRequest {
        method: overrides.method.unwrap_or(transactionMethod),
        url: overrides.url.unwrap_or(transactionUrl),
        headers: overrides.headers.unwrap_or(transactionHeaders),
        bodyBase64: overrides.bodyBase64.unwrap_or(transactionBodyBase64),
        viaProxy: overrides.viaProxy.unwrap_or(true),
    }
}

/// 验证并发、次数、间隔和确认标记的硬边界，拒绝无确认任务。
fn validateAdvancedPlan(plan: &AdvancedRepeatPlan, confirmed: bool) -> Result<(), ApiError> {
    if !confirmed {
        return Err(ApiError::conflict(
            ErrorCode::AdvancedRepeatConfirmationRequired,
        ));
    }
    if plan.name.trim().is_empty()
        || plan.name.len() > maximumAdvancedRepeatNameCharacters
        || !(1..=maximumAdvancedRepeatConcurrency).contains(&plan.concurrency)
        || !(1..=maximumAdvancedRepeatIterations).contains(&plan.totalIterations)
        || plan.intervalMilliseconds > maximumAdvancedRepeatIntervalMilliseconds
    {
        return Err(ApiError::badRequest(ErrorCode::InvalidAdvancedRepeatPlan));
    }
    prepareReplayRequest(&plan.base)?;
    Ok(())
}

impl ControlState {
    /// 从可编辑请求草稿发送一次 HTTP 请求，并返回新事务标识。
    async fn composeRequest(&self, request: ComposeRequest) -> Result<ComposeResult, ApiError> {
        let prepared = prepareReplayRequest(&request)?;
        let capture = beginReplayCapture(&self.recording, &prepared).await?;
        let transactionId = capture.transactionId.clone();
        let dependencies = self.replayDependencies().await;
        tokio::spawn(async move {
            let _ = executeReplay(
                prepared,
                Some(capture),
                dependencies,
                CancellationToken::new(),
            )
            .await;
        });
        Ok(ComposeResult {
            transactionId,
            revision: self.currentRevision(),
        })
    }

    /// 从已录制事务派生请求；不支持的协议或缺失正文返回结构化错误。
    async fn repeatTransaction(&self, request: RepeatRequest) -> Result<ComposeResult, ApiError> {
        let detail = self
            .recording
            .getTransactionDetail(&request.transactionId)
            .await
            .map_err(super::mapCaptureLookupError)?;
        if !matches!(
            detail.transaction.protocol,
            TransactionProtocol::Http | TransactionProtocol::Https
        ) {
            return Err(ApiError::badRequest(
                ErrorCode::UnsupportedRepeatTransaction,
            ));
        }
        let body = self
            .recording
            .getBody(&request.transactionId, MessageSide::Request)
            .await
            .map_err(|error| match error {
                capture_core::CaptureError::BodyNotFound => {
                    ApiError::conflict(ErrorCode::RepeatBodyUnavailable)
                }
                error => super::mapCaptureLookupError(error),
            })?;
        let compose = mergeRepeatRequest(
            detail.transaction.method,
            detail.transaction.urlDisplay,
            detail.requestHeaders,
            base64Standard.encode(body.bytes),
            request.overrides,
        );
        self.composeRequest(compose).await
    }

    /// 确认后启动有界高级重复作业，返回可轮询的作业快照。
    async fn startAdvancedRepeat(
        &self,
        request: AdvancedRepeatStartRequest,
    ) -> Result<AdvancedRepeatJob, ApiError> {
        self.repeatRuntime
            .start(
                request.plan,
                request.confirmed,
                self.replayDependencies().await,
            )
            .await
    }

    /// 读取单个高级重复作业的权威进度快照。
    async fn advancedRepeatJob(&self, jobId: &str) -> Result<AdvancedRepeatJob, ApiError> {
        self.repeatRuntime.get(jobId).await
    }

    /// 列出当前仍保留的高级重复作业。
    async fn advancedRepeatJobs(&self) -> Vec<AdvancedRepeatJob> {
        self.repeatRuntime.list().await
    }

    /// 取消指定高级重复作业，取消后不再调度新迭代。
    async fn cancelAdvancedRepeat(&self, jobId: &str) -> Result<AdvancedRepeatJob, ApiError> {
        self.repeatRuntime.cancel(jobId).await
    }

    /// 从当前控制状态构造重复执行所需的录制、工具和配置依赖。
    async fn replayDependencies(&self) -> ReplayDependencies {
        let configuration = self.httpConfiguration.read().await.configuration.clone();
        ReplayDependencies {
            recording: self.recording.clone(),
            pipeline: self.toolPipeline(),
            httpConfiguration: configuration,
        }
    }
}

/// 定义重复请求和高级重复作业使用的协议状态。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router
        .route("/api/v1/compose", post(compose))
        .route("/api/v1/transactions/{transactionId}/repeat", post(repeat))
        .route("/api/v1/loadTests", get(listLoadTests).post(startLoadTest))
        .route("/api/v1/loadTests/{jobId}", get(getLoadTest))
        .route("/api/v1/loadTests/{jobId}/cancel", post(cancelLoadTest))
}

/// 处理 Compose 控制 API 请求，拒绝未知字段和无效请求体。
async fn compose(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    requestResult: Result<Json<ComposeRequest>, JsonRejection>,
) -> Result<Json<ComposeResult>, LocalizedApiError> {
    let Json(request) = requestResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidRepeatRequest).withLocale(locale))?;
    state
        .composeRequest(request)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 处理原样或带覆盖字段的事务重复控制 API 请求。
async fn repeat(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(transactionId): Path<String>,
    requestResult: Result<Json<RepeatRequest>, JsonRejection>,
) -> Result<Json<ComposeResult>, LocalizedApiError> {
    let Json(request) = requestResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidRepeatRequest).withLocale(locale))?;
    if request.transactionId != transactionId {
        return Err(ApiError::badRequest(ErrorCode::InvalidRepeatRequest).withLocale(locale));
    }
    state
        .repeatTransaction(request)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 处理高级重复启动请求；只有 confirmed 为真才会占用执行资源。
async fn startLoadTest(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    requestResult: Result<Json<AdvancedRepeatStartRequest>, JsonRejection>,
) -> Result<Json<AdvancedRepeatJob>, LocalizedApiError> {
    let Json(request) = requestResult.map_err(|_| {
        ApiError::badRequest(ErrorCode::InvalidAdvancedRepeatPlan).withLocale(locale)
    })?;
    state
        .startAdvancedRepeat(request)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 返回所有保留中的高级重复作业快照。
async fn listLoadTests(State(state): State<ControlState>) -> Json<Vec<AdvancedRepeatJob>> {
    Json(state.advancedRepeatJobs().await)
}

/// 返回指定高级重复作业，未知标识映射为 404。
async fn getLoadTest(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(jobId): Path<String>,
) -> Result<Json<AdvancedRepeatJob>, LocalizedApiError> {
    state
        .advancedRepeatJob(&jobId)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 取消指定高级重复作业，并返回取消后的状态快照。
async fn cancelLoadTest(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(jobId): Path<String>,
) -> Result<Json<AdvancedRepeatJob>, LocalizedApiError> {
    state
        .cancelAdvancedRepeat(&jobId)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}
