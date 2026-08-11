//! 定义插件包、阶段事件、开放访问策略和动作的公共模型。
//!
//! 这一层不依赖具体运行时和代理实现。Wasm、Sidecar、Native Worker、控制 API、CLI 与
//! 模拟宿主必须复用同一组类型，避免不同入口对权限、阶段或失败策略产生不一致解释。

use std::{collections::BTreeMap, path::Path};

use schemars::JsonSchema;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::PluginHostError;

pub const EXTENSION_MANIFEST_VERSION: u32 = 2;
const EXTENSION_RUNTIME_PROTOCOL_VERSION: &str = "2.0";
const MAXIMUM_IDENTIFIER_BYTES: usize = 128;
const MAXIMUM_MODULES: usize = 128;
const MAXIMUM_SUBSCRIPTIONS_PER_MODULE: usize = 256;
const MAXIMUM_RUNTIME_ARGUMENTS: usize = 64;
const MAXIMUM_RUNTIME_ARGUMENT_BYTES: usize = 4_096;

/// 标识插件作者可自由选择的运行方式；`Native` 允许动态库直接装入代理主进程。
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionRuntimeKind {
    Wasm,
    Sidecar,
    NativeWorker,
    Native,
    LegacyNative,
}

/// 描述一个插件包的运行入口；入口路径始终相对于已校验的插件版本目录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRuntimeManifest {
    pub kind: ExtensionRuntimeKind,
    pub entry: String,
    #[serde(default)]
    pub protocolVersion: Option<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
}

/// 声明插件要求的宿主和扩展 API 版本范围；安装与启用均要重新验证该边界。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineRequirements {
    pub host: String,
    pub api: String,
}

/// 标识插件包内模块的职责；模块类型与运行时相互独立。
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ModuleKind {
    TrafficHandler,
    StreamTransformer,
    ProtocolDecoder,
    RecordingPolicy,
    BodyViewer,
    MediaRenderer,
    Importer,
    Exporter,
    CommandProvider,
    UiContribution,
    BackgroundService,
}

/// 标识宿主发布给插件的稳定处理阶段；枚举值同时作为 RPC、WIT 和控制 API 的线名。
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    ServiceStarting,
    ServiceStarted,
    ConfigurationChanged,
    ServiceStopping,
    ConnectionAccepted,
    Socks5Authentication,
    ProtocolClassified,
    TargetResolving,
    BeforeConnect,
    Connected,
    ConnectionClosing,
    ClientHelloObserved,
    CertificateSelecting,
    TlsEstablished,
    TlsFailed,
    RequestHeaders,
    RequestBodyChunk,
    RequestComplete,
    BeforeUpstream,
    ResponseHeaders,
    ResponseBodyChunk,
    ResponseComplete,
    WebSocketOpening,
    WebSocketFrame,
    WebSocketClosing,
    TcpChunk,
    UdpDatagram,
    DnsMessage,
    BeforeRecord,
    TransactionUpdated,
    TransactionCompleted,
    RecordingCleared,
    InspectorDataRequested,
    CommandInvoked,
    ContextActionInvoked,
}

/// 标识插件对阶段事件可返回的标准动作；宿主只根据阶段和线上数据性质复验结构。
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ActionKind {
    Continue,
    Modify,
    Hold,
    Drop,
    Reject,
    Respond,
    Redirect,
    Annotate,
    Close,
}

/// 保存插件作者自定义的行为说明标签；宿主接受任意字符串且不据此授权或拒绝调用。
pub type Capability = String;

/// 标识插件可以向宿主注册的声明式贡献点。
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ContributionKind {
    Settings,
    Commands,
    InspectorTabs,
    TransactionContextActions,
    ConnectionBadges,
    StatusItems,
    Decoders,
    BodyViewers,
    MediaRenderers,
    Importers,
    Exporters,
    WorkspacePanels,
}

/// 描述插件订阅的连接、协议和事务范围；空字段表示该维度不限制。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionMatch {
    #[serde(default)]
    pub entries: Vec<String>,
    #[serde(default)]
    pub processNames: Vec<String>,
    #[serde(default)]
    pub processPaths: Vec<String>,
    #[serde(default)]
    pub transports: Vec<String>,
    #[serde(default)]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub directions: Vec<String>,
    #[serde(default)]
    pub schemes: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub cidrs: Vec<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub statusCodes: Vec<u16>,
    #[serde(default)]
    pub mimeTypes: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

/// 声明模块对一个稳定阶段的订阅和默认执行顺序。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageSubscription {
    pub stage: Stage,
    #[serde(default)]
    pub order: i32,
    #[serde(default, rename = "match")]
    pub matchRule: ExtensionMatch,
}

/// 描述一个插件模块；同一包的模块共享运行时、配置和升级事务。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionModule {
    pub id: String,
    pub kind: ModuleKind,
    #[serde(default)]
    pub subscriptions: Vec<StageSubscription>,
    #[serde(default)]
    pub contributes: Vec<ContributionKind>,
}

