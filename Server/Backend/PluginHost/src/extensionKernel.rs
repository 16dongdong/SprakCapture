//! 提供运行时无关的扩展调度内核。
//!
//! 内核只负责匹配、排序、调度、代际、动作契约复验和审计。第三方代码由运行时
//! 适配器执行，网络模块只向内核提交阶段事件，因此不会依赖 Wasm、Sidecar 或工作进程协议。

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    net::IpAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

use ipnet::IpNet;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::{
    ActionKind, EventEnvelope, ExtensionAction, ExtensionManifest, ExtensionMatch, FailurePolicy,
    InterceptionMode, ModuleKind, PluginExecutionOptions, Stage, StageSubscription,
    availableActions,
};

const MAXIMUM_INVOCATION_TRACES: usize = 4_096;

/// 描述内核传给运行时的一次不可变调用；截止字段仅供插件作者自行决定调度策略。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInvocation {
    pub pluginId: String,
    pub moduleId: String,
    pub moduleKind: ModuleKind,
    pub envelope: EventEnvelope,
}

/// 定义所有运行时适配器共享的统一接口；运行时是否隔离完全由插件作者选择。
pub trait ExtensionRuntime: Send + Sync {
    /// 执行一次阶段调用；宿主原样等待结果，不注入超时、队列或资源策略。
    fn invoke<'a>(
        &'a self,
        invocation: RuntimeInvocation,
    ) -> Pin<Box<dyn Future<Output = Result<ExtensionAction, String>> + Send + 'a>>;

    /// 通知运行时宿主生命周期已停止；插件作者自行处理线程、连接和其他资源。
    fn stop(&self);
}

/// 描述一次插件调用的审计结果；正文不进入追踪，只保留大小、动作、耗时和稳定错误码。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvocationTrace {
    pub pluginId: String,
    pub moduleId: String,
    pub eventId: String,
    pub stage: Stage,
    pub action: Option<ActionKind>,
    pub elapsedMicroseconds: u64,
    pub inputBytes: usize,
    pub outputBytes: usize,
    pub errorCode: Option<String>,
}

/// 描述一个已发布插件实例的运行状态；控制面不暴露运行时对象、路径或配置正文。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInstanceSnapshot {
    pub pluginId: String,
    pub version: String,
    pub runtimeKind: crate::ExtensionRuntimeKind,
    pub instanceGeneration: u64,
    pub consecutiveFailures: u64,
    pub inFlightInvocations: usize,
}

/// 返回一条阶段事件经过全部插件后的最终视图、动作链、标注和审计信息。
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchResult {
    pub finalPayload: JsonValue,
    pub appliedActions: Vec<ExtensionAction>,
    pub annotations: Vec<JsonValue>,
    pub terminalAction: Option<ActionKind>,
    pub traces: Vec<InvocationTrace>,
}

/// 描述内核无法继续当前阶段的精确原因；调用方根据阶段把它映射为协议正确的拒绝或关闭。
#[derive(Debug, Error, Eq, PartialEq)]
pub enum DispatchFailure {
    #[error("extensionInstanceNotFound")]
    InstanceNotFound,
    #[error("extensionConfigurationInvalid")]
    ConfigurationInvalid,
    #[error("extensionRuntimeFailed")]
    RuntimeFailed,
    #[error("extensionInvalidAction")]
    InvalidAction,
    #[error("extensionGenerationExpired")]
    GenerationExpired,
    #[error("extensionPatchFailed")]
    PatchFailed,
}

/// 保存一个已编译订阅；匹配器在注册或覆盖变更时构建，数据面不再解析 manifest。
#[derive(Clone)]
struct CompiledSubscription {
    pluginId: String,
    moduleId: String,
    moduleKind: ModuleKind,
    order: (usize, i32, String, String),
    matchRule: CompiledMatch,
}

