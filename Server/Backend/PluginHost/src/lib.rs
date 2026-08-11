//! 提供第三方 Native 插件的发现、生命周期与高频流量 Hook 调度。
//!
//! 本 crate 只保存插件 ABI、运行态和连接级状态，不依赖 SOCKS、HTTP 或录制模型。
//! 数据面在读取到字节后同步调用本 crate，避免 JSON、跨进程 IPC 与每包堆分配进入热路径。

#![allow(non_snake_case)]
#![allow(non_camel_case_types)]

mod developerTools;
mod extensionConfiguration;
mod extensionDataPlane;
mod extensionKernel;
mod extensionManager;
mod extensionModel;
mod hostSharedState;
mod nativeExtensionRuntime;
mod packetFilters;
mod processExtensionRuntime;
mod socks5Authentication;
mod streamTransformer;

pub use developerTools::{
    DeveloperToolError, ScaffoldOptions, ScaffoldRuntime, StageFixture, checkPluginDirectory,
    createPluginScaffold, readStageFixture, validateStageFixture, writeDeveloperSchemas,
};
pub use extensionConfiguration::{
    ExtensionConfigurationStore, PluginPlatformConfiguration, PluginUserConfiguration,
};
pub use extensionDataPlane::DataPlaneActionResult;
pub use extensionKernel::{
    DispatchFailure, DispatchResult, ExtensionInstanceSnapshot, ExtensionKernel, ExtensionRuntime,
    InvocationTrace, RuntimeInvocation,
};
pub use extensionManager::{ExtensionManager, ExtensionPackageSnapshot};
pub use extensionModel::{
    ActionKind, Capability, ContributionKind, EngineRequirements, EventEnvelope, ExtensionAction,
    ExtensionLimits, ExtensionManifest, ExtensionMatch, ExtensionModule, ExtensionRuntimeKind,
    ExtensionRuntimeManifest, FailurePolicy, InterceptionMode, ModuleKind, PluginExecutionOptions,
    Stage, StageContext, StageSubscription, availableActions,
};
pub use hostSharedState::ByteSlice;
pub use nativeExtensionRuntime::{
    NativeExtensionBuffer, NativeExtensionExports, NativeExtensionInit, NativeExtensionInitRequest,
    NativeExtensionRuntime,
};
pub use packetFilters::{
    PacketFilterAction, PacketFilterConfiguration, PacketFilterDirection, PacketFilterError,
    PacketFilterResult, PacketFilterRule, PacketFilterRuntime, PacketFilterTransport,
};
pub use processExtensionRuntime::ProcessExtensionRuntime;
pub use socks5Authentication::{Socks5AuthenticationDecision, Socks5AuthenticationRequest};
pub use streamTransformer::{
    DecodedField, DecodedFrame, StreamInput, StreamOutputFrame, StreamTransformDecision,
    StreamTransformError, StreamTransformer, StreamTransformerSession,
};

use std::{
    collections::{BTreeMap, HashSet},
    ffi::c_void,
    fs,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use libloading::Library;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};
use thiserror::Error;
use zip::ZipArchive;

use hostSharedState::{
    HostSharedState, NativeHostContext, byteSlice, hostCloseConnection, hostGetConfig,
    hostGetSessionValue, hostLog, hostSetSessionValue,
};

const PLUGIN_API_VERSION: u32 = 1;
const PLUGIN_INIT_SYMBOL: &[u8] = b"stream_plugin_init\0";
const PLUGIN_STATE_FILE_NAME: &str = "pluginState.json";
const PLUGIN_MANIFEST_FILE_NAME: &str = "plugin.json";
const PLUGIN_CONFIGURATION_FILE_NAME: &str = "config.json";
const MAXIMUM_SESSION_VALUE_BYTES: usize = 64 * 1024;
const MAXIMUM_SESSION_VALUES_PER_CONNECTION: usize = 128;
const MAXIMUM_HELD_STREAM_BYTES: usize = 256 * 1024;
const MAXIMUM_PLUGIN_PACKAGE_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_PLUGIN_PACKAGE_ENTRIES: usize = 512;
const MAXIMUM_PLUGIN_PACKAGE_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
const MAXIMUM_PLUGIN_CONFIGURATION_BYTES: usize = 256 * 1024;

/// 描述宿主可接受的 Native 插件加载、调用与配置错误。
#[derive(Debug, Error)]
pub enum PluginHostError {
    #[error("pluginDirectory")]
    Directory(#[source] std::io::Error),
    #[error("pluginState")]
    State(#[source] std::io::Error),
    #[error("pluginStateFormat")]
    StateFormat(#[source] serde_json::Error),
    #[error("pluginNotFound")]
    NotFound,
    #[error("pluginIncompatibleApiVersion")]
    IncompatibleApiVersion,
    #[error("pluginUnsupportedRuntime")]
    UnsupportedRuntime,
    #[error("pluginMissingEntry")]
    MissingEntry,
    #[error("pluginLoad")]
    Load(#[source] libloading::Error),
    #[error("pluginWorker")]
    Worker(#[source] std::io::Error),
    #[error("pluginInit")]
    Initialization,
    #[error("pluginInvalidExports")]
    InvalidExports,
    #[error("pluginManifest")]
    InvalidManifest,
    #[error("pluginConfiguration")]
    InvalidConfiguration,
    #[error("pluginPackage")]
    Package,
    #[error("pluginPackageTooLarge")]
    PackageTooLarge,
    #[error("pluginAlreadyInstalled")]
    AlreadyInstalled,
    #[error("pluginActiveConnections")]
    ActiveConnections,
}

/// 标识连接的传输层；数值属于冻结的 Native ABI，新增值必须提升 apiVersion。
#[repr(u8)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Tcp = 1,
    Udp = 2,
}

/// 标识当前字节或数据报的线上方向；数值属于冻结的 Native ABI。
#[repr(u8)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamDirection {
    ClientToServer = 1,
    ServerToClient = 2,
}

impl StreamDirection {
    /// 将固定 ABI 的方向值映射为连接内的独立暂存槽，两个方向绝不共享半包缓冲。
    const fn slot(self) -> usize {
        match self {
            Self::ClientToServer => 0,
            Self::ServerToClient => 1,
        }
    }
}

/// 标识插件对当前段的同步转发决定；UDP 不接受 Hold，调用方必须明确处理该结果。
#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookAction {
    Forward = 0,
    Hold = 1,
    Drop = 2,
    Close = 3,
}

impl HookAction {
    /// 将 ABI 返回码转换为宿主动作；未知值代表插件与宿主版本不兼容。
    fn fromRaw(value: i32) -> Result<Self, PluginHostError> {
        match value {
            0 => Ok(Self::Forward),
            1 => Ok(Self::Hold),
            2 => Ok(Self::Drop),
            3 => Ok(Self::Close),
            _ => Err(PluginHostError::InvalidExports),
        }
    }
}

/// 保存数据面创建连接时可用的稳定元数据；字节热路径只携带 `connectionId`，不复制这些字符串。
#[derive(Clone, Debug)]
pub struct ConnectionMetadata {
    pub transport: TransportKind,
    pub clientAddress: String,
    pub targetHost: String,
    pub targetPort: u16,
}

/// 表示一个已经分配的连接 Hook 上下文；调用方应在连接退出时调用 `closeConnection`。
#[derive(Clone)]
pub struct PluginConnection {
    pub connectionId: u64,
    transport: TransportKind,
    metadata: Arc<ConnectionMetadata>,
    matchedPlugins: Arc<[(String, Arc<NativePlugin>)]>,
    pendingStreams: [Arc<PendingStream>; 2],
}

impl PluginConnection {
    /// 返回连接的传输类型，供 TCP/UDP 调用方应用各自的 Hold 语义。
    pub const fn transport(&self) -> TransportKind {
        self.transport
    }
}

/// 保存某个方向被 `Hold` 的已处理字节及恢复插件下标；仅出现半包时分配，透明转发不进入锁路径。
struct PendingStream {
    available: std::sync::atomic::AtomicBool,
    value: Mutex<Option<PendingSegment>>,
}

/// 表示恢复数据链时应从哪个插件继续处理；此前插件已经处理过该段，不得重复调用。
struct PendingSegment {
    resumePluginIndex: usize,
    bytes: Vec<u8>,
}

impl PendingStream {
    /// 创建空的方向专属暂存槽；`available` 让正常直通路径跳过互斥锁。
    fn new() -> Self {
        Self {
            available: std::sync::atomic::AtomicBool::new(false),
            value: Mutex::new(None),
        }
    }

    /// 取出等待后续字节的段；连接的单方向转发任务串行调用，锁只保护关闭与恢复边界。
    fn take(&self) -> Option<PendingSegment> {
        if !self.available.load(Ordering::Acquire) {
            return None;
        }
        let value = self.value.lock().take();
        self.available.store(false, Ordering::Release);
        value
    }

    /// 保存当前半包并发布可恢复标记；写入完成后才允许下一次数据面读取该段。
    fn store(&self, segment: PendingSegment) {
        *self.value.lock() = Some(segment);
        self.available.store(true, Ordering::Release);
    }
}

/// 公开插件运行快照；配置内容和动态库绝对路径不进入控制面。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSnapshot {
    pub id: String,
    pub name: String,
    pub version: String,
    pub apiVersion: u32,
    pub runtime: PluginRuntime,
    pub hooks: Vec<PluginHook>,
    pub enabled: bool,
    pub state: PluginState,
    pub errorCode: Option<String>,
    pub activeConnections: usize,
}

/// 标识 manifest 声明的运行时；当前只加载 Native，Sidecar 保留给后续同语义实现。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginRuntime {
    Native,
    Sidecar,
}

