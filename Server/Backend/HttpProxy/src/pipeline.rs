use std::{collections::BTreeMap, fmt, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use capture_core::TransactionFlags;
use http::{HeaderMap, Method, StatusCode, Uri, Version};
use location_core::ResolvedLocation;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::tools::ThrottlePlan;

/// 标识工具可参与的固定流水线阶段；同一工具可同时挂接请求和响应阶段。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolPhase {
    Request,
    Response,
}

/// 标识内置工具在流水线中的稳定位置；枚举顺序不能作为执行顺序，必须使用阶段常量。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolId {
    BlockList,
    NoCaching,
    BlockCookies,
    MapRemote,
    MapLocal,
    Rewrite,
    Breakpoints,
    Throttling,
    Mirror,
    AutoSave,
}

impl ToolId {
    /// 返回写入事务 `appliedTools` 和控制面 JSON 的稳定 camelCase 名称。
    pub const fn asStr(self) -> &'static str {
        match self {
            Self::BlockList => "blockList",
            Self::NoCaching => "noCaching",
            Self::BlockCookies => "blockCookies",
            Self::MapRemote => "mapRemote",
            Self::MapLocal => "mapLocal",
            Self::Rewrite => "rewrite",
            Self::Breakpoints => "breakpoints",
            Self::Throttling => "throttling",
            Self::Mirror => "mirror",
            Self::AutoSave => "autoSave",
        }
    }
}

impl fmt::Display for ToolId {
    /// 使用稳定工具名称格式化错误上下文，避免调试枚举名进入控制面或日志协议。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.asStr())
    }
}

/// 描述一个工具的热更新运行快照；`enabled` 的权威值由工具自身配置锁在每次调用时给出。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolRegistration {
    pub id: ToolId,
    pub phases: Vec<ToolPhase>,
    pub enabled: bool,
    pub requiresRequestBody: bool,
    pub requiresResponseBody: bool,
}

impl ToolRegistration {
    /// 创建工具注册快照；正文访问仅在工具需要改写正文时开启，空流水线保持 M1 的流式转发边界。
    pub fn new(id: ToolId, phases: Vec<ToolPhase>, enabled: bool) -> Self {
        Self {
            id,
            phases,
            enabled,
            requiresRequestBody: false,
            requiresResponseBody: false,
        }
    }

    /// 标记工具需要完整请求正文；代理接入层据此在调用请求钩子前准备可变正文草稿。
    pub fn withRequestBody(mut self) -> Self {
        self.requiresRequestBody = true;
        self
    }

    /// 标记工具需要完整响应正文；代理接入层据此在调用响应钩子前准备可变正文草稿。
    pub fn withResponseBody(mut self) -> Self {
        self.requiresResponseBody = true;
        self
    }

    /// 判断当前注册快照是否参与指定阶段，避免工具在错误方向被错误调用。
    pub fn participatesIn(&self, phase: ToolPhase) -> bool {
        self.phases.contains(&phase)
    }
}

/// 表示代理可修改的 HTTP 请求草稿；`body=None` 表示当前请求仍使用流式正文，工具必须声明正文需求才会得到完整字节。
#[derive(Clone, Debug)]
pub struct RequestDraft {
    pub method: Method,
    pub uri: Uri,
    pub version: Version,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
}

impl RequestDraft {
    /// 从已拆分请求头复制可编辑草稿；正文仍由调用方按工具声明选择流式转发或有界物化。
    pub(crate) fn fromParts(request: &http::request::Parts) -> Self {
        Self {
            method: request.method.clone(),
            uri: request.uri.clone(),
            version: request.version,
            headers: request.headers.clone(),
            body: None,
        }
    }
}

/// 表示代理可修改的 HTTP 响应草稿；响应工具可改写状态、头和已物化的正文。
#[derive(Clone, Debug)]
pub struct ResponseDraft {
    pub status: StatusCode,
    pub version: Version,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
}

impl ResponseDraft {
    /// 由合成响应转换为标准响应草稿，使短路与真实上游响应共享同一组响应钩子。
    pub fn fromSynthetic(response: SyntheticResponse) -> Self {
        Self {
            status: response.status,
            version: response.version,
            headers: response.headers,
            body: Some(response.body),
        }
    }
}