/// 保存预编译后的匹配条件；CIDR 和大小写规范化只在控制面执行一次。
#[derive(Clone, Default)]
struct CompiledMatch {
    entries: Vec<String>,
    processNames: Vec<String>,
    processPaths: Vec<String>,
    transports: Vec<String>,
    protocols: Vec<String>,
    directions: Vec<String>,
    schemes: Vec<String>,
    hosts: Vec<String>,
    cidrs: Vec<IpNet>,
    ports: Vec<u16>,
    methods: Vec<String>,
    paths: Vec<String>,
    statusCodes: Vec<u16>,
    mimeTypes: Vec<String>,
    labels: Vec<String>,
}

/// 保存一个发布后的插件实例；宿主只记录调用状态，不对可信 Mod 施加并发、超时或熔断门禁。
struct ExtensionInstance {
    manifest: ExtensionManifest,
    options: PluginExecutionOptions,
    runtime: Arc<dyn ExtensionRuntime>,
    instanceGeneration: u64,
    consecutiveFailures: AtomicU64,
    inFlightInvocations: AtomicUsize,
}
impl ExtensionInstance {
    /// 记录调用成功并关闭连续失败窗口；历史总量由追踪系统负责，不在此处累积。
    fn recordSuccess(&self) {
        self.consecutiveFailures.store(0, Ordering::Release);
    }

    /// 记录调用失败供诊断界面展示；开放可信模式不会据此停用或熔断插件。
    fn recordFailure(&self) {
        self.consecutiveFailures.fetch_add(1, Ordering::AcqRel);
    }
}

/// 在一次运行时调用期间维护只读诊断计数；析构保证错误和取消路径都会归还计数。
struct InFlightInvocationGuard<'a> {
    counter: &'a AtomicUsize,
}

impl<'a> InFlightInvocationGuard<'a> {
    /// 登记一次正在执行的调用；计数只用于观测，不会阻止插件继续接收事件。
    fn enter(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self { counter }
    }
}

impl Drop for InFlightInvocationGuard<'_> {
    /// 归还调用计数；运行时错误、取消和正常完成共享同一条释放路径。
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 保存扩展内核的发布态；计划和实例在注册、重载或卸载时一起原子替换。
struct KernelState {
    instances: BTreeMap<String, Arc<ExtensionInstance>>,
    plans: HashMap<Stage, Arc<[CompiledSubscription]>>,
}

impl Default for KernelState {
    /// 创建没有插件的透明内核；空计划不会分配事件副本或进入运行时。
    fn default() -> Self {
        Self {
            instances: BTreeMap::new(),
            plans: HashMap::new(),
        }
    }
}

/// 提供可热更新的插件执行内核；所有克隆共享同一发布态与代际计数。
#[derive(Clone)]
pub struct ExtensionKernel {
    state: Arc<RwLock<KernelState>>,
    serviceGeneration: Arc<AtomicU64>,
    recordingGeneration: Arc<AtomicU64>,
    nextInstanceGeneration: Arc<AtomicU64>,
    traces: Arc<Mutex<VecDeque<InvocationTrace>>>,
}

impl Default for ExtensionKernel {
    /// 创建透明内核；调用方可随后原子注册运行时实例。
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(KernelState::default())),
            serviceGeneration: Arc::new(AtomicU64::new(0)),
            recordingGeneration: Arc::new(AtomicU64::new(0)),
            nextInstanceGeneration: Arc::new(AtomicU64::new(1)),
            traces: Arc::new(Mutex::new(VecDeque::with_capacity(
                MAXIMUM_INVOCATION_TRACES,
            ))),
        }
    }
}

