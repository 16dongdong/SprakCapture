use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use capture_core::currentTimeMilliseconds;
use http::{
    HeaderValue, StatusCode,
    header::{CONTENT_LENGTH, CONTENT_TYPE},
};
use location_core::{LocationPattern, validateLocationPattern};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    sync::{oneshot, watch},
    time::timeout,
};

use crate::{
    PipelineContext, PipelineDirective, PipelineError, PipelineTool, ResponseDraft,
    SyntheticResponse, ToolId, ToolPhase, ToolRegistration,
};

pub use super::messageDraft::{EditableHttpMessage, MessageDraftError};
use super::{
    locationScope::matchesLocations,
    messageDraft::{
        applyRequestDraft, applyResponseDraft, editableRequest, editableResponse,
        maximumEditableBodyBytes, validateRequestDraft, validateResponseDraft,
    },
};

const defaultSuspendTimeoutSeconds: u32 = 120;
const defaultMaximumSuspended: u16 = 32;
const maximumBreakpointRules: usize = 2_000;
const maximumBreakpointIdentifierLength: usize = 128;

/// 描述断点命中时暂停的消息阶段；字符串值与控制 API 的 request/response 协议保持一致。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BreakpointPhase {
    Request,
    Response,
}

/// 描述等待人工处理超时后的确定性动作，避免任何连接因前端离线而永久占用代理资源。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BreakpointTimeoutAction {
    #[default]
    Continue,
    Abort,
}

/// 描述按 Location 与消息阶段匹配的一条断点规则；同一规则可同时暂停请求和响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BreakpointRule {
    pub id: String,
    pub enabled: bool,
    pub location: LocationPattern,
    pub onRequest: bool,
    pub onResponse: bool,
}

/// 描述断点调度器的完整热更新配置；队列上限和超时均在配置写入时完成边界校验。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct BreakpointsConfiguration {
    pub enabled: bool,
    pub rules: Vec<BreakpointRule>,
    pub suspendTimeoutSeconds: u32,
    pub maxSuspended: u16,
    pub onTimeout: BreakpointTimeoutAction,
}

impl Default for BreakpointsConfiguration {
    /// 采用关闭状态、120 秒超时与 32 个槽位作为默认值，确保新工具实例不会意外暂停任何流量。
    fn default() -> Self {
        Self {
            enabled: false,
            rules: Vec::new(),
            suspendTimeoutSeconds: defaultSuspendTimeoutSeconds,
            maxSuspended: defaultMaximumSuspended,
            onTimeout: BreakpointTimeoutAction::Continue,
        }
    }
}

/// 描述暴露给控制 API 的暂停快照；草稿副本独立于流水线可变上下文，继续时才通过显式 API 回写。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuspendedBreakpoint {
    pub breakpointId: String,
    pub transactionId: String,
    pub phase: BreakpointPhase,
    pub suspendedAtMilliseconds: u64,
    pub expiresAtMilliseconds: u64,
    pub draft: EditableHttpMessage,
}

/// 描述断点配置、继续操作和队列调度的稳定失败原因；错误不携带 URL、头部和正文等报文内容。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BreakpointError {
    #[error("error.breakpoints.tooManyRules")]
    TooManyRules,
    #[error("error.breakpoints.invalidRuleId")]
    InvalidRuleId,
    #[error("error.breakpoints.duplicateRuleId")]
    DuplicateRuleId,
    #[error("error.breakpoints.invalidLocation")]
    InvalidLocation,
    #[error("error.breakpoints.invalidSuspendTimeout")]
    InvalidSuspendTimeout,
    #[error("error.breakpoints.invalidMaximumSuspended")]
    InvalidMaximumSuspended,
    #[error("error.breakpoints.notFound")]
    NotFound,
    #[error("error.breakpoints.resolutionClosed")]
    ResolutionClosed,
    #[error("{0}")]
    InvalidDraft(#[from] MessageDraftError),
}