impl Default for PluginRuntime {
    /// 空 manifest 默认声明 Native 运行时，便于 PL1 验证不含 Hook 的元数据插件。
    fn default() -> Self {
        Self::Native
    }
}

/// 标识 manifest 可声明的第一版连接 Hook；名称与设计文档保持一致。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginHook {
    OnConnectionOpen,
    OnStreamData,
    OnConnectionClose,
}

/// 描述插件当前生命周期；Failed 代表加载或调用已被熔断，后续字节不再进入该插件。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PluginState {
    Disabled,
    Enabled,
    Failed,
    Incompatible,
}

/// 描述宿主支持的声明式配置字段类型；该子集故意不支持嵌套对象和数组，保证控制面、桌面表单和 Native 配置校验语义一致。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginConfigValueType {
    String,
    Number,
    Integer,
    Boolean,
}

/// 描述一个可由桌面端渲染的插件配置字段；`format: password` 声明的值永不返回控制面。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginConfigField {
    #[serde(rename = "type")]
    pub valueType: PluginConfigValueType,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "enum")]
    pub enumValues: Vec<JsonValue>,
    #[serde(default)]
    pub default: Option<JsonValue>,
    #[serde(default)]
    pub format: String,
    #[serde(default, rename = "xAdvanced", alias = "x-advanced")]
    pub advanced: bool,
    #[serde(default)]
    pub minimum: Option<f64>,
    #[serde(default)]
    pub maximum: Option<f64>,
    #[serde(default)]
    pub minLength: Option<usize>,
    #[serde(default)]
    pub maxLength: Option<usize>,
}

impl PluginConfigField {
    /// 返回字段是否承载秘密；敏感值只写入插件目录，读取接口仅返回已配置字段名。
    fn isSecret(&self) -> bool {
        self.format == "password"
    }
}

/// 描述可自动生成设置表单的 JSON Schema 子集；字段名和约束在安装时校验，避免不可信 manifest 驱动无界 UI。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginConfigSchema {
    #[serde(rename = "type")]
    pub valueType: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub properties: BTreeMap<String, PluginConfigField>,
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub additionalProperties: bool,
}

/// 返回单插件设置页所需的公开详情；配置中的秘密字段被剥离，完整配置不会进入快照、事件流或日志。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetails {
    pub snapshot: PluginSnapshot,
    pub configSchema: Option<PluginConfigSchema>,
    pub configuration: JsonValue,
    pub configuredSecretFields: Vec<String>,
}

/// 插件包的稳定清单；未知字段拒绝，防止错拼字段被静默忽略后改变数据面行为。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginManifest {
    id: String,
    name: String,
    version: String,
    apiVersion: u32,
    #[serde(default)]
    runtime: PluginRuntime,
    #[serde(default)]
    entry: String,
    #[serde(default)]
    hooks: Vec<PluginHook>,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    streamMatch: StreamMatch,
    #[serde(default)]
    configSchema: Option<PluginConfigSchema>,
}

/// 定义当前插件可处理的连接范围；空数组表示通配，避免为未配置插件增加匹配分支。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StreamMatch {
    #[serde(default)]
    hosts: Vec<String>,
    #[serde(default)]
    ports: Vec<u16>,
    #[serde(default)]
    transports: Vec<TransportKind>,
}

impl StreamMatch {
    /// 判断连接是否命中 manifest 声明范围；主机比较使用 ASCII 不区分大小写的精确匹配或 `*`。
    fn matches(&self, metadata: &ConnectionMetadata) -> bool {
        let transportMatches =
            self.transports.is_empty() || self.transports.contains(&metadata.transport);
        let portMatches = self.ports.is_empty() || self.ports.contains(&metadata.targetPort);
        let hostMatches = self.hosts.is_empty()
            || self
                .hosts
                .iter()
                .any(|host| host == "*" || host.eq_ignore_ascii_case(&metadata.targetHost));
        transportMatches && portMatches && hostMatches
    }
}

/// 保存启停持久化状态；manifest 保持只读，用户操作不回写第三方包内容。
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginStateDocument {
    #[serde(default)]
    enabled: BTreeMap<String, bool>,
}

/// 保存单个已发现插件的运行态；动态库只在 Enabled 状态被持有。
struct ManagedPlugin {
    manifest: PluginManifest,
    directory: PathBuf,
    enabled: bool,
    state: PluginState,
    errorCode: Option<String>,
    runtime: Option<Arc<NativePlugin>>,
}

impl ManagedPlugin {
    /// 构造对外快照，屏蔽文件系统位置、配置内容与 FFI 上下文。
    fn snapshot(&self, shared: &HostSharedState) -> PluginSnapshot {
        PluginSnapshot {
            id: self.manifest.id.clone(),
            name: self.manifest.name.clone(),
            version: self.manifest.version.clone(),
            apiVersion: self.manifest.apiVersion,
            runtime: self.manifest.runtime,
            hooks: self.manifest.hooks.clone(),
            enabled: self.enabled,
            state: self.state,
            errorCode: self.errorCode.clone(),
            activeConnections: shared.pluginConnectionCount(&self.manifest.id),
        }
    }
}

/// C ABI 的连接打开事件；地址和目标字符串仅在回调同步期间有效。
#[repr(C)]
pub struct ConnectionOpenEvent {
    pub connectionId: u64,
    pub transport: TransportKind,
    pub clientAddress: ByteSlice,
    pub targetHost: ByteSlice,
    pub targetPort: u16,
}

/// C ABI 的可变数据事件；插件可以原地修改内容并缩短 `length`，不得超过 `capacity`。
#[repr(C)]
pub struct StreamDataEvent {
    pub connectionId: u64,
    pub direction: StreamDirection,
    pub data: *mut u8,
    pub length: *mut usize,
    pub capacity: usize,
}

/// C ABI 的连接关闭事件；宿主保证同一连接至多发送一次。
#[repr(C)]
pub struct ConnectionCloseEvent {
    pub connectionId: u64,
}

/// C ABI 的宿主函数表；函数表地址与 `hostContext` 在插件运行周期内稳定，函数参数中的临时字节指针仅在单次调用期间有效。
#[repr(C)]
pub struct HostFunctions {
    pub apiVersion: u32,
    pub hostContext: *mut c_void,
    pub log: Option<unsafe extern "C" fn(*mut c_void, u32, ByteSlice)>,
    pub getConfig: Option<unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> usize>,
    pub setSessionValue:
        Option<unsafe extern "C" fn(*mut c_void, u64, ByteSlice, ByteSlice) -> i32>,
    pub getSessionValue:
        Option<unsafe extern "C" fn(*mut c_void, u64, ByteSlice, *mut u8, usize) -> usize>,
    pub closeConnection: Option<unsafe extern "C" fn(*mut c_void, u64)>,
}

/// 插件初始化请求；配置字节为 `config.json` 的 UTF-8 内容，缺失时为 `{}`。
#[repr(C)]
pub struct PluginInitRequest {
    pub apiVersion: u32,
    pub configuration: ByteSlice,
}

/// 插件初始化输出；所有回调必须遵守 C ABI，context 由插件创建并由 destroy 回收。
#[repr(C)]
pub struct PluginExports {
    pub apiVersion: u32,
    pub pluginContext: *mut c_void,
    pub destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    pub onConnectionOpen:
        Option<unsafe extern "C" fn(*mut c_void, *const ConnectionOpenEvent) -> i32>,
    pub onStreamData: Option<unsafe extern "C" fn(*mut c_void, *mut StreamDataEvent) -> i32>,
    pub onConnectionClose: Option<unsafe extern "C" fn(*mut c_void, *const ConnectionCloseEvent)>,
}

/// 定义 Native 插件初始化符号；导出名称固定为 `stream_plugin_init`。
pub type PluginInit =
    unsafe extern "C" fn(*const HostFunctions, *const PluginInitRequest, *mut PluginExports) -> i32;

/// 持有动态库、宿主上下文与插件回调；字段顺序确保 destroy 在卸载动态库前执行。
struct NativePlugin {
    exports: PluginExports,
    hostFunctions: Box<HostFunctions>,
    hostContext: Box<NativeHostContext>,
    library: Library,
    enabled: std::sync::atomic::AtomicBool,
}