/// 描述不经出站网络即可返回给客户端的响应；Map Local 和屏蔽列表使用此模型完成短路。
#[derive(Clone, Debug)]
pub struct SyntheticResponse {
    pub status: StatusCode,
    pub version: Version,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl SyntheticResponse {
    /// 创建默认 HTTP/1.1 合成响应；调用方可继续填充头字段和正文以保持工具职责单一。
    pub fn new(status: StatusCode, body: Bytes) -> Self {
        Self {
            status,
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body,
        }
    }
}

/// 汇总单个事务跨请求和响应阶段共享的可变状态；`originalLocation` 仅保留客户端原始目标供规则匹配与工具痕迹比对，录制摘要使用工具执行后的 `location`。
#[derive(Clone, Debug)]
pub struct PipelineContext {
    pub transactionId: Option<String>,
    pub recordingSessionId: Option<String>,
    pub clientAddress: String,
    pub clientProcessName: Option<String>,
    pub clientProcessId: Option<u32>,
    pub originalLocation: ResolvedLocation,
    pub location: ResolvedLocation,
    pub request: RequestDraft,
    pub response: Option<ResponseDraft>,
    /// 请求阶段命中的不可变节流计划；转发器仅对请求正文创建上传调度器。
    pub requestThrottlePlan: Option<ThrottlePlan>,
    /// 响应阶段命中的不可变节流计划；转发器仅对上游或本地响应正文创建下载调度器。
    pub responseThrottlePlan: Option<ThrottlePlan>,
    pub shortCircuit: bool,
    pub blocked: bool,
    pub suspended: bool,
    pub flags: TransactionFlags,
    pub appliedTools: Vec<String>,
}

impl PipelineContext {
    /// 构造尚未绑定录制事务的请求上下文；录制创建后由 `bindTransaction` 填入真实 ID。
    pub fn new(clientAddress: String, location: ResolvedLocation, request: RequestDraft) -> Self {
        Self {
            transactionId: None,
            recordingSessionId: None,
            clientAddress,
            clientProcessName: None,
            clientProcessId: None,
            originalLocation: location.clone(),
            location,
            request,
            response: None,
            requestThrottlePlan: None,
            responseThrottlePlan: None,
            shortCircuit: false,
            blocked: false,
            suspended: false,
            flags: TransactionFlags::default(),
            appliedTools: Vec::new(),
        }
    }

    /// 绑定透明捕获提供的本机进程身份；普通显式代理连接保持 None，不进行猜测。
    pub fn bindClientProcess(&mut self, name: Option<String>, processId: Option<u32>) {
        self.clientProcessName = name;
        self.clientProcessId = processId;
    }

    /// 绑定 capture-core 分配的事务与录制会话 ID；录制暂停时两个字段保持 None。
    pub fn bindTransaction(&mut self, transactionId: String, recordingSessionId: String) {
        self.transactionId = Some(transactionId);
        self.recordingSessionId = Some(recordingSessionId);
    }

    /// 将工具标识按首次实际生效顺序写入痕迹，避免请求和响应阶段重复同一工具名称。
    pub fn markApplied(&mut self, id: ToolId) {
        let name = id.asStr();
        if !self.appliedTools.iter().any(|applied| applied == name) {
            self.appliedTools.push(name.to_owned());
        }
    }

    /// 写入合成响应并标记短路；后续响应阶段仍会运行以确保录制与响应工具路径完整。
    pub fn shortCircuit(&mut self, response: SyntheticResponse) {
        self.shortCircuit = true;
        self.response = Some(ResponseDraft::fromSynthetic(response));
    }

    /// 写入阻断响应；阻断是短路的一种，并在最终录制终态中保持独立状态。
    pub fn block(&mut self, response: SyntheticResponse) {
        self.blocked = true;
        self.shortCircuit(response);
    }
}

/// 表示单个工具钩子的调度结果；只有实际改变路径或消息时才应返回 `Applied`。
#[derive(Clone, Debug)]
pub enum PipelineDirective {
    Continue,
    Applied,
    ShortCircuit(SyntheticResponse),
    Blocked(SyntheticResponse),
}

/// 表示请求阶段结束后的出站决定；调用方负责将合成响应送入响应钩子和录制提交。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipelineRequestOutcome {
    Forward,
    Synthetic,
    Blocked,
}