impl BreakpointError {
    /// 返回跨控制 API、MCP 和流水线共享的机器码，草稿校验错误沿用消息草稿的精确错误码。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooManyRules => "breakpointTooManyRules",
            Self::InvalidRuleId => "breakpointInvalidRuleId",
            Self::DuplicateRuleId => "breakpointDuplicateRuleId",
            Self::InvalidLocation => "breakpointInvalidLocation",
            Self::InvalidSuspendTimeout => "breakpointInvalidSuspendTimeout",
            Self::InvalidMaximumSuspended => "breakpointInvalidMaximumSuspended",
            Self::NotFound => "breakpointNotFound",
            Self::ResolutionClosed => "breakpointResolutionClosed",
            Self::InvalidDraft(error) => error.code(),
        }
    }

    /// 返回语言包使用的稳定消息键；工具层不拼接包含用户请求细节的可见错误文本。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::TooManyRules => "error.breakpoints.tooManyRules",
            Self::InvalidRuleId => "error.breakpoints.invalidRuleId",
            Self::DuplicateRuleId => "error.breakpoints.duplicateRuleId",
            Self::InvalidLocation => "error.breakpoints.invalidLocation",
            Self::InvalidSuspendTimeout => "error.breakpoints.invalidSuspendTimeout",
            Self::InvalidMaximumSuspended => "error.breakpoints.invalidMaximumSuspended",
            Self::NotFound => "error.breakpoints.notFound",
            Self::ResolutionClosed => "error.breakpoints.resolutionClosed",
            Self::InvalidDraft(error) => error.messageKey(),
        }
    }
}

/// 保存暂停请求所属的单次分辨通道；发送端只保留在队列中，移除时即由 continue、abort 或超时取得所有权。
struct PendingBreakpoint {
    snapshot: SuspendedBreakpoint,
    sender: oneshot::Sender<BreakpointResolution>,
    ownership: Arc<()>,
}

/// 描述流水线等待结束后的内部结果；继续携带经过控制边界验证的完整草稿。
enum BreakpointResolution {
    Continue(EditableHttpMessage),
    Abort,
}

/// 绑定暂停队列项与实际等待任务的生命周期；异步任务被客户端取消时由析构路径立即清除过期项。
struct SuspendedQueueLease {
    transactionId: String,
    ownership: Arc<()>,
    suspended: Arc<Mutex<BTreeMap<String, PendingBreakpoint>>>,
    suspendedChanges: watch::Sender<u64>,
}

impl Drop for SuspendedQueueLease {
    /// 仅移除当前等待任务拥有的队列代次；相同事务随后进入响应阶段时不会被旧租约误删。
    fn drop(&mut self) {
        let mut suspended = self.suspended.lock();
        let ownsCurrent = suspended
            .get(&self.transactionId)
            .is_some_and(|pending| Arc::ptr_eq(&pending.ownership, &self.ownership));
        if !ownsCurrent {
            return;
        }
        suspended.remove(&self.transactionId);
        drop(suspended);
        self.suspendedChanges
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

/// 汇总一次成功挂起所需的公开快照、配置、分辨通道与生命周期租约，避免调用边界暴露难以维护的嵌套元组类型。
type PendingSuspension = (
    SuspendedBreakpoint,
    BreakpointsConfiguration,
    oneshot::Receiver<BreakpointResolution>,
    SuspendedQueueLease,
);

/// 提供请求和响应断点的热更新、暂停队列、继续、中止和生命周期释放能力。
#[derive(Clone)]
pub struct BreakpointsTool {
    configuration: Arc<RwLock<BreakpointsConfiguration>>,
    suspended: Arc<Mutex<BTreeMap<String, PendingBreakpoint>>>,
    suspendedChanges: watch::Sender<u64>,
}

impl BreakpointsTool {
    /// 使用已校验配置创建断点工具；规则无效时拒绝创建，避免运行期报文路径解释半有效配置。
    pub fn new(configuration: BreakpointsConfiguration) -> Result<Self, BreakpointError> {
        validateConfiguration(&configuration)?;
        let (suspendedChanges, _) = watch::channel(0_u64);
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
            suspended: Arc::new(Mutex::new(BTreeMap::new())),
            suspendedChanges,
        })
    }

    /// 返回当前可序列化的配置快照，供控制 API 读取而不暴露内部锁和暂停通道。
    pub fn configuration(&self) -> BreakpointsConfiguration {
        self.configuration.read().clone()
    }

    /// 先校验完整新配置再原子替换；总开关关闭时同步中止既有暂停，确保不会留下无法再处理的连接。
    pub fn replaceConfiguration(
        &self,
        configuration: BreakpointsConfiguration,
    ) -> Result<(), BreakpointError> {
        validateConfiguration(&configuration)?;
        let releasePending = !configuration.enabled;
        *self.configuration.write() = configuration;
        if releasePending {
            self.releaseAll();
        }
        Ok(())
    }