/// 原生插件回调在多个代理连接的 Tokio 任务中并发执行；ABI 要求插件自行保证回调实现的线程安全，
/// 宿主只在进程退出或所有连接释放后卸载动态库，因此库句柄与导出的函数指针可跨任务共享。
unsafe impl Send for NativePlugin {}

/// 原生插件回调在多个代理连接的 Tokio 任务中并发执行；ABI 要求插件自行保证回调实现的线程安全，
/// 宿主只在进程退出或所有连接释放后卸载动态库，因此库句柄与导出的函数指针可跨任务共享。
unsafe impl Sync for NativePlugin {}

impl NativePlugin {
    /// 调用插件初始化入口并验证 API 版本和所声明 Hook 的回调完整性。
    fn load(
        manifest: &PluginManifest,
        directory: &Path,
        shared: Arc<HostSharedState>,
    ) -> Result<Self, PluginHostError> {
        let entry = manifest.entry.trim();
        if entry.is_empty() {
            return Err(PluginHostError::MissingEntry);
        }
        let entryPath = directory.join(entry);
        if !entryPath.starts_with(directory) {
            return Err(PluginHostError::InvalidManifest);
        }
        let configurationValue = readPluginConfiguration(directory)?;
        validatePluginConfiguration(manifest.configSchema.as_ref(), &configurationValue)?;
        let configuration = Arc::new(
            serde_json::to_vec(&configurationValue).map_err(PluginHostError::StateFormat)?,
        );
        let library = unsafe { Library::new(entryPath) }.map_err(PluginHostError::Load)?;
        let initialize = unsafe {
            *library
                .get::<PluginInit>(PLUGIN_INIT_SYMBOL)
                .map_err(PluginHostError::Load)?
        };
        let mut hostContext = Box::new(NativeHostContext {
            pluginId: manifest.id.clone(),
            configuration: configuration.clone(),
            shared,
        });
        // 插件允许在整个运行周期保存宿主函数表指针，因此函数表必须先固定在堆上再进入初始化回调。
        // 若先传栈地址再把结构体移动进 NativePlugin，插件后续回调会解引用已经失效的初始化栈帧。
        let hostFunctions = Box::new(HostFunctions {
            apiVersion: PLUGIN_API_VERSION,
            hostContext: hostContext.as_mut() as *mut NativeHostContext as *mut c_void,
            log: Some(hostLog),
            getConfig: Some(hostGetConfig),
            setSessionValue: Some(hostSetSessionValue),
            getSessionValue: Some(hostGetSessionValue),
            closeConnection: Some(hostCloseConnection),
        });
        let initRequest = PluginInitRequest {
            apiVersion: PLUGIN_API_VERSION,
            configuration: byteSlice(&configuration),
        };
        let mut exports = PluginExports {
            apiVersion: 0,
            pluginContext: ptr::null_mut(),
            destroy: None,
            onConnectionOpen: None,
            onStreamData: None,
            onConnectionClose: None,
        };
        let status = unsafe { initialize(hostFunctions.as_ref(), &initRequest, &mut exports) };
        if status != 0 {
            return Err(PluginHostError::Initialization);
        }
        if exports.apiVersion != PLUGIN_API_VERSION {
            return Err(PluginHostError::IncompatibleApiVersion);
        }
        if !manifest.hooks.is_empty() && exports.pluginContext.is_null() {
            return Err(PluginHostError::InvalidExports);
        }
        if manifest.hooks.contains(&PluginHook::OnConnectionOpen)
            && exports.onConnectionOpen.is_none()
        {
            return Err(PluginHostError::InvalidExports);
        }
        if manifest.hooks.contains(&PluginHook::OnStreamData) && exports.onStreamData.is_none() {
            return Err(PluginHostError::InvalidExports);
        }
        if manifest.hooks.contains(&PluginHook::OnConnectionClose)
            && exports.onConnectionClose.is_none()
        {
            return Err(PluginHostError::InvalidExports);
        }
        Ok(Self {
            exports,
            hostFunctions,
            hostContext,
            library,
            enabled: std::sync::atomic::AtomicBool::new(true),
        })
    }

    /// 同步触发连接打开回调；插件返回非零动作码会被视为 ABI 错误并熔断该插件。
    fn connectionOpened(
        &self,
        connectionId: u64,
        metadata: &ConnectionMetadata,
    ) -> Result<(), PluginHostError> {
        let Some(callback) = self.exports.onConnectionOpen else {
            return Ok(());
        };
        let event = ConnectionOpenEvent {
            connectionId,
            transport: metadata.transport,
            clientAddress: byteSlice(metadata.clientAddress.as_bytes()),
            targetHost: byteSlice(metadata.targetHost.as_bytes()),
            targetPort: metadata.targetPort,
        };
        let status = unsafe { callback(self.exports.pluginContext, &event) };
        if status == 0 {
            Ok(())
        } else {
            Err(PluginHostError::Initialization)
        }
    }

    /// 同步触发数据回调；宿主在写入网络前校验插件写回的长度不超过原有缓冲容量。
    fn streamData(
        &self,
        connectionId: u64,
        direction: StreamDirection,
        buffer: &mut [u8],
    ) -> Result<(HookAction, usize), PluginHostError> {
        let Some(callback) = self.exports.onStreamData else {
            return Ok((HookAction::Forward, buffer.len()));
        };
        let mut length = buffer.len();
        let mut event = StreamDataEvent {
            connectionId,
            direction,
            data: buffer.as_mut_ptr(),
            length: &mut length,
            capacity: buffer.len(),
        };
        let action =
            HookAction::fromRaw(unsafe { callback(self.exports.pluginContext, &mut event) })?;
        if length > buffer.len() {
            return Err(PluginHostError::InvalidExports);
        }
        Ok((action, length))
    }

    /// 通知插件释放该连接对应的私有状态；关闭回调失败不得阻塞动态库析构。
    fn connectionClosed(&self, connectionId: u64) {
        if let Some(callback) = self.exports.onConnectionClose {
            let event = ConnectionCloseEvent { connectionId };
            unsafe { callback(self.exports.pluginContext, &event) };
        }
    }

    /// 关闭后续 Hook 调用；已复制到连接上下文的 Arc 会保留动态库直到当前连接结束。
    fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// 判断此运行实例是否仍接受数据面回调；该原子读取避免每包回到插件注册表加锁。
    fn isEnabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

impl Drop for NativePlugin {
    /// 先让插件销毁自身 context，再由字段析构释放动态库，避免回调地址在卸载后被调用。
    fn drop(&mut self) {
        if let Some(destroy) = self.exports.destroy {
            unsafe { destroy(self.exports.pluginContext) };
        }
        let _ = (&self.hostFunctions, &self.hostContext, &self.library);
    }
}

/// 管理用户目录中的 Native 插件，并提供不会阻塞异步数据面的同步 Hook 调度入口。
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<PluginHostInner>,
}

struct PluginHostInner {
    rootDirectory: Option<PathBuf>,
    plugins: RwLock<BTreeMap<String, ManagedPlugin>>,
    shared: Arc<HostSharedState>,
    operationLock: Mutex<()>,
    nextConnectionId: AtomicU64,
    nextExtensionEventId: AtomicU64,
    nextInstallId: AtomicU64,
    extensionConfiguration: ExtensionConfigurationStore,
    extensionKernel: ExtensionKernel,
    extensionManager: ExtensionManager,
    packetFilters: PacketFilterRuntime,
}

impl PluginHost {
    /// 创建禁用宿主供未接入插件系统的独立库路径使用；其所有 Hook 调用均为零插件透明转发。
    pub fn disabled() -> Self {
        let extensionConfiguration = ExtensionConfigurationStore::memory();
        let extensionKernel = ExtensionKernel::default();
        let extensionManager =
            ExtensionManager::memory(extensionConfiguration.clone(), extensionKernel.clone());
        Self {
            inner: Arc::new(PluginHostInner {
                rootDirectory: None,
                plugins: RwLock::new(BTreeMap::new()),
                shared: Arc::new(HostSharedState::new()),
                operationLock: Mutex::new(()),
                nextConnectionId: AtomicU64::new(1),
                nextExtensionEventId: AtomicU64::new(1),
                nextInstallId: AtomicU64::new(1),
                extensionConfiguration,
                extensionKernel,
                extensionManager,
                packetFilters: PacketFilterRuntime::default(),
            }),
        }
    }