/// 描述插件作者希望展示的运行参数；宿主仅持久化和诊断，不据此限制可信 Mod。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionLimits {
    pub timeoutMs: u64,
    pub maxPendingEvents: usize,
    pub maxOutputBytes: usize,
    pub maxStorageBytes: u64,
}

impl Default for ExtensionLimits {
    /// 返回脚手架使用的建议值；插件可忽略或改写，宿主不会把这些值变成执行门禁。
    fn default() -> Self {
        Self {
            timeoutMs: 50,
            maxPendingEvents: 128,
            maxOutputBytes: 1024 * 1024,
            maxStorageBytes: 64 * 1024 * 1024,
        }
    }
}

/// 描述运行时失败对当前阶段的处理方式；插件作者提供默认值，用户可以按实例覆盖。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FailurePolicy {
    #[default]
    FailClosed,
    FailOpen,
}

/// 描述完整插件包清单；未知字段拒绝，仅 `extensions` 允许发布者保存命名空间数据。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionManifest {
    pub manifestVersion: u32,
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub publisher: String,
    pub engines: EngineRequirements,
    pub runtime: ExtensionRuntimeManifest,
    pub modules: Vec<ExtensionModule>,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub limits: ExtensionLimits,
    #[serde(default)]
    pub failurePolicy: FailurePolicy,
    #[serde(default)]
    pub configurationSchema: Option<String>,
    #[serde(default)]
    pub contributes: Option<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, JsonValue>,
}

impl ExtensionManifest {
    /// 解析并完整校验外部清单；失败表示该包不能安装或启用，调用方不得部分采用字段。
    pub fn parse(bytes: &[u8]) -> Result<Self, PluginHostError> {
        let manifest =
            serde_json::from_slice(bytes).map_err(|_| PluginHostError::InvalidManifest)?;
        validateManifest(manifest)
    }
}

/// 保存用户对顺序、匹配、失败策略和运行说明的权威覆盖；运行说明不产生宿主门禁。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginExecutionOptions {
    #[serde(default)]
    pub moduleOrder: Vec<String>,
    #[serde(default)]
    pub subscriptionOverrides: BTreeMap<String, ExtensionMatch>,
    #[serde(default)]
    pub failurePolicy: FailurePolicy,
    #[serde(default)]
    pub limits: Option<ExtensionLimits>,
}

/// 标识事件对应的数据面是否允许插件改变线上内容。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InterceptionMode {
    Intercept,
    ObserveOnly,
}

/// 保存每个阶段都可读取的稳定上下文；不适用字段使用 `None`，不以空字符串伪造身份。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageContext {
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub processId: Option<u32>,
    #[serde(default)]
    pub processName: Option<String>,
    #[serde(default)]
    pub processPath: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub scheme: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub statusCode: Option<u16>,
    #[serde(default)]
    pub mimeType: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub interceptionMode: InterceptionMode,
}

impl Default for InterceptionMode {
    /// 未明确声明的数据面事件默认为可拦截；SNIFF 调用方必须显式设置只观察模式。
    fn default() -> Self {
        Self::Intercept
    }
}

/// 定义所有运行时共享的阶段事件信封；代际由宿主保证生命周期，截止时间仅供插件参考。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventEnvelope {
    pub apiVersion: String,
    pub eventId: String,
    pub stage: Stage,
    pub serviceGeneration: u64,
    pub recordingGeneration: u64,
    pub pluginInstanceId: String,
    #[serde(default)]
    pub connectionId: Option<String>,
    #[serde(default)]
    pub transactionId: Option<String>,
    pub deadlineUnixMs: u64,
    pub context: StageContext,
    pub payload: JsonValue,
}

/// 保存插件对当前阶段的结构化决定；运行时输出必须在应用前通过内核复验。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionAction {
    pub eventId: String,
    pub action: ActionKind,
    #[serde(default)]
    pub patch: Vec<JsonValue>,
    #[serde(default)]
    pub annotations: Vec<JsonValue>,
    #[serde(default)]
    pub output: Option<JsonValue>,
}