    /// 校验配置但不改变运行状态，供控制层的显式验证请求复用相同边界与错误码。
    pub fn validate(configuration: &BreakpointsConfiguration) -> Result<(), BreakpointError> {
        validateConfiguration(configuration)
    }

    /// 返回按 transactionId 稳定排序的暂停快照；副本不包含内部发送端，因此可安全传给控制 API。
    pub fn suspendedBreakpoints(&self) -> Vec<SuspendedBreakpoint> {
        self.suspended
            .lock()
            .values()
            .map(|pending| pending.snapshot.clone())
            .collect()
    }

    /// 返回当前暂停队列版本；控制层可将它与快照一起缓存，避免无变化时重复序列化完整草稿列表。
    pub fn suspendedRevision(&self) -> u64 {
        *self.suspendedChanges.borrow()
    }

    /// 订阅暂停队列版本变更；调用方收到变更后读取 suspendedBreakpoints，即可向前端推送完整一致快照。
    pub fn subscribeSuspendedChanges(&self) -> watch::Receiver<u64> {
        self.suspendedChanges.subscribe()
    }

    /// 校验并提交继续草稿；草稿无效时保留原暂停项，调用方可以修正后重试而不会丢失连接。
    pub fn continueBreakpoint(
        &self,
        transactionId: &str,
        draft: EditableHttpMessage,
    ) -> Result<(), BreakpointError> {
        let phase = self
            .suspended
            .lock()
            .get(transactionId)
            .map(|pending| pending.snapshot.phase)
            .ok_or(BreakpointError::NotFound)?;
        match phase {
            BreakpointPhase::Request => validateRequestDraft(&draft)?,
            BreakpointPhase::Response => validateResponseDraft(&draft)?,
        }
        let pending = self
            .suspended
            .lock()
            .remove(transactionId)
            .ok_or(BreakpointError::NotFound)?;
        let result = pending
            .sender
            .send(BreakpointResolution::Continue(draft))
            .map_err(|_| BreakpointError::ResolutionClosed);
        self.notifySuspendedChanges();
        result
    }

    /// 中止指定暂停项；成功后流水线会为请求生成 502，或将响应替换为 502 草稿后返回客户端。
    pub fn abortBreakpoint(&self, transactionId: &str) -> Result<(), BreakpointError> {
        let pending = self
            .suspended
            .lock()
            .remove(transactionId)
            .ok_or(BreakpointError::NotFound)?;
        let result = pending
            .sender
            .send(BreakpointResolution::Abort)
            .map_err(|_| BreakpointError::ResolutionClosed);
        self.notifySuspendedChanges();
        result
    }

    /// 释放全部暂停项并中止关联流水线，供服务停止和工具关闭路径调用；返回实际释放的槽位数量。
    pub fn releaseAll(&self) -> usize {
        let pending = std::mem::take(&mut *self.suspended.lock());
        let count = pending.len();
        for (_, pending) in pending {
            let _ = pending.sender.send(BreakpointResolution::Abort);
        }
        if count > 0 {
            self.notifySuspendedChanges();
        }
        count
    }

    /// 查找当前阶段命中的首条启用规则；规则顺序即优先级，避免多规则同时挂起同一事务。
    fn matchingRule(
        &self,
        phase: BreakpointPhase,
        location: &location_core::ResolvedLocation,
    ) -> Result<Option<(BreakpointRule, BreakpointsConfiguration)>, BreakpointError> {
        let configuration = self.configuration();
        if !configuration.enabled {
            return Ok(None);
        }
        for rule in &configuration.rules {
            let matchesPhase = match phase {
                BreakpointPhase::Request => rule.onRequest,
                BreakpointPhase::Response => rule.onResponse,
            };
            if !rule.enabled || !matchesPhase {
                continue;
            }
            let matched = matchesLocations(std::slice::from_ref(&rule.location), location)
                .map_err(|_| BreakpointError::InvalidLocation)?;
            if matched {
                return Ok(Some((rule.clone(), configuration)));
            }
        }
        Ok(None)
    }