    /// 创建并扫描用户数据目录 `plugins/`；损坏单包保留为 Failed 快照，不阻止整个代理启动。
    pub fn new(rootDirectory: impl Into<PathBuf>) -> Result<Self, PluginHostError> {
        let rootDirectory = rootDirectory.into();
        fs::create_dir_all(&rootDirectory).map_err(PluginHostError::Directory)?;
        let enabledStates = readPluginStates(&rootDirectory)?;
        let extensionConfiguration = ExtensionConfigurationStore::open(&rootDirectory)?;
        let extensionKernel = ExtensionKernel::default();
        let extensionManager = ExtensionManager::open(
            rootDirectory.clone(),
            extensionConfiguration.clone(),
            extensionKernel.clone(),
        )?;
        let host = Self {
            inner: Arc::new(PluginHostInner {
                rootDirectory: Some(rootDirectory.clone()),
                plugins: RwLock::new(BTreeMap::new()),
                shared: Arc::new(HostSharedState::new()),
                operationLock: Mutex::new(()),
                nextConnectionId: AtomicU64::new(1),
                nextExtensionEventId: AtomicU64::new(1),
                nextInstallId: AtomicU64::new(1),
                extensionConfiguration,
                extensionKernel,
                extensionManager,
                packetFilters: PacketFilterRuntime::default(),
            }),
        };
        host.discover(enabledStates)?;
        Ok(host)
    }

    /// 返回按插件 ID 稳定排序的公开快照，供控制 API 和 MCP 复用同一运行事实源。
    pub fn snapshots(&self) -> Vec<PluginSnapshot> {
        let shared = self.inner.shared.clone();
        self.inner
            .plugins
            .read()
            .values()
            .map(|plugin| plugin.snapshot(&shared))
            .collect()
    }

    /// 订阅插件公开状态变化；调用方收到通知后应重新读取完整有序快照，并自行合并短时间内的连接抖动。
    pub fn subscribeChanges(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.shared.subscribeChanges()
    }

    /// 返回完整扩展平台的用户配置存储；控制 API、CLI 和运行时必须共享这一事实源。
    pub fn extensionConfiguration(&self) -> ExtensionConfigurationStore {
        self.inner.extensionConfiguration.clone()
    }

    /// 返回宿主唯一的扩展调度内核；服务、协议、录制和控制面必须共享同一代际与执行计划。
    pub fn extensionKernel(&self) -> ExtensionKernel {
        self.inner.extensionKernel.clone()
    }

    /// 返回完整插件包和运行态管理器；控制面必须通过它原子提交配置与热替换。
    pub fn extensionManager(&self) -> ExtensionManager {
        self.inner.extensionManager.clone()
    }

    /// 返回最终写线前共享的封包滤镜；控制面热更新与全部 TCP/UDP 连接读取同一原子快照。
    pub fn packetFilters(&self) -> PacketFilterRuntime {
        self.inner.packetFilters.clone()
    }

    /// 启用一个已发现插件；空 Hook manifest 只进入 Enabled 元数据态，不加载任何第三方代码。
    pub fn enable(&self, pluginId: &str) -> Result<PluginSnapshot, PluginHostError> {
        let _operationGuard = self.inner.operationLock.lock();
        let snapshot = self.enableLocked(pluginId)?;
        self.inner.shared.notifyChanged();
        Ok(snapshot)
    }

    /// 在生命周期操作锁内启用插件；调用方必须保证配置、安装和卸载不会与本次加载并发执行。
    fn enableLocked(&self, pluginId: &str) -> Result<PluginSnapshot, PluginHostError> {
        let (manifest, directory) = {
            let plugins = self.inner.plugins.read();
            let plugin = plugins.get(pluginId).ok_or(PluginHostError::NotFound)?;
            if plugin.manifest.apiVersion != PLUGIN_API_VERSION {
                return Err(PluginHostError::IncompatibleApiVersion);
            }
            (plugin.manifest.clone(), plugin.directory.clone())
        };
        let runtime = createPluginRuntime(&manifest, &directory, self.inner.shared.clone())?;
        self.persistPluginEnabled(pluginId, true)?;
        let snapshot = {
            let mut plugins = self.inner.plugins.write();
            let plugin = plugins.get_mut(pluginId).ok_or(PluginHostError::NotFound)?;
            if let Some(previousRuntime) = plugin.runtime.take() {
                previousRuntime.disable();
            }
            plugin.enabled = true;
            plugin.state = PluginState::Enabled;
            plugin.errorCode = None;
            plugin.runtime = runtime;
            plugin.snapshot(&self.inner.shared)
        };
        Ok(snapshot)
    }

    /// 禁用插件并请求关闭其已处理的活动连接，避免 TCP Hold 状态被无声遗留。
    pub fn disable(&self, pluginId: &str) -> Result<PluginSnapshot, PluginHostError> {
        let _operationGuard = self.inner.operationLock.lock();
        let snapshot = self.disableLocked(pluginId)?;
        self.inner.shared.notifyChanged();
        Ok(snapshot)
    }

    /// 在生命周期操作锁内禁用插件；先持久化意图再解除运行时，失败时保留原运行态。
    fn disableLocked(&self, pluginId: &str) -> Result<PluginSnapshot, PluginHostError> {
        if !self.inner.plugins.read().contains_key(pluginId) {
            return Err(PluginHostError::NotFound);
        }
        self.persistPluginEnabled(pluginId, false)?;
        let snapshot = {
            let mut plugins = self.inner.plugins.write();
            let plugin = plugins.get_mut(pluginId).ok_or(PluginHostError::NotFound)?;
            if let Some(runtime) = plugin.runtime.take() {
                runtime.disable();
            }
            plugin.enabled = false;
            plugin.state = PluginState::Disabled;
            plugin.errorCode = None;
            plugin.runtime = None;
            plugin.snapshot(&self.inner.shared)
        };
        self.inner.shared.requestPluginConnectionClose(pluginId);
        Ok(snapshot)
    }

    /// 为新连接分配递增 ID、筛选已启用插件并同步发送打开事件；失败插件只影响自身并请求关闭该连接。
    pub fn openConnection(&self, metadata: ConnectionMetadata) -> PluginConnection {
        let connectionId = self.inner.nextConnectionId.fetch_add(1, Ordering::Relaxed);
        let selected = self.selectPlugins(&metadata);
        let mut matchedPlugins = Vec::with_capacity(selected.len());
        for (pluginId, plugin) in selected {
            self.inner
                .shared
                .registerConnection(&pluginId, connectionId);
            if plugin.connectionOpened(connectionId, &metadata).is_err() {
                self.markFailed(&pluginId, "pluginConnectionOpenFailed");
                self.inner
                    .shared
                    .unregisterConnection(&pluginId, connectionId);
                self.inner.shared.requestClose(connectionId);
                continue;
            }
            matchedPlugins.push((pluginId, plugin));
        }
        PluginConnection {
            connectionId,
            transport: metadata.transport,
            metadata: Arc::new(metadata),
            matchedPlugins: Arc::from(matchedPlugins),
            pendingStreams: [
                Arc::new(PendingStream::new()),
                Arc::new(PendingStream::new()),
            ],
        }
    }

    /// 顺序调用匹配插件并返回最终写入结果；直通分支只在原缓冲区原地替换或缩短，`Hold` 才进入有上限的重组缓冲。
    pub fn processStreamData(
        &self,
        connection: &PluginConnection,
        direction: StreamDirection,
        buffer: &mut [u8],
    ) -> HookActionResult {
        if self.inner.shared.takeCloseRequest(connection.connectionId) {
            return HookActionResult::Close;
        }
        let pendingStream = &connection.pendingStreams[direction.slot()];
        if let Some(pending) = pendingStream.take() {
            return self.resumePendingStream(connection, direction, buffer, pendingStream, pending);
        }
        let mut currentLength = buffer.len();
        for (pluginIndex, (pluginId, plugin)) in connection.matchedPlugins.iter().enumerate() {
            if !plugin.isEnabled() {
                return HookActionResult::Close;
            }
            let result = plugin.streamData(
                connection.connectionId,
                direction,
                &mut buffer[..currentLength],
            );
            let (action, outputLength) = match result {
                Ok(result) => result,
                Err(_) => {
                    self.markFailed(pluginId, "pluginStreamDataFailed");
                    return HookActionResult::Close;
                }
            };
            currentLength = outputLength;
            match action {
                HookAction::Forward => {}
                HookAction::Hold => {
                    if connection.transport == TransportKind::Udp {
                        return HookActionResult::Drop;
                    }
                    if currentLength > MAXIMUM_HELD_STREAM_BYTES {
                        self.markFailed(pluginId, "pluginHoldBufferExceeded");
                        return HookActionResult::Close;
                    }
                    pendingStream.store(PendingSegment {
                        resumePluginIndex: pluginIndex,
                        bytes: buffer[..currentLength].to_vec(),
                    });
                    return HookActionResult::Hold;
                }
                HookAction::Drop => return HookActionResult::Drop,
                HookAction::Close => return HookActionResult::Close,
            }
            if self.inner.shared.takeCloseRequest(connection.connectionId) {
                return HookActionResult::Close;
            }
        }
        HookActionResult::Forward {
            length: currentLength,
        }
    }