/// 返回阶段允许的动作集合；运行时、模拟器和控制面阶段浏览器共享该事实源。
pub fn availableActions(stage: Stage) -> &'static [ActionKind] {
    use ActionKind::*;
    match stage {
        Stage::ServiceStarting | Stage::ConfigurationChanged => &[Continue, Reject],
        Stage::ServiceStarted
        | Stage::TlsEstablished
        | Stage::TlsFailed
        | Stage::ConnectionClosing
        | Stage::WebSocketClosing
        | Stage::TransactionUpdated
        | Stage::TransactionCompleted => &[Continue, Annotate],
        Stage::ServiceStopping | Stage::RecordingCleared => &[Continue],
        Stage::ConnectionAccepted | Stage::ClientHelloObserved | Stage::WebSocketOpening => {
            &[Continue, Reject, Annotate]
        }
        Stage::Socks5Authentication => &[Continue, Respond, Reject, Close, Annotate],
        Stage::ProtocolClassified | Stage::TargetResolving | Stage::CertificateSelecting => {
            &[Continue, Modify, Reject, Annotate]
        }
        Stage::BeforeConnect => &[Continue, Redirect, Reject, Annotate],
        Stage::Connected => &[Continue, Annotate],
        Stage::RequestHeaders => &[Continue, Modify, Respond, Reject, Annotate],
        Stage::RequestBodyChunk => &[Continue, Modify, Hold, Drop, Reject],
        Stage::RequestComplete => &[Continue, Respond, Reject, Annotate],
        Stage::BeforeUpstream => &[Continue, Redirect, Respond, Reject],
        Stage::ResponseHeaders => &[Continue, Modify, Reject, Annotate],
        Stage::ResponseBodyChunk => &[Continue, Modify, Hold, Drop, Close],
        Stage::ResponseComplete => &[Continue, Annotate],
        Stage::WebSocketFrame => &[Continue, Modify, Drop, Close, Annotate],
        Stage::TcpChunk => &[Continue, Modify, Hold, Drop, Close, Annotate],
        Stage::UdpDatagram => &[Continue, Modify, Drop, Reject, Annotate],
        Stage::DnsMessage => &[Continue, Modify, Respond, Reject, Annotate],
        Stage::BeforeRecord => &[Continue, Drop, Annotate],
        Stage::InspectorDataRequested => &[Continue, Annotate],
        Stage::CommandInvoked | Stage::ContextActionInvoked => &[Continue, Respond, Reject],
    }
}

/// 完整校验清单中的身份、版本、路径、模块、订阅、自描述能力、依赖和数值结构。
fn validateManifest(manifest: ExtensionManifest) -> Result<ExtensionManifest, PluginHostError> {
    if manifest.manifestVersion != EXTENSION_MANIFEST_VERSION
        || !validIdentifier(&manifest.id)
        || manifest.name.trim().is_empty()
        || manifest.description.len() > 8_192
        || !validIdentifier(&manifest.publisher)
        || Version::parse(&manifest.version).is_err()
        || !validVersionRequirement(&manifest.engines.host)
        || !validVersionRequirement(&manifest.engines.api)
        || !safeRelativePath(&manifest.runtime.entry)
        || manifest.modules.is_empty()
        || manifest.modules.len() > MAXIMUM_MODULES
    {
        return Err(PluginHostError::InvalidManifest);
    }
    // 所有生产适配器当前固定执行 v2；在没有协商器时接受其他声明会让作者按错误 ABI/JSONL 解释字节。
    if manifest.runtime.protocolVersion.as_deref() != Some(EXTENSION_RUNTIME_PROTOCOL_VERSION)
        || manifest.runtime.arguments.len() > MAXIMUM_RUNTIME_ARGUMENTS
        || manifest.runtime.arguments.iter().any(|argument| {
            argument.len() > MAXIMUM_RUNTIME_ARGUMENT_BYTES || argument.contains('\0')
        })
    {
        return Err(PluginHostError::InvalidManifest);
    }
    let mut moduleIds = std::collections::BTreeSet::new();
    for module in &manifest.modules {
        if !validIdentifier(&module.id)
            || !moduleIds.insert(module.id.as_str())
            || module.subscriptions.len() > MAXIMUM_SUBSCRIPTIONS_PER_MODULE
        {
            return Err(PluginHostError::InvalidManifest);
        }
        if module.subscriptions.is_empty()
            && !matches!(
                module.kind,
                ModuleKind::UiContribution
                    | ModuleKind::CommandProvider
                    | ModuleKind::BackgroundService
            )
        {
            return Err(PluginHostError::InvalidManifest);
        }
    }
    for (dependencyId, requirement) in &manifest.dependencies {
        if dependencyId == &manifest.id
            || !validIdentifier(dependencyId)
            || !validVersionRequirement(requirement)
        {
            return Err(PluginHostError::InvalidManifest);
        }
    }
    for path in [
        manifest.configurationSchema.as_deref(),
        manifest.contributes.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !safeRelativePath(path) {
            return Err(PluginHostError::InvalidManifest);
        }
    }
    Ok(manifest)
}

/// 校验插件和模块标识；标识会进入目录、锁文件和实例 ID，因此只接受稳定 ASCII 子集。
fn validIdentifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= MAXIMUM_IDENTIFIER_BYTES
        && identifier
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, b'.' | b'-'))
}

/// 校验语义版本范围；同时接受文档常用的空格分隔比较器并规范为 semver 的逗号形式。
fn validVersionRequirement(requirement: &str) -> bool {
    if VersionReq::parse(requirement).is_ok() {
        return true;
    }
    let normalized = requirement
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(", ");
    normalized != requirement && VersionReq::parse(&normalized).is_ok()
}

/// 校验插件包内路径；入口和 Schema 不允许绝对路径、父目录或平台设备前缀。
fn safeRelativePath(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}