    /// 将命中规则注册到队列并返回独占等待通道；未绑定录制事务、正文超限、重复 ID 或队列满均按继续处理。
    fn suspend(
        &self,
        context: &PipelineContext,
        phase: BreakpointPhase,
        draft: EditableHttpMessage,
        bodyLength: Option<usize>,
    ) -> Result<Option<PendingSuspension>, BreakpointError> {
        let Some(transactionId) = context.transactionId.as_deref() else {
            return Ok(None);
        };
        if bodyLength.is_some_and(|length| length > maximumEditableBodyBytes) {
            return Ok(None);
        }
        let Some((rule, configuration)) = self.matchingRule(phase, &context.location)? else {
            return Ok(None);
        };
        let mut pending = self.suspended.lock();
        if pending.len() >= usize::from(configuration.maxSuspended)
            || pending.contains_key(transactionId)
        {
            return Ok(None);
        }
        let suspendedAtMilliseconds = currentTimeMilliseconds();
        let expiresAtMilliseconds = suspendedAtMilliseconds
            .saturating_add(u64::from(configuration.suspendTimeoutSeconds).saturating_mul(1_000));
        let snapshot = SuspendedBreakpoint {
            breakpointId: rule.id,
            transactionId: transactionId.to_owned(),
            phase,
            suspendedAtMilliseconds,
            expiresAtMilliseconds,
            draft,
        };
        let (sender, receiver) = oneshot::channel();
        let ownership = Arc::new(());
        pending.insert(
            transactionId.to_owned(),
            PendingBreakpoint {
                snapshot: snapshot.clone(),
                sender,
                ownership: ownership.clone(),
            },
        );
        drop(pending);
        self.notifySuspendedChanges();
        let lease = SuspendedQueueLease {
            transactionId: transactionId.to_owned(),
            ownership,
            suspended: self.suspended.clone(),
            suspendedChanges: self.suspendedChanges.clone(),
        };
        Ok(Some((snapshot, configuration, receiver, lease)))
    }

    /// 等待人工继续、中止或超时；超时先夺取队列所有权，避免与手动操作产生双重分辨。
    async fn waitForResolution(
        &self,
        snapshot: &SuspendedBreakpoint,
        configuration: BreakpointsConfiguration,
        receiver: oneshot::Receiver<BreakpointResolution>,
        _lease: SuspendedQueueLease,
    ) -> BreakpointResolution {
        let mut receiver = receiver;
        match timeout(
            Duration::from_secs(u64::from(configuration.suspendTimeoutSeconds)),
            &mut receiver,
        )
        .await
        {
            Ok(Ok(resolution)) => resolution,
            Ok(Err(_)) => BreakpointResolution::Abort,
            Err(_) => {
                let timeoutOwner = self.suspended.lock().remove(&snapshot.transactionId);
                if timeoutOwner.is_some() {
                    self.notifySuspendedChanges();
                    match configuration.onTimeout {
                        BreakpointTimeoutAction::Continue => {
                            BreakpointResolution::Continue(snapshot.draft.clone())
                        }
                        BreakpointTimeoutAction::Abort => BreakpointResolution::Abort,
                    }
                } else {
                    receiver.await.unwrap_or(BreakpointResolution::Abort)
                }
            }
        }
    }

    /// 将内部断点错误转换为固定工具槽位错误，使数据面不暴露控制规则或消息草稿的内容。
    fn pipelineError(error: BreakpointError) -> PipelineError {
        PipelineError::ToolFailed {
            toolId: ToolId::Breakpoints,
            code: error.code().to_owned(),
        }
    }