    /// 恢复同方向的半包重组链；此前插件不重复处理已暂存字节，后续插件仅在恢复插件返回 Forward 后继续执行。
    fn resumePendingStream(
        &self,
        connection: &PluginConnection,
        direction: StreamDirection,
        buffer: &mut [u8],
        pendingStream: &PendingStream,
        mut pending: PendingSegment,
    ) -> HookActionResult {
        let mut inputLength = buffer.len();
        for (pluginId, plugin) in connection
            .matchedPlugins
            .iter()
            .take(pending.resumePluginIndex)
        {
            let Some((action, outputLength)) = self.invokeStreamPlugin(
                pluginId,
                plugin,
                connection.connectionId,
                direction,
                &mut buffer[..inputLength],
            ) else {
                return HookActionResult::Close;
            };
            inputLength = outputLength;
            match action {
                HookAction::Forward => {}
                HookAction::Drop => {
                    pendingStream.store(pending);
                    return HookActionResult::Drop;
                }
                HookAction::Hold => {
                    self.markFailed(pluginId, "pluginNestedHold");
                    return HookActionResult::Close;
                }
                HookAction::Close => return HookActionResult::Close,
            }
            if self.inner.shared.takeCloseRequest(connection.connectionId) {
                return HookActionResult::Close;
            }
        }
        if pending.bytes.len().saturating_add(inputLength) > MAXIMUM_HELD_STREAM_BYTES {
            if let Some((pluginId, _)) = connection.matchedPlugins.get(pending.resumePluginIndex) {
                self.markFailed(pluginId, "pluginHoldBufferExceeded");
            }
            return HookActionResult::Close;
        }
        pending.bytes.extend_from_slice(&buffer[..inputLength]);
        let mut currentLength = pending.bytes.len();
        for (pluginIndex, (pluginId, plugin)) in connection
            .matchedPlugins
            .iter()
            .enumerate()
            .skip(pending.resumePluginIndex)
        {
            let Some((action, outputLength)) = self.invokeStreamPlugin(
                pluginId,
                plugin,
                connection.connectionId,
                direction,
                &mut pending.bytes[..currentLength],
            ) else {
                return HookActionResult::Close;
            };
            currentLength = outputLength;
            match action {
                HookAction::Forward => {}
                HookAction::Hold => {
                    pending.bytes.truncate(currentLength);
                    pending.resumePluginIndex = pluginIndex;
                    pendingStream.store(pending);
                    return HookActionResult::Hold;
                }
                HookAction::Drop => return HookActionResult::Drop,
                HookAction::Close => return HookActionResult::Close,
            }
            if self.inner.shared.takeCloseRequest(connection.connectionId) {
                return HookActionResult::Close;
            }
        }
        pending.bytes.truncate(currentLength);
        HookActionResult::ForwardOwned {
            bytes: pending.bytes,
        }
    }

    /// 执行单个原生回调并将 ABI 失败熔断为连接关闭；调用者负责处理顺序、暂存和最终转发语义。
    fn invokeStreamPlugin(
        &self,
        pluginId: &str,
        plugin: &NativePlugin,
        connectionId: u64,
        direction: StreamDirection,
        buffer: &mut [u8],
    ) -> Option<(HookAction, usize)> {
        if !plugin.isEnabled() {
            return None;
        }
        match plugin.streamData(connectionId, direction, buffer) {
            Ok(result) => Some(result),
            Err(_) => {
                self.markFailed(pluginId, "pluginStreamDataFailed");
                None
            }
        }
    }

    /// 发送关闭事件并释放宿主 session bag；即使插件先前已失败也会完成宿主侧资源回收。
    pub fn closeConnection(&self, connection: PluginConnection) {
        for (pluginId, plugin) in connection.matchedPlugins.iter() {
            if plugin.isEnabled() {
                plugin.connectionClosed(connection.connectionId);
            }
            self.inner
                .shared
                .unregisterConnection(pluginId, connection.connectionId);
        }
        self.inner.shared.takeCloseRequest(connection.connectionId);
    }

    /// 返回对指定插件的公开快照，便于控制面在状态变更后只返回受影响资源。
    pub fn snapshot(&self, pluginId: &str) -> Option<PluginSnapshot> {
        self.inner
            .plugins
            .read()
            .get(pluginId)
            .map(|plugin| plugin.snapshot(&self.inner.shared))
    }

    /// 返回单个插件的可编辑公开详情；秘密字段仅以已配置字段名呈现，调用方不能通过读取接口恢复原值。
    pub fn details(&self, pluginId: &str) -> Result<PluginDetails, PluginHostError> {
        let (manifest, directory, snapshot) = {
            let plugins = self.inner.plugins.read();
            let plugin = plugins.get(pluginId).ok_or(PluginHostError::NotFound)?;
            (
                plugin.manifest.clone(),
                plugin.directory.clone(),
                plugin.snapshot(&self.inner.shared),
            )
        };
        let configuration = readPluginConfiguration(&directory)?;
        let (configuration, configuredSecretFields) =
            publicPluginConfiguration(manifest.configSchema.as_ref(), &configuration);
        Ok(PluginDetails {
            snapshot,
            configSchema: manifest.configSchema,
            configuration,
            configuredSecretFields,
        })
    }

    /// 保存插件配置并在插件已启用时重新初始化运行时；旧连接会收到关闭请求，新连接只使用已校验的新配置。
    pub fn updateConfiguration(
        &self,
        pluginId: &str,
        update: JsonValue,
    ) -> Result<PluginDetails, PluginHostError> {
        let _operationGuard = self.inner.operationLock.lock();
        let (manifest, directory, enabled) = {
            let plugins = self.inner.plugins.read();
            let plugin = plugins.get(pluginId).ok_or(PluginHostError::NotFound)?;
            (
                plugin.manifest.clone(),
                plugin.directory.clone(),
                plugin.enabled,
            )
        };
        let current = readPluginConfiguration(&directory)?;
        let configuration =
            mergeSecretConfiguration(manifest.configSchema.as_ref(), &current, update);
        validatePluginConfiguration(manifest.configSchema.as_ref(), &configuration)?;
        let previousFile = readPluginConfigurationFile(&directory)?;
        writePluginConfiguration(&directory, &configuration)?;
        if enabled && let Err(error) = self.reloadLocked(pluginId) {
            restorePluginConfigurationFile(&directory, previousFile)?;
            return Err(error);
        }
        let details = self.details(pluginId)?;
        self.inner.shared.notifyChanged();
        Ok(details)
    }

    /// 使用磁盘中当前 manifest 与配置重新创建插件运行时；用于开发调试和手动替换包后恢复稳定的回调边界。
    pub fn reload(&self, pluginId: &str) -> Result<PluginSnapshot, PluginHostError> {
        let _operationGuard = self.inner.operationLock.lock();
        let snapshot = self.reloadLocked(pluginId)?;
        self.inner.shared.notifyChanged();
        Ok(snapshot)
    }

    /// 安装根目录包含 plugin.json 的 .tplugin.zip；压缩包在隔离暂存目录完整校验后一次性进入插件目录。
    pub fn installPackage(&self, package: &[u8]) -> Result<PluginSnapshot, PluginHostError> {
        if package.is_empty() || package.len() > MAXIMUM_PLUGIN_PACKAGE_BYTES {
            return Err(PluginHostError::PackageTooLarge);
        }
        let _operationGuard = self.inner.operationLock.lock();
        let rootDirectory = self
            .inner
            .rootDirectory
            .as_ref()
            .ok_or(PluginHostError::Package)?;
        let mut archive =
            ZipArchive::new(Cursor::new(package)).map_err(|_| PluginHostError::Package)?;
        if archive.len() > MAXIMUM_PLUGIN_PACKAGE_ENTRIES {
            return Err(PluginHostError::PackageTooLarge);
        }
        let manifest = readPackageManifest(&mut archive)?;
        if self.inner.plugins.read().contains_key(&manifest.id)
            || rootDirectory.join(&manifest.id).exists()
        {
            return Err(PluginHostError::AlreadyInstalled);
        }
        let installId = self.inner.nextInstallId.fetch_add(1, Ordering::Relaxed);
        let stageDirectory = rootDirectory.join(format!(".{}.install.{installId}", manifest.id));
        fs::create_dir(&stageDirectory).map_err(PluginHostError::Directory)?;
        let result = extractPackage(&mut archive, &stageDirectory).and_then(|_| {
            let extractedManifest = readPluginManifest(&stageDirectory)?;
            if extractedManifest.id != manifest.id {
                return Err(PluginHostError::Package);
            }
            let _configuration = readPluginConfiguration(&stageDirectory)?;
            let destination = rootDirectory.join(&extractedManifest.id);
            if destination.exists() {
                return Err(PluginHostError::AlreadyInstalled);
            }
            fs::rename(&stageDirectory, &destination).map_err(PluginHostError::Directory)?;
            let state = if extractedManifest.apiVersion == PLUGIN_API_VERSION {
                PluginState::Disabled
            } else {
                PluginState::Incompatible
            };
            let snapshot = {
                let mut plugins = self.inner.plugins.write();
                let pluginId = extractedManifest.id.clone();
                plugins.insert(
                    pluginId.clone(),
                    ManagedPlugin {
                        manifest: extractedManifest,
                        directory: destination,
                        enabled: false,
                        state,
                        errorCode: None,
                        runtime: None,
                    },
                );
                plugins
                    .get(&pluginId)
                    .ok_or(PluginHostError::Package)?
                    .snapshot(&self.inner.shared)
            };
            Ok(snapshot)
        });
        if result.is_err() && stageDirectory.exists() {
            fs::remove_dir_all(&stageDirectory).map_err(PluginHostError::Directory)?;
        }
        if result.is_ok() {
            self.inner.shared.notifyChanged();
        }
        result
    }