impl ExtensionKernel {
    /// 注册或热替换一个完整插件实例；验证失败时旧实例和执行计划保持不变。
    pub fn register(
        &self,
        manifest: ExtensionManifest,
        options: PluginExecutionOptions,
        runtime: Arc<dyn ExtensionRuntime>,
    ) -> Result<(), DispatchFailure> {
        validateExecutionOptions(&manifest, &options)?;
        let instance = Arc::new(ExtensionInstance {
            manifest,
            options,
            runtime,
            instanceGeneration: self.nextInstanceGeneration.fetch_add(1, Ordering::Relaxed),
            consecutiveFailures: AtomicU64::new(0),
            inFlightInvocations: AtomicUsize::new(0),
        });
        let pluginId = instance.manifest.id.clone();
        let previous = {
            let mut state = self.state.write();
            let mut proposedInstances = state.instances.clone();
            let previous = proposedInstances.insert(pluginId, instance);
            let proposedPlans = compilePlans(&proposedInstances)?;
            state.instances = proposedInstances;
            state.plans = proposedPlans;
            previous
        };
        if let Some(previous) = previous {
            previous.runtime.stop();
        }
        Ok(())
    }

    /// 原子卸载插件实例和全部贡献计划；返回后新事件不再引用旧实例。
    pub fn remove(&self, pluginId: &str) -> Result<bool, DispatchFailure> {
        let removed = {
            let mut state = self.state.write();
            let mut proposedInstances = state.instances.clone();
            let removed = proposedInstances.remove(pluginId);
            let proposedPlans = compilePlans(&proposedInstances)?;
            state.instances = proposedInstances;
            state.plans = proposedPlans;
            removed
        };
        if let Some(instance) = removed {
            instance.runtime.stop();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 发布新的服务代际；所有仍在执行的旧代调用即使返回也不会再应用结果。
    pub fn setServiceGeneration(&self, generation: u64) {
        self.serviceGeneration.store(generation, Ordering::Release);
    }

    /// 发布新的录制代际；清空后旧事件产生的标注和录制动作会被拒绝。
    pub fn setRecordingGeneration(&self, generation: u64) {
        self.recordingGeneration
            .store(generation, Ordering::Release);
    }

    /// 返回创建阶段事件所需的当前服务与录制代际；调用方必须把这两个值原样写入同一事件信封。
    ///
    /// 运行上下文：数据面在读取线上块后调用，两个原子读取不会持有执行计划锁。
    /// 失败语义：本函数不失败；若读取后发生代际切换，`dispatch` 会拒绝该旧事件。
    pub fn currentGenerations(&self) -> (u64, u64) {
        (
            self.serviceGeneration.load(Ordering::Acquire),
            self.recordingGeneration.load(Ordering::Acquire),
        )
    }

    /// 判断指定阶段是否存在已编译订阅；数据面据此跳过事件对象和 JSON 字节数组分配。
    ///
    /// 运行上下文：每个 TCP/UDP 块进入完整 Mod 调度前调用，读取锁只保护不可变计划指针。
    /// 失败语义：本函数不失败；热替换并发发生时，当前块使用读取瞬间的计划存在性。
    pub fn hasSubscriptions(&self, stage: Stage) -> bool {
        self.state.read().plans.contains_key(&stage)
    }

    /// 返回全部插件实例的脱敏运行快照；顺序按插件 ID 稳定，便于 UI 和诊断包比较。
    pub fn snapshots(&self) -> Vec<ExtensionInstanceSnapshot> {
        self.state
            .read()
            .instances
            .values()
            .map(|instance| ExtensionInstanceSnapshot {
                pluginId: instance.manifest.id.clone(),
                version: instance.manifest.version.clone(),
                runtimeKind: instance.manifest.runtime.kind,
                instanceGeneration: instance.instanceGeneration,
                consecutiveFailures: instance.consecutiveFailures.load(Ordering::Acquire),
                inFlightInvocations: instance.inFlightInvocations.load(Ordering::Acquire),
            })
            .collect()
    }

    /// 返回最新调用追踪；limit 为零时不克隆任何条目，最大值受固定环形预算限制。
    pub fn invocationTraces(&self, limit: usize) -> Vec<InvocationTrace> {
        if limit == 0 {
            return Vec::new();
        }
        let traces = self.traces.lock();
        let skip = traces
            .len()
            .saturating_sub(limit.min(MAXIMUM_INVOCATION_TRACES));
        traces.iter().skip(skip).cloned().collect()
    }

    /// 清空诊断追踪而不影响执行计划或运行实例；正文从未进入该缓冲。
    pub fn clearInvocationTraces(&self) {
        self.traces.lock().clear();
    }

    /// 顺序执行命中当前阶段的插件计划；修改结果会成为后续插件的输入，终止动作结束该阶段。
    pub async fn dispatch(
        &self,
        mut envelope: EventEnvelope,
    ) -> Result<DispatchResult, DispatchFailure> {
        validateEnvelopeGeneration(self, &envelope)?;
        let subscriptions = {
            let state = self.state.read();
            state.plans.get(&envelope.stage).cloned()
        };
        let Some(subscriptions) = subscriptions else {
            return Ok(emptyDispatch(envelope.payload));
        };

        let mut dispatch = emptyDispatch(envelope.payload.clone());
        for subscription in subscriptions.iter() {
            if !subscription.matchRule.matches(&envelope) {
                continue;
            }
            let instance = {
                let state = self.state.read();
                state.instances.get(&subscription.pluginId).cloned()
            }
            .ok_or(DispatchFailure::InstanceNotFound)?;
            envelope.pluginInstanceId = format!(
                "{}@{}#{}",
                instance.manifest.id, instance.manifest.version, instance.instanceGeneration
            );
            envelope.payload = dispatch.finalPayload.clone();
            let invocation = RuntimeInvocation {
                pluginId: subscription.pluginId.clone(),
                moduleId: subscription.moduleId.clone(),
                moduleKind: subscription.moduleKind,
                envelope: envelope.clone(),
            };
            let startedAt = Instant::now();
            let inputBytes = serializedSize(&envelope.payload);
            let callResult = self.invoke(&instance, invocation).await;
            let elapsedMicroseconds = startedAt.elapsed().as_micros().min(u64::MAX as u128) as u64;
            match callResult {
                Ok(action) => {
                    let outputBytes = serializedSize(&action);
                    let validation = validateAction(&envelope, &action);
                    if let Err(failure) = validation {
                        instance.recordFailure();
                        let trace = failureTrace(
                            subscription,
                            &envelope,
                            elapsedMicroseconds,
                            inputBytes,
                            outputBytes,
                            &failure,
                        );
                        self.recordTrace(trace.clone());
                        dispatch.traces.push(trace);
                        applyFailurePolicy(&instance, failure)?;
                        continue;
                    }
                    applyAction(&mut dispatch, &action)?;
                    instance.recordSuccess();
                    let trace = successTrace(
                        subscription,
                        &envelope,
                        elapsedMicroseconds,
                        inputBytes,
                        outputBytes,
                        action.action,
                    );
                    self.recordTrace(trace.clone());
                    dispatch.traces.push(trace);
                    dispatch.appliedActions.push(action.clone());
                    if terminalAction(action.action) {
                        dispatch.terminalAction = Some(action.action);
                        break;
                    }
                }
                Err(failure) => {
                    instance.recordFailure();
                    let trace = failureTrace(
                        subscription,
                        &envelope,
                        elapsedMicroseconds,
                        inputBytes,
                        0,
                        &failure,
                    );
                    self.recordTrace(trace.clone());
                    dispatch.traces.push(trace);
                    applyFailurePolicy(&instance, failure)?;
                }
            }
            validateEnvelopeGeneration(self, &envelope)?;
        }
        Ok(dispatch)
    }

    /// 直接调用插件运行时；插件作者自行决定并发、超时、线程和资源策略。
    ///
    /// 运行上下文：数据面按订阅顺序等待返回，因此阻塞型 Native Mod 会直接影响对应连接。
    /// 失败语义：运行时错误只按插件声明的失败策略映射，不触发宿主队列、熔断或超时门禁。
    async fn invoke(
        &self,
        instance: &Arc<ExtensionInstance>,
        invocation: RuntimeInvocation,
    ) -> Result<ExtensionAction, DispatchFailure> {
        let _inFlight = InFlightInvocationGuard::enter(&instance.inFlightInvocations);
        instance
            .runtime
            .invoke(invocation)
            .await
            .map_err(|_| DispatchFailure::RuntimeFailed)
    }

    /// 把一条脱敏追踪写入固定环形缓冲；写满时只淘汰最早诊断，不影响数据面动作结果。
    fn recordTrace(&self, trace: InvocationTrace) {
        let mut traces = self.traces.lock();
        if traces.len() == MAXIMUM_INVOCATION_TRACES {
            traces.pop_front();
        }
        traces.push_back(trace);
    }
}

/// 将全部实例编译为按阶段索引的稳定执行计划；控制面更新失败不会发布半成品计划。
fn compilePlans(
    instances: &BTreeMap<String, Arc<ExtensionInstance>>,
) -> Result<HashMap<Stage, Arc<[CompiledSubscription]>>, DispatchFailure> {
    let mut plans: HashMap<Stage, Vec<CompiledSubscription>> = HashMap::new();
    for (pluginId, instance) in instances {
        for module in &instance.manifest.modules {
            let userOrder = instance
                .options
                .moduleOrder
                .iter()
                .position(|moduleId| moduleId == &module.id)
                .unwrap_or(usize::MAX);
            for StageSubscription {
                stage,
                order,
                matchRule,
            } in &module.subscriptions
            {
                let overrideKey = format!("{}.{}", module.id, stageName(*stage));
                let effectiveMatch = instance
                    .options
                    .subscriptionOverrides
                    .get(&overrideKey)
                    .unwrap_or(matchRule);
                plans.entry(*stage).or_default().push(CompiledSubscription {
                    pluginId: pluginId.clone(),
                    moduleId: module.id.clone(),
                    moduleKind: module.kind,
                    order: (userOrder, *order, pluginId.clone(), module.id.clone()),
                    matchRule: CompiledMatch::compile(effectiveMatch)?,
                });
            }
        }
    }
    Ok(plans
        .into_iter()
        .map(|(stage, mut subscriptions)| {
            subscriptions.sort_by(|left, right| left.order.cmp(&right.order));
            (stage, Arc::from(subscriptions))
        })
        .collect())
}

impl CompiledMatch {
    /// 编译匹配规则中的大小写规范化和 CIDR；失败表示用户覆盖或清单整体不可发布。
    fn compile(matchRule: &ExtensionMatch) -> Result<Self, DispatchFailure> {
        let cidrs = matchRule
            .cidrs
            .iter()
            .map(|cidr| {
                cidr.parse::<IpNet>()
                    .map_err(|_| DispatchFailure::InvalidAction)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            entries: normalized(&matchRule.entries),
            processNames: normalized(&matchRule.processNames),
            processPaths: normalized(&matchRule.processPaths),
            transports: normalized(&matchRule.transports),
            protocols: normalized(&matchRule.protocols),
            directions: normalized(&matchRule.directions),
            schemes: normalized(&matchRule.schemes),
            hosts: normalized(&matchRule.hosts),
            cidrs,
            ports: matchRule.ports.clone(),
            methods: normalized(&matchRule.methods),
            paths: matchRule.paths.clone(),
            statusCodes: matchRule.statusCodes.clone(),
            mimeTypes: normalized(&matchRule.mimeTypes),
            labels: normalized(&matchRule.labels),
        })
    }

    /// 判断事件是否命中所有已声明维度；每个维度为空时不施加限制。
    fn matches(&self, envelope: &EventEnvelope) -> bool {
        let context = &envelope.context;
        matchesValue(&self.entries, context.entry.as_deref(), wildcardMatch)
            && matchesValue(
                &self.processNames,
                context.processName.as_deref(),
                wildcardMatch,
            )
            && matchesValue(
                &self.processPaths,
                context.processPath.as_deref(),
                wildcardMatch,
            )
            && matchesValue(&self.transports, context.transport.as_deref(), exactMatch)
            && matchesValue(&self.protocols, context.protocol.as_deref(), exactMatch)
            && matchesValue(&self.directions, context.direction.as_deref(), exactMatch)
            && matchesValue(&self.schemes, context.scheme.as_deref(), exactMatch)
            && matchesValue(&self.hosts, context.host.as_deref(), wildcardMatch)
            && matchesAddress(&self.cidrs, context.address.as_deref())
            && matchesNumber(&self.ports, context.port)
            && matchesValue(&self.methods, context.method.as_deref(), exactMatch)
            && matchesPath(&self.paths, context.path.as_deref())
            && matchesNumber(&self.statusCodes, context.statusCode)
            && matchesValue(&self.mimeTypes, context.mimeType.as_deref(), wildcardMatch)
            && matchesLabels(&self.labels, &context.labels)
    }
}

/// 校验用户顺序、订阅覆盖和可选资源参数的结构一致性；能力列表仅用于管理界面展示，不参与运行拦截。
fn validateExecutionOptions(
    manifest: &ExtensionManifest,
    options: &PluginExecutionOptions,
) -> Result<(), DispatchFailure> {
    let uniqueModuleOrder = options
        .moduleOrder
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let validOverrideKeys = manifest
        .modules
        .iter()
        .flat_map(|module| {
            module
                .subscriptions
                .iter()
                .map(|subscription| format!("{}.{}", module.id, stageName(subscription.stage)))
        })
        .collect::<std::collections::BTreeSet<_>>();
    if uniqueModuleOrder.len() != options.moduleOrder.len()
        || options
            .moduleOrder
            .iter()
            .any(|moduleId| !manifest.modules.iter().any(|module| &module.id == moduleId))
        || options
            .subscriptionOverrides
            .keys()
            .any(|overrideKey| !validOverrideKeys.contains(overrideKey))
    {
        return Err(DispatchFailure::ConfigurationInvalid);
    }
    Ok(())
}

/// 验证动作属于当前阶段与线上可变性契约；不执行能力、输出大小或资源授权检查。
fn validateAction(
    envelope: &EventEnvelope,
    action: &ExtensionAction,
) -> Result<(), DispatchFailure> {
    if action.eventId != envelope.eventId
        || !availableActions(envelope.stage).contains(&action.action)
    {
        return Err(DispatchFailure::InvalidAction);
    }
    if envelope.context.interceptionMode == InterceptionMode::ObserveOnly
        && !matches!(action.action, ActionKind::Continue | ActionKind::Annotate)
    {
        return Err(DispatchFailure::InvalidAction);
    }
    Ok(())
}

/// 把已验证动作应用到阶段草稿；结构化补丁失败时保持调用前 payload 不变。
fn applyAction(
    dispatch: &mut DispatchResult,
    action: &ExtensionAction,
) -> Result<(), DispatchFailure> {
    dispatch.annotations.extend(action.annotations.clone());
    if action.action != ActionKind::Modify {
        return Ok(());
    }
    if let Some(output) = &action.output {
        dispatch.finalPayload = output.clone();
    }
    if action.patch.is_empty() {
        return Ok(());
    }
    let patchValue = JsonValue::Array(action.patch.clone());
    let patch = serde_json::from_value::<json_patch::Patch>(patchValue)
        .map_err(|_| DispatchFailure::PatchFailed)?;
    json_patch::patch(&mut dispatch.finalPayload, &patch).map_err(|_| DispatchFailure::PatchFailed)
}

/// 检查事件代际仍为当前发布值；旧服务或旧录制结果没有任何应用窗口。
fn validateEnvelopeGeneration(
    kernel: &ExtensionKernel,
    envelope: &EventEnvelope,
) -> Result<(), DispatchFailure> {
    let serviceMatches =
        kernel.serviceGeneration.load(Ordering::Acquire) == envelope.serviceGeneration;
    let recordingMatches = !recordingStage(envelope.stage)
        || kernel.recordingGeneration.load(Ordering::Acquire) == envelope.recordingGeneration;
    (serviceMatches && recordingMatches)
        .then_some(())
        .ok_or(DispatchFailure::GenerationExpired)
}

/// 应用实例失败策略；开放策略仅跳过当前插件，关闭策略终止当前阶段。
fn applyFailurePolicy(
    instance: &ExtensionInstance,
    failure: DispatchFailure,
) -> Result<(), DispatchFailure> {
    match instance.options.failurePolicy {
        FailurePolicy::FailOpen => Ok(()),
        FailurePolicy::FailClosed => Err(failure),
    }
}

/// 标识会改变控制流并终止后续插件链的动作。
fn terminalAction(action: ActionKind) -> bool {
    matches!(
        action,
        ActionKind::Reject | ActionKind::Respond | ActionKind::Redirect | ActionKind::Close
    )
}

/// 标识结果必须受录制代际保护的阶段。
fn recordingStage(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::BeforeRecord
            | Stage::TransactionUpdated
            | Stage::TransactionCompleted
            | Stage::RecordingCleared
            | Stage::InspectorDataRequested
    )
}

/// 创建没有插件命中的透明调度结果。
fn emptyDispatch(payload: JsonValue) -> DispatchResult {
    DispatchResult {
        finalPayload: payload,
        appliedActions: Vec::new(),
        annotations: Vec::new(),
        terminalAction: None,
        traces: Vec::new(),
    }
}

/// 创建成功调用追踪；所有字节计数均是结构化消息大小，不代表线上流量大小。
fn successTrace(
    subscription: &CompiledSubscription,
    envelope: &EventEnvelope,
    elapsedMicroseconds: u64,
    inputBytes: usize,
    outputBytes: usize,
    action: ActionKind,
) -> InvocationTrace {
    InvocationTrace {
        pluginId: subscription.pluginId.clone(),
        moduleId: subscription.moduleId.clone(),
        eventId: envelope.eventId.clone(),
        stage: envelope.stage,
        action: Some(action),
        elapsedMicroseconds,
        inputBytes,
        outputBytes,
        errorCode: None,
    }
}

/// 创建失败调用追踪；内部错误对象不会跨越控制 API，只发布稳定错误码。
fn failureTrace(
    subscription: &CompiledSubscription,
    envelope: &EventEnvelope,
    elapsedMicroseconds: u64,
    inputBytes: usize,
    outputBytes: usize,
    failure: &DispatchFailure,
) -> InvocationTrace {
    InvocationTrace {
        pluginId: subscription.pluginId.clone(),
        moduleId: subscription.moduleId.clone(),
        eventId: envelope.eventId.clone(),
        stage: envelope.stage,
        action: None,
        elapsedMicroseconds,
        inputBytes,
        outputBytes,
        errorCode: Some(failure.to_string()),
    }
}

/// 返回 JSON 序列化后的实际追踪大小；无法序列化时记录为最大值以暴露诊断异常。
fn serializedSize(value: &impl Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

/// 将大小写不敏感的匹配字段预先规范化。
fn normalized(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

/// 判断可选字符串是否命中给定维度；规则非空而上下文缺失时明确不命中。
fn matchesValue(rules: &[String], value: Option<&str>, predicate: fn(&str, &str) -> bool) -> bool {
    rules.is_empty()
        || value.is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            rules.iter().any(|rule| predicate(rule, &value))
        })
}

/// 执行大小写规范化后的精确匹配。
fn exactMatch(rule: &str, value: &str) -> bool {
    rule == value
}

/// 执行主机、MIME 与进程字段的单星号通配匹配；不引入正则回溯路径。
fn wildcardMatch(rule: &str, value: &str) -> bool {
    if rule == "*" {
        return true;
    }
    let Some((prefix, suffix)) = rule.split_once('*') else {
        return rule == value;
    };
    value.len() >= prefix.len() + suffix.len()
        && value.starts_with(prefix)
        && value.ends_with(suffix)
}

/// 判断地址是否属于任一预编译 CIDR；没有声明 CIDR 时不限制地址。
fn matchesAddress(cidrs: &[IpNet], address: Option<&str>) -> bool {
    cidrs.is_empty()
        || address
            .and_then(|address| address.parse::<IpAddr>().ok())
            .is_some_and(|address| cidrs.iter().any(|cidr| cidr.contains(&address)))
}

/// 判断可选数字是否命中声明集合。
fn matchesNumber<T: Eq>(rules: &[T], value: Option<T>) -> bool {
    rules.is_empty() || value.is_some_and(|value| rules.contains(&value))
}

/// 判断 HTTP 路径是否命中精确值或末尾星号前缀规则。
fn matchesPath(rules: &[String], value: Option<&str>) -> bool {
    rules.is_empty()
        || value.is_some_and(|value| {
            rules.iter().any(|rule| {
                rule.strip_suffix('*')
                    .map_or(rule == value, |prefix| value.starts_with(prefix))
            })
        })
}

/// 判断事务标签是否包含所有声明标签；标签比较不区分 ASCII 大小写。
fn matchesLabels(rules: &[String], labels: &[String]) -> bool {
    rules.is_empty()
        || rules
            .iter()
            .all(|rule| labels.iter().any(|label| label.eq_ignore_ascii_case(rule)))
}

/// 返回阶段的稳定线名；订阅覆盖键依赖该值，不能随内部枚举重构改变。
fn stageName(stage: Stage) -> &'static str {
    match stage {
        Stage::ServiceStarting => "serviceStarting",
        Stage::ServiceStarted => "serviceStarted",
        Stage::ConfigurationChanged => "configurationChanged",
        Stage::ServiceStopping => "serviceStopping",
        Stage::ConnectionAccepted => "connectionAccepted",
        Stage::Socks5Authentication => "socks5Authentication",
        Stage::ProtocolClassified => "protocolClassified",
        Stage::TargetResolving => "targetResolving",
        Stage::BeforeConnect => "beforeConnect",
        Stage::Connected => "connected",
        Stage::ConnectionClosing => "connectionClosing",
        Stage::ClientHelloObserved => "clientHelloObserved",
        Stage::CertificateSelecting => "certificateSelecting",
        Stage::TlsEstablished => "tlsEstablished",
        Stage::TlsFailed => "tlsFailed",
        Stage::RequestHeaders => "requestHeaders",
        Stage::RequestBodyChunk => "requestBodyChunk",
        Stage::RequestComplete => "requestComplete",
        Stage::BeforeUpstream => "beforeUpstream",
        Stage::ResponseHeaders => "responseHeaders",
        Stage::ResponseBodyChunk => "responseBodyChunk",
        Stage::ResponseComplete => "responseComplete",
        Stage::WebSocketOpening => "webSocketOpening",
        Stage::WebSocketFrame => "webSocketFrame",
        Stage::WebSocketClosing => "webSocketClosing",
        Stage::TcpChunk => "tcpChunk",
        Stage::UdpDatagram => "udpDatagram",
        Stage::DnsMessage => "dnsMessage",
        Stage::BeforeRecord => "beforeRecord",
        Stage::TransactionUpdated => "transactionUpdated",
        Stage::TransactionCompleted => "transactionCompleted",
        Stage::RecordingCleared => "recordingCleared",
        Stage::InspectorDataRequested => "inspectorDataRequested",
        Stage::CommandInvoked => "commandInvoked",
        Stage::ContextActionInvoked => "contextActionInvoked",
    }
}