    /// 递增 watch 版本并唤醒订阅者；该操作不复制暂停草稿，也不持有队列锁。
    fn notifySuspendedChanges(&self) {
        self.suspendedChanges
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

#[async_trait]
impl PipelineTool for BreakpointsTool {
    /// 返回总开关、双阶段参与和正文需求；任一启用的阶段规则都请求完整正文，以允许控制面编辑原始草稿。
    fn registration(&self) -> ToolRegistration {
        let configuration = self.configuration();
        let mut registration = ToolRegistration::new(
            ToolId::Breakpoints,
            vec![ToolPhase::Request, ToolPhase::Response],
            configuration.enabled,
        );
        if configuration.enabled
            && configuration
                .rules
                .iter()
                .any(|rule| rule.enabled && rule.onRequest)
        {
            registration = registration.withRequestBody();
        }
        if configuration.enabled
            && configuration
                .rules
                .iter()
                .any(|rule| rule.enabled && rule.onResponse)
        {
            registration = registration.withResponseBody();
        }
        registration
    }

    /// 命中请求断点后暂停并等待控制面分辨；继续时回写方法、URL、头部和正文，中止时生成标准 502 合成响应。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let bodyLength = context.request.body.as_ref().map(Bytes::len);
        if bodyLength.is_some_and(|length| length > maximumEditableBodyBytes) {
            return Ok(PipelineDirective::Continue);
        }
        let draft = editableRequest(context);
        let Some((snapshot, configuration, receiver, lease)) = self
            .suspend(context, BreakpointPhase::Request, draft, bodyLength)
            .map_err(Self::pipelineError)?
        else {
            return Ok(PipelineDirective::Continue);
        };
        context.suspended = true;
        context.flags.breakpointHit = true;
        let resolution = self
            .waitForResolution(&snapshot, configuration, receiver, lease)
            .await;
        context.suspended = false;
        match resolution {
            BreakpointResolution::Continue(draft) => {
                applyRequestDraft(context, draft)
                    .map_err(|error| Self::pipelineError(error.into()))?;
                Ok(PipelineDirective::Applied)
            }
            BreakpointResolution::Abort => Ok(PipelineDirective::ShortCircuit(abortedResponse())),
        }
    }

    /// 命中响应断点后暂停并等待控制面分辨；中止替换为 502 响应草稿而非响应阶段短路，保持流水线契约。
    async fn onResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let Some(response) = context.response.as_ref() else {
            return Ok(PipelineDirective::Continue);
        };
        let bodyLength = response.body.as_ref().map(Bytes::len);
        if bodyLength.is_some_and(|length| length > maximumEditableBodyBytes) {
            return Ok(PipelineDirective::Continue);
        }
        let draft = editableResponse(response);
        let Some((snapshot, configuration, receiver, lease)) = self
            .suspend(context, BreakpointPhase::Response, draft, bodyLength)
            .map_err(Self::pipelineError)?
        else {
            return Ok(PipelineDirective::Continue);
        };
        context.suspended = true;
        context.flags.breakpointHit = true;
        let resolution = self
            .waitForResolution(&snapshot, configuration, receiver, lease)
            .await;
        context.suspended = false;
        match resolution {
            BreakpointResolution::Continue(draft) => {
                let response = context
                    .response
                    .as_mut()
                    .expect("响应断点等待期间响应草稿由当前流水线上下文独占");
                applyResponseDraft(response, draft)
                    .map_err(|error| Self::pipelineError(error.into()))?;
                Ok(PipelineDirective::Applied)
            }
            BreakpointResolution::Abort => {
                context.response = Some(ResponseDraft::fromSynthetic(abortedResponse()));
                Ok(PipelineDirective::Applied)
            }
        }
    }
}

/// 校验断点规则数量、ID、Location 与资源边界；热更新在调用此函数成功后才替换当前配置。
fn validateConfiguration(configuration: &BreakpointsConfiguration) -> Result<(), BreakpointError> {
    if configuration.rules.len() > maximumBreakpointRules {
        return Err(BreakpointError::TooManyRules);
    }
    if !(1..=3_600).contains(&configuration.suspendTimeoutSeconds) {
        return Err(BreakpointError::InvalidSuspendTimeout);
    }
    if !(1..=1_024).contains(&configuration.maxSuspended) {
        return Err(BreakpointError::InvalidMaximumSuspended);
    }
    let mut identifiers = HashSet::new();
    for rule in &configuration.rules {
        if rule.id.is_empty() || rule.id.len() > maximumBreakpointIdentifierLength {
            return Err(BreakpointError::InvalidRuleId);
        }
        if !identifiers.insert(rule.id.clone()) {
            return Err(BreakpointError::DuplicateRuleId);
        }
        validateLocationPattern(&rule.location).map_err(|_| BreakpointError::InvalidLocation)?;
    }
    Ok(())
}

/// 构造中止断点使用的空 502 合成响应，明确内容长度以让 HTTP/1.1 客户端可靠结束读取。
fn abortedResponse() -> SyntheticResponse {
    let body = Bytes::new();
    let mut response = SyntheticResponse::new(StatusCode::BAD_GATEWAY, body.clone());
    response.headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response.headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&body.len().to_string())
            .expect("空响应正文长度必须能表示为 HTTP Content-Length"),
    );
    response
}