    /// 卸载未处理活动连接的插件包；运行中的 Native DLL 不允许强删，调用方应先禁用并等待连接数归零。
    pub fn uninstall(&self, pluginId: &str) -> Result<(), PluginHostError> {
        let _operationGuard = self.inner.operationLock.lock();
        if self.inner.shared.pluginConnectionCount(pluginId) > 0 {
            self.inner.shared.requestPluginConnectionClose(pluginId);
            return Err(PluginHostError::ActiveConnections);
        }
        let mut plugin = self
            .inner
            .plugins
            .write()
            .remove(pluginId)
            .ok_or(PluginHostError::NotFound)?;
        let enabledStates = self.currentEnabledStates();
        if let Err(error) = self.persistEnabledStates(enabledStates) {
            self.inner
                .plugins
                .write()
                .insert(pluginId.to_owned(), plugin);
            return Err(error);
        }
        if let Some(runtime) = plugin.runtime.take() {
            runtime.disable();
            drop(runtime);
        }
        plugin.enabled = false;
        plugin.state = PluginState::Disabled;
        if let Err(error) =
            fs::remove_dir_all(&plugin.directory).map_err(PluginHostError::Directory)
        {
            self.inner
                .plugins
                .write()
                .insert(pluginId.to_owned(), plugin);
            let _ = self.persistEnabledStates(self.currentEnabledStates());
            return Err(error);
        }
        self.inner.shared.notifyChanged();
        Ok(())
    }

    /// 在生命周期操作锁内重建已启用插件；加载新运行时成功前绝不替换旧运行时，保证失败不影响现有连接。
    fn reloadLocked(&self, pluginId: &str) -> Result<PluginSnapshot, PluginHostError> {
        let (manifest, directory, enabled) = {
            let plugins = self.inner.plugins.read();
            let plugin = plugins.get(pluginId).ok_or(PluginHostError::NotFound)?;
            (
                plugin.manifest.clone(),
                plugin.directory.clone(),
                plugin.enabled,
            )
        };
        if !enabled {
            return self.snapshot(pluginId).ok_or(PluginHostError::NotFound);
        }
        let runtime = createPluginRuntime(&manifest, &directory, self.inner.shared.clone())?;
        let snapshot = {
            let mut plugins = self.inner.plugins.write();
            let plugin = plugins.get_mut(pluginId).ok_or(PluginHostError::NotFound)?;
            if let Some(previousRuntime) = std::mem::replace(&mut plugin.runtime, runtime) {
                previousRuntime.disable();
            }
            plugin.state = PluginState::Enabled;
            plugin.errorCode = None;
            plugin.snapshot(&self.inner.shared)
        };
        self.inner.shared.requestPluginConnectionClose(pluginId);
        Ok(snapshot)
    }

    /// 扫描插件目录并把启用状态恢复到内存；自动加载失败转为 Failed，不让损坏 DLL 中断控制服务启动。
    fn discover(&self, enabledStates: PluginStateDocument) -> Result<(), PluginHostError> {
        let Some(rootDirectory) = self.inner.rootDirectory.as_ref() else {
            return Ok(());
        };
        let entries = fs::read_dir(rootDirectory).map_err(PluginHostError::Directory)?;
        for entry in entries {
            let entry = entry.map_err(PluginHostError::Directory)?;
            let fileType = entry.file_type().map_err(PluginHostError::Directory)?;
            if !fileType.is_dir() {
                continue;
            }
            let directory = entry.path();
            let manifestPath = directory.join(PLUGIN_MANIFEST_FILE_NAME);
            if !manifestPath.is_file() {
                continue;
            }
            let manifestBytes = fs::read(&manifestPath).map_err(PluginHostError::Directory)?;
            // 完整 manifest 由 ExtensionManager 处理；legacy 扫描不得把同一目录伪报为损坏旧插件。
            if extensionManager::isCompleteManifest(&manifestBytes) {
                continue;
            }
            let manifestResult = serde_json::from_slice::<PluginManifest>(&manifestBytes)
                .map_err(PluginHostError::StateFormat)
                .and_then(validateManifest);
            match manifestResult {
                Ok(manifest) => {
                    let enabled = enabledStates
                        .enabled
                        .get(&manifest.id)
                        .copied()
                        .unwrap_or(false);
                    let state = if manifest.apiVersion == PLUGIN_API_VERSION {
                        PluginState::Disabled
                    } else {
                        PluginState::Incompatible
                    };
                    let pluginId = manifest.id.clone();
                    self.inner.plugins.write().insert(
                        pluginId.clone(),
                        ManagedPlugin {
                            manifest,
                            directory,
                            enabled: false,
                            state,
                            errorCode: None,
                            runtime: None,
                        },
                    );
                    if enabled && let Err(error) = self.enable(&pluginId) {
                        self.markFailed(&pluginId, errorCode(&error));
                    }
                }
                Err(_) => {
                    let directoryName = directory
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("invalidPlugin")
                        .to_owned();
                    let pluginId = format!("invalid.{directoryName}");
                    self.inner
                        .plugins
                        .write()
                        .insert(pluginId.clone(), invalidManagedPlugin(directory, pluginId));
                }
            }
        }
        Ok(())
    }

    /// 按 manifest priority 与 ID 稳定排序选择当前连接插件；读锁只用于复制 Arc，不跨 FFI 回调持有。
    fn selectPlugins(&self, metadata: &ConnectionMetadata) -> Vec<(String, Arc<NativePlugin>)> {
        let plugins = self.inner.plugins.read();
        let mut selected: Vec<_> = plugins
            .iter()
            .filter_map(|(pluginId, plugin)| {
                (plugin.enabled
                    && plugin.state == PluginState::Enabled
                    && plugin.manifest.streamMatch.matches(metadata))
                .then(|| {
                    plugin
                        .runtime
                        .clone()
                        .map(|runtime| (plugin.manifest.priority, pluginId.clone(), runtime))
                })
                .flatten()
            })
            .collect();
        selected.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        selected
            .into_iter()
            .map(|(_, pluginId, runtime)| (pluginId, runtime))
            .collect()
    }

    /// 熔断回调失败插件并标记其活动连接关闭；Native 回调失败不能继续处理后续字节。
    fn markFailed(&self, pluginId: &str, code: &str) {
        if let Some(plugin) = self.inner.plugins.write().get_mut(pluginId) {
            if let Some(runtime) = plugin.runtime.take() {
                runtime.disable();
            }
            plugin.state = PluginState::Failed;
            plugin.errorCode = Some(code.to_owned());
        }
        self.inner.shared.requestPluginConnectionClose(pluginId);
        self.inner.shared.notifyChanged();
    }

    /// 先持久化单个插件的目标启停状态，再更新内存运行态，避免磁盘写入失败时返回与实际状态相反的控制结果。
    fn persistPluginEnabled(&self, pluginId: &str, enabled: bool) -> Result<(), PluginHostError> {
        let mut enabledStates = self.currentEnabledStates();
        if !enabledStates.contains_key(pluginId) {
            return Err(PluginHostError::NotFound);
        }
        enabledStates.insert(pluginId.to_owned(), enabled);
        self.persistEnabledStates(enabledStates)
    }

    /// 从当前受管插件生成持久化启停视图；调用方在移除插件前后复用它，避免状态文件保留失效 ID。
    fn currentEnabledStates(&self) -> BTreeMap<String, bool> {
        self.inner
            .plugins
            .read()
            .iter()
            .map(|(pluginId, plugin)| (pluginId.clone(), plugin.enabled))
            .collect()
    }

    /// 原子替换启停状态文件；所有生命周期操作都在操作锁内调用，临时文件只在本次写入窗口存在。
    fn persistEnabledStates(
        &self,
        enabledStates: BTreeMap<String, bool>,
    ) -> Result<(), PluginHostError> {
        let Some(rootDirectory) = self.inner.rootDirectory.as_ref() else {
            return Ok(());
        };
        let document = PluginStateDocument {
            enabled: enabledStates,
        };
        let content = serde_json::to_vec_pretty(&document).map_err(PluginHostError::StateFormat)?;
        replacePluginFile(rootDirectory, PLUGIN_STATE_FILE_NAME, &content).map_err(|error| {
            match error {
                PluginHostError::Directory(source) => PluginHostError::State(source),
                other => other,
            }
        })
    }
}