/// 描述注册或工具执行失败；工具配置错误必须在更新配置时拒绝，不应依赖此路径吞没。
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("pipelineDuplicateTool")]
    DuplicateTool,
    #[error("pipelineToolFailed:{toolId}:{code}")]
    ToolFailed { toolId: ToolId, code: String },
    #[error("pipelineResponseShortCircuit:{toolId}")]
    ResponseShortCircuit { toolId: ToolId },
}

/// 定义内置工具与固定流水线之间的异步边界；实现应从自身并发配置中读取最新 enabled 与规则快照。
#[async_trait]
pub trait PipelineTool: Send + Sync {
    /// 返回当前运行快照；该方法不得持有异步锁跨越网络或文件 I/O。
    fn registration(&self) -> ToolRegistration;

    /// 在请求发送前处理可变草稿；默认不产生副作用，使只参与响应的工具无需空实现。
    async fn onRequest(
        &self,
        _context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        Ok(PipelineDirective::Continue)
    }

    /// 在响应返回客户端前处理可变草稿；短路响应同样进入此钩子，默认不产生副作用。
    async fn onResponse(
        &self,
        _context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        Ok(PipelineDirective::Continue)
    }
}

/// 持有可热更新的内置工具集合；每次调度只持锁复制 Arc，绝不跨异步钩子持有注册表锁。
#[derive(Clone, Default)]
pub struct ToolPipeline {
    tools: Arc<RwLock<BTreeMap<ToolId, Arc<dyn PipelineTool>>>>,
}

impl ToolPipeline {
    /// 创建空流水线；空流水线不改变 M1 HTTP 转发、CONNECT 和录制语义。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个内置工具；同一稳定 ToolId 重复注册会返回错误，防止顺序槽产生歧义。
    pub fn register(&self, tool: Arc<dyn PipelineTool>) -> Result<(), PipelineError> {
        let registration = tool.registration();
        let mut tools = self.tools.write();
        if tools.contains_key(&registration.id) {
            return Err(PipelineError::DuplicateTool);
        }
        tools.insert(registration.id, tool);
        Ok(())
    }

    /// 原子替换指定位置的工具实现；控制层可用它切换完整配置容器而不重启监听器。
    pub fn replace(&self, tool: Arc<dyn PipelineTool>) {
        let registration = tool.registration();
        self.tools.write().insert(registration.id, tool);
    }

    /// 删除指定工具；删除后该顺序槽直接跳过，适用于测试和延后加载的内置能力。
    pub fn unregister(&self, id: ToolId) -> Option<Arc<dyn PipelineTool>> {
        self.tools.write().remove(&id)
    }

    /// 返回所有当前工具的热更新注册快照，按 ToolId 稳定排序供控制面构建工具总览。
    pub fn registrations(&self) -> Vec<ToolRegistration> {
        self.tools
            .read()
            .values()
            .map(|tool| tool.registration())
            .collect()
    }

    /// 判断是否存在启用且需要完整请求正文的工具；调用方只在需要时物化正文，避免空管线退化为缓冲代理。
    pub fn requiresRequestBody(&self) -> bool {
        self.requiresBody(ToolPhase::Request, |registration| {
            registration.requiresRequestBody
        })
    }

    /// 判断是否存在启用且需要完整响应正文的工具；调用方据此在响应钩子前选择物化正文或保持流式泵。
    pub fn requiresResponseBody(&self) -> bool {
        self.requiresBody(ToolPhase::Response, |registration| {
            registration.requiresResponseBody
        })
    }

    /// 按固定请求槽运行工具；Map Local/Block 等短路立即停止剩余请求钩子，但不会跳过响应钩子。
    pub async fn runRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineRequestOutcome, PipelineError> {
        for (id, tool) in self.toolsForPhase(ToolPhase::Request) {
            match tool.onRequest(context).await? {
                PipelineDirective::Continue => {}
                PipelineDirective::Applied => context.markApplied(id),
                PipelineDirective::ShortCircuit(response) => {
                    context.markApplied(id);
                    context.shortCircuit(response);
                    return Ok(PipelineRequestOutcome::Synthetic);
                }
                PipelineDirective::Blocked(response) => {
                    context.markApplied(id);
                    context.block(response);
                    return Ok(PipelineRequestOutcome::Blocked);
                }
            }
        }
        Ok(PipelineRequestOutcome::Forward)
    }