/// 根据 manifest 创建当前运行时；纯元数据插件不加载第三方库，Native Hook 插件则在此处完成 ABI 与配置校验。
fn createPluginRuntime(
    manifest: &PluginManifest,
    directory: &Path,
    shared: Arc<HostSharedState>,
) -> Result<Option<Arc<NativePlugin>>, PluginHostError> {
    // 空 Hook 插件也可能声明必填配置；启用前统一验证磁盘配置，避免元数据插件绕过同一份 manifest 契约。
    let configuration = readPluginConfiguration(directory)?;
    validatePluginConfiguration(manifest.configSchema.as_ref(), &configuration)?;
    if manifest.hooks.is_empty() {
        return Ok(None);
    }
    if manifest.runtime != PluginRuntime::Native {
        return Err(PluginHostError::UnsupportedRuntime);
    }
    Ok(Some(Arc::new(NativePlugin::load(
        manifest, directory, shared,
    )?)))
}

/// 读取并校验插件根目录中的 manifest；该入口供启动扫描和压缩包暂存目录复用，字段漂移统一映射为 manifest 错误。
fn readPluginManifest(directory: &Path) -> Result<PluginManifest, PluginHostError> {
    let manifestPath = directory.join(PLUGIN_MANIFEST_FILE_NAME);
    let bytes = fs::read(manifestPath).map_err(PluginHostError::Directory)?;
    serde_json::from_slice::<PluginManifest>(&bytes)
        .map_err(|_| PluginHostError::InvalidManifest)
        .and_then(validateManifest)
}

/// 读取现有 config.json 的原始字节；更新失败时使用它恢复精确的用户配置格式与内容。
fn readPluginConfigurationFile(directory: &Path) -> Result<Option<Vec<u8>>, PluginHostError> {
    let path = directory.join(PLUGIN_CONFIGURATION_FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }
    fs::read(path).map(Some).map_err(PluginHostError::Directory)
}

/// 将已校验 JSON 配置写入受管插件目录；临时文件只存在于替换窗口，避免写入中断留下半截 JSON。
fn writePluginConfiguration(
    directory: &Path,
    configuration: &JsonValue,
) -> Result<(), PluginHostError> {
    let content = serde_json::to_vec_pretty(configuration).map_err(PluginHostError::StateFormat)?;
    if content.len() > MAXIMUM_PLUGIN_CONFIGURATION_BYTES {
        return Err(PluginHostError::InvalidConfiguration);
    }
    replacePluginFile(directory, PLUGIN_CONFIGURATION_FILE_NAME, &content)
}

/// 恢复配置更新前的原始文件；加载新运行时失败时调用，确保磁盘配置与仍在运行的旧插件保持一致。
fn restorePluginConfigurationFile(
    directory: &Path,
    previous: Option<Vec<u8>>,
) -> Result<(), PluginHostError> {
    match previous {
        Some(content) => replacePluginFile(directory, PLUGIN_CONFIGURATION_FILE_NAME, &content),
        None => {
            let path = directory.join(PLUGIN_CONFIGURATION_FILE_NAME);
            if path.exists() {
                fs::remove_file(path).map_err(PluginHostError::Directory)?;
            }
            Ok(())
        }
    }
}

/// 用同目录同步临时文件原子替换一个插件私有文件；仅受管常量文件名调用本函数。
///
/// 运行上下文：配置和启停状态都必须先持久化再发布内存。参数 `directory` 与 `fileName`
/// 只来自宿主管理目录和常量；创建、同步或原子替换失败时保留旧权威文件并返回目录错误。
fn replacePluginFile(
    directory: &Path,
    fileName: &str,
    content: &[u8],
) -> Result<(), PluginHostError> {
    let destination = directory.join(fileName);
    let temporary = directory.join(format!(".{fileName}.pending"));
    {
        let mut temporaryFile = fs::File::create(&temporary).map_err(PluginHostError::Directory)?;
        temporaryFile
            .write_all(content)
            .map_err(PluginHostError::Directory)?;
        temporaryFile
            .sync_all()
            .map_err(PluginHostError::Directory)?;
    }
    replacePluginFileName(&temporary, &destination).map_err(PluginHostError::Directory)
}

/// 在 Windows 上以写穿透语义替换目标文件；失败时原目标仍是权威版本。
#[cfg(windows)]
fn replacePluginFileName(temporary: &Path, destination: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let temporaryWide = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destinationWide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(temporaryWide.as_ptr()),
            PCWSTR(destinationWide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(std::io::Error::other)
    }
}

/// 在 Unix 上替换目标文件并同步父目录，确保进程重启后仍能观察到新的目录项。
#[cfg(unix)]
fn replacePluginFileName(temporary: &Path, destination: &Path) -> Result<(), std::io::Error> {
    fs::rename(temporary, destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("插件配置目标缺少父目录"))?;
    fs::File::open(parent)?.sync_all()
}

/// 从压缩包根读取 plugin.json；安装格式固定为根 manifest，拒绝多层目录和多个候选清单带来的歧义。
fn readPackageManifest(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
) -> Result<PluginManifest, PluginHostError> {
    let mut entry = archive
        .by_name(PLUGIN_MANIFEST_FILE_NAME)
        .map_err(|_| PluginHostError::Package)?;
    if entry.is_dir() || entry.size() > 64 * 1024 {
        return Err(PluginHostError::Package);
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut bytes)
        .map_err(|_| PluginHostError::Package)?;
    serde_json::from_slice::<PluginManifest>(&bytes)
        .map_err(|_| PluginHostError::Package)
        .and_then(validateManifest)
}

/// 将压缩包安全解压到已创建的暂存目录；路径、条目数和总展开大小均在写盘前受限，避免路径穿越与压缩炸弹。
fn extractPackage(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    stageDirectory: &Path,
) -> Result<(), PluginHostError> {
    let mut unpackedBytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| PluginHostError::Package)?;
        let relativePath = packageEntryPath(entry.name())?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(PluginHostError::Package);
        }
        if entry.is_dir() {
            fs::create_dir_all(stageDirectory.join(relativePath))
                .map_err(PluginHostError::Directory)?;
            continue;
        }
        unpackedBytes = unpackedBytes
            .checked_add(entry.size())
            .ok_or(PluginHostError::PackageTooLarge)?;
        if unpackedBytes > MAXIMUM_PLUGIN_PACKAGE_UNPACKED_BYTES {
            return Err(PluginHostError::PackageTooLarge);
        }
        let destination = stageDirectory.join(relativePath);
        let parent = destination.parent().ok_or(PluginHostError::Package)?;
        fs::create_dir_all(parent).map_err(PluginHostError::Directory)?;
        let mut output = fs::File::create(destination).map_err(PluginHostError::Directory)?;
        std::io::copy(&mut entry, &mut output).map_err(PluginHostError::Directory)?;
    }
    Ok(())
}

/// 将压缩包条目解析为受控相对路径；绝对路径、空路径和任何导航分量都会在创建文件前拒绝。
fn packageEntryPath(name: &str) -> Result<PathBuf, PluginHostError> {
    let path = Path::new(name);
    let valid = !name.is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    valid
        .then(|| path.to_path_buf())
        .ok_or(PluginHostError::Package)
}

/// 描述 Hook 处理后的转发结果；透明路径借用调用方缓冲，半包重组完成时才返回独占字节，避免每段分配。
#[derive(Debug, Eq, PartialEq)]
pub enum HookActionResult {
    Forward { length: usize },
    ForwardOwned { bytes: Vec<u8> },
    Hold,
    Drop,
    Close,
}

/// 验证外部 manifest 的稳定标识和版本字段，拒绝路径分隔符与空字符串进入用户目录映射。
fn validateManifest(manifest: PluginManifest) -> Result<PluginManifest, PluginHostError> {
    let idValid = !manifest.id.is_empty()
        && manifest.id.len() <= 128
        && manifest
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
    if !idValid || manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err(PluginHostError::InvalidManifest);
    }
    if Path::new(&manifest.entry).is_absolute()
        || manifest
            .entry
            .split(['/', '\\'])
            .any(|segment| segment == "..")
    {
        return Err(PluginHostError::InvalidManifest);
    }
    if let Some(schema) = manifest.configSchema.as_ref() {
        validateConfigurationSchema(schema)?;
    }
    Ok(manifest)
}

/// 校验可视化配置 Schema 的结构；宿主只接收可直接生成表单的标量子集，避免第三方清单把复杂递归模型带入控制面。
fn validateConfigurationSchema(schema: &PluginConfigSchema) -> Result<(), PluginHostError> {
    if schema.valueType != "object"
        || schema.properties.len() > 64
        || schema.additionalProperties
        || schema.title.len() > 160
        || schema.description.len() > 1_024
    {
        return Err(PluginHostError::InvalidManifest);
    }
    let mut requiredFields = HashSet::new();
    for requiredField in &schema.required {
        if !schema.properties.contains_key(requiredField) || !requiredFields.insert(requiredField) {
            return Err(PluginHostError::InvalidManifest);
        }
    }
    for (name, field) in &schema.properties {
        let validName = !name.is_empty()
            && name.len() <= 96
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
        let stringBoundsValid = match field.valueType {
            PluginConfigValueType::String => {
                field.minLength.unwrap_or(0) <= field.maxLength.unwrap_or(usize::MAX)
            }
            _ => field.minLength.is_none() && field.maxLength.is_none(),
        };
        let numericBoundsValid = match field.valueType {
            PluginConfigValueType::Number | PluginConfigValueType::Integer => {
                field.minimum.unwrap_or(f64::NEG_INFINITY) <= field.maximum.unwrap_or(f64::INFINITY)
            }
            _ => field.minimum.is_none() && field.maximum.is_none(),
        };
        if !validName
            || field.title.len() > 160
            || field.description.len() > 1_024
            || !stringBoundsValid
            || !numericBoundsValid
            || (!field.format.is_empty() && field.format != "password")
            || field
                .enumValues
                .iter()
                .any(|value| !configurationValueHasType(value, field.valueType))
            || field
                .default
                .as_ref()
                .is_some_and(|value| !validateConfigurationValue(field, value))
        {
            return Err(PluginHostError::InvalidManifest);
        }
    }
    Ok(())
}

/// 判断 JSON 值是否符合单个声明字段的基本类型；数值边界和枚举约束由 validateConfigurationValue 继续验证。
fn configurationValueHasType(value: &JsonValue, valueType: PluginConfigValueType) -> bool {
    match valueType {
        PluginConfigValueType::String => value.is_string(),
        PluginConfigValueType::Number => value.is_number(),
        PluginConfigValueType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        PluginConfigValueType::Boolean => value.is_boolean(),
    }
}

/// 验证配置值是否满足字段的类型、枚举和范围约束；该逻辑同时约束安装时默认值与每次控制面配置更新。
fn validateConfigurationValue(field: &PluginConfigField, value: &JsonValue) -> bool {
    if !configurationValueHasType(value, field.valueType) {
        return false;
    }
    if !field.enumValues.is_empty() && !field.enumValues.contains(value) {
        return false;
    }
    match field.valueType {
        PluginConfigValueType::String => {
            let length = value.as_str().map_or(0, str::len);
            length >= field.minLength.unwrap_or(0)
                && length <= field.maxLength.unwrap_or(usize::MAX)
        }
        PluginConfigValueType::Number | PluginConfigValueType::Integer => {
            let Some(number) = value.as_f64() else {
                return false;
            };
            number >= field.minimum.unwrap_or(f64::NEG_INFINITY)
                && number <= field.maximum.unwrap_or(f64::INFINITY)
        }
        PluginConfigValueType::Boolean => true,
    }
}

/// 校验待持久化的插件配置；无 Schema 的插件仍要求根值为对象，Schema 插件还必须拒绝未知字段和遗漏必填字段。
fn validatePluginConfiguration(
    schema: Option<&PluginConfigSchema>,
    configuration: &JsonValue,
) -> Result<(), PluginHostError> {
    let object = configuration
        .as_object()
        .ok_or(PluginHostError::InvalidConfiguration)?;
    let Some(schema) = schema else {
        return Ok(());
    };
    if object
        .keys()
        .any(|name| !schema.properties.contains_key(name))
        || schema
            .required
            .iter()
            .any(|name| !object.contains_key(name))
        || object.iter().any(|(name, value)| {
            schema
                .properties
                .get(name)
                .is_some_and(|field| !validateConfigurationValue(field, value))
        })
    {
        return Err(PluginHostError::InvalidConfiguration);
    }
    Ok(())
}

/// 合并未在更新请求中出现的秘密字段；浏览器不读取秘密原值，因此留空提交必须保留磁盘中的既有配置。
fn mergeSecretConfiguration(
    schema: Option<&PluginConfigSchema>,
    current: &JsonValue,
    update: JsonValue,
) -> JsonValue {
    let Some(schema) = schema else {
        return update;
    };
    let (Some(currentObject), Some(mut updateObject)) =
        (current.as_object(), update.as_object().cloned())
    else {
        return update;
    };
    for (name, field) in &schema.properties {
        if field.isSecret()
            && !updateObject.contains_key(name)
            && let Some(value) = currentObject.get(name)
        {
            updateObject.insert(name.clone(), value.clone());
        }
    }
    JsonValue::Object(updateObject)
}

/// 将内部配置转为控制面安全视图；秘密字段只返回“已配置”标记，不向 UI、日志或事件流回显原始值。
fn publicPluginConfiguration(
    schema: Option<&PluginConfigSchema>,
    configuration: &JsonValue,
) -> (JsonValue, Vec<String>) {
    let Some(schema) = schema else {
        return (configuration.clone(), Vec::new());
    };
    let Some(configurationObject) = configuration.as_object() else {
        return (JsonValue::Object(JsonMap::new()), Vec::new());
    };
    let mut publicObject = configurationObject.clone();
    let mut configuredSecretFields = Vec::new();
    for (name, field) in &schema.properties {
        if field.isSecret() && publicObject.remove(name).is_some() {
            configuredSecretFields.push(name.clone());
        }
    }
    (JsonValue::Object(publicObject), configuredSecretFields)
}

/// 创建损坏 manifest 的占位插件，保证列表能反馈目录故障而不泄露文件内容。
fn invalidManagedPlugin(directory: PathBuf, pluginId: String) -> ManagedPlugin {
    ManagedPlugin {
        manifest: PluginManifest {
            id: pluginId,
            name: "无效插件".to_owned(),
            version: String::new(),
            apiVersion: 0,
            runtime: PluginRuntime::Native,
            entry: String::new(),
            hooks: Vec::new(),
            priority: 0,
            streamMatch: StreamMatch::default(),
            configSchema: None,
        },
        directory,
        enabled: false,
        state: PluginState::Failed,
        errorCode: Some("pluginManifest".to_owned()),
        runtime: None,
    }
}

/// 读取用户启停状态；缺失文件表示首次启动，格式错误必须显式失败而不猜测用户选择。
fn readPluginStates(rootDirectory: &Path) -> Result<PluginStateDocument, PluginHostError> {
    let path = rootDirectory.join(PLUGIN_STATE_FILE_NAME);
    if !path.exists() {
        return Ok(PluginStateDocument::default());
    }
    let bytes = fs::read(path).map_err(PluginHostError::State)?;
    serde_json::from_slice(&bytes).map_err(PluginHostError::StateFormat)
}

/// 读取插件私有 JSON 配置；缺失配置等价于空对象，格式、大小或根类型不正确时拒绝加载。
fn readPluginConfiguration(directory: &Path) -> Result<JsonValue, PluginHostError> {
    let path = directory.join(PLUGIN_CONFIGURATION_FILE_NAME);
    if !path.exists() {
        return Ok(JsonValue::Object(JsonMap::new()));
    }
    let bytes = fs::read(path).map_err(PluginHostError::Directory)?;
    if bytes.len() > MAXIMUM_PLUGIN_CONFIGURATION_BYTES {
        return Err(PluginHostError::InvalidConfiguration);
    }
    let configuration =
        serde_json::from_slice::<JsonValue>(&bytes).map_err(PluginHostError::StateFormat)?;
    configuration
        .is_object()
        .then_some(configuration)
        .ok_or(PluginHostError::InvalidConfiguration)
}

/// 返回 ABI 错误的稳定机器码，控制面不需要依赖底层动态库或文件系统诊断。
fn errorCode(error: &PluginHostError) -> &'static str {
    match error {
        PluginHostError::Directory(_) => "pluginDirectory",
        PluginHostError::State(_) | PluginHostError::StateFormat(_) => "pluginState",
        PluginHostError::NotFound => "pluginNotFound",
        PluginHostError::IncompatibleApiVersion => "pluginIncompatibleApiVersion",
        PluginHostError::UnsupportedRuntime => "pluginUnsupportedRuntime",
        PluginHostError::MissingEntry => "pluginMissingEntry",
        PluginHostError::Load(_) => "pluginLoad",
        PluginHostError::Worker(_) => "pluginWorker",
        PluginHostError::Initialization => "pluginInit",
        PluginHostError::InvalidExports => "pluginInvalidExports",
        PluginHostError::InvalidManifest => "pluginManifest",
        PluginHostError::InvalidConfiguration => "pluginConfiguration",
        PluginHostError::Package => "pluginPackage",
        PluginHostError::PackageTooLarge => "pluginPackageTooLarge",
        PluginHostError::AlreadyInstalled => "pluginAlreadyInstalled",
        PluginHostError::ActiveConnections => "pluginActiveConnections",
    }
}