    /// 按固定响应槽运行工具；无论上游还是合成响应都会经过这里，保证工具痕迹与捕获终态一致。
    pub async fn runResponse(&self, context: &mut PipelineContext) -> Result<(), PipelineError> {
        self.runResponseTools(context, false).await
    }

    /// 为不能结束的流式响应运行仅依赖响应头的工具；声明完整响应正文需求的工具会被整项跳过，
    /// 防止 SSE 因正文改写或响应断点永久等待流结束。跳过的工具不会写入 `appliedTools`，
    /// 头字段工具仍按固定响应槽顺序执行，失败语义与普通响应流水线保持一致。
    pub async fn runStreamingResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<(), PipelineError> {
        self.runResponseTools(context, true).await
    }

    /// 按响应槽顺序执行当前快照；`skipBodyTools` 只用于协议明确要求持续交付的响应，
    /// 不能用于普通响应，否则正文改写和响应断点的可观察语义会被静默改变。
    async fn runResponseTools(
        &self,
        context: &mut PipelineContext,
        skipBodyTools: bool,
    ) -> Result<(), PipelineError> {
        for (id, tool) in self.toolsForPhase(ToolPhase::Response) {
            if skipBodyTools && tool.registration().requiresResponseBody {
                continue;
            }
            match tool.onResponse(context).await? {
                PipelineDirective::Continue => {}
                PipelineDirective::Applied => context.markApplied(id),
                PipelineDirective::ShortCircuit(_) | PipelineDirective::Blocked(_) => {
                    return Err(PipelineError::ResponseShortCircuit { toolId: id });
                }
            }
        }
        Ok(())
    }

    /// 根据固定槽位和最新注册快照选择需要调用的工具；克隆 Arc 后释放读锁，配置热更新不会阻塞异步钩子。
    fn toolsForPhase(&self, phase: ToolPhase) -> Vec<(ToolId, Arc<dyn PipelineTool>)> {
        let tools = self.tools.read();
        toolOrder(phase)
            .iter()
            .filter_map(|id| tools.get(id).map(|tool| (*id, tool.clone())))
            .filter(|(_, tool)| {
                let registration = tool.registration();
                registration.enabled && registration.participatesIn(phase)
            })
            .collect()
    }

    /// 查询启用工具的正文访问需求；该检查只读取瞬时配置快照，不会把配置锁带入数据面 I/O。
    fn requiresBody<Selector>(&self, phase: ToolPhase, selector: Selector) -> bool
    where
        Selector: Fn(&ToolRegistration) -> bool,
    {
        self.tools.read().values().any(|tool| {
            let registration = tool.registration();
            registration.enabled && registration.participatesIn(phase) && selector(&registration)
        })
    }
}

/// 返回不可重排的请求与响应工具槽；出站和 capture 由代理调用方承担，因而不作为可注册工具。
fn toolOrder(phase: ToolPhase) -> &'static [ToolId] {
    const requestOrder: &[ToolId] = &[
        ToolId::BlockList,
        ToolId::NoCaching,
        ToolId::BlockCookies,
        ToolId::MapRemote,
        ToolId::MapLocal,
        ToolId::Rewrite,
        ToolId::Breakpoints,
        ToolId::Throttling,
        // 镜像需要在请求出站前获取已被前序工具改写后的完整报文；漏列会导致请求方向永远不落盘。
        ToolId::Mirror,
    ];
    const responseOrder: &[ToolId] = &[
        ToolId::Rewrite,
        ToolId::Breakpoints,
        ToolId::NoCaching,
        ToolId::BlockCookies,
        ToolId::Throttling,
        ToolId::Mirror,
        ToolId::AutoSave,
    ];
    match phase {
        ToolPhase::Request => requestOrder,
        ToolPhase::Response => responseOrder,
    }
}
