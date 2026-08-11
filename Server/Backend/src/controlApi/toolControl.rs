use std::{path::Path, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State, rejection::JsonRejection},
    http::{
        HeaderValue,
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use capture_core::{
    HarExportRequest, RecordingRuleConfiguration, RecordingRuleRuntime, RecordingSession,
};
use http_proxy_core::{
    AutoSaveConfiguration, AutoSavePublicState, AutoSaveTool, BlockCookiesConfiguration,
    BlockCookiesTool, BlockListConfiguration, BlockListTool, BreakpointError,
    BreakpointsConfiguration, BreakpointsTool, DnsSpoofingConfiguration, DnsSpoofingTool,
    EditableHttpMessage, MapLocalConfiguration, MapLocalTool, MapRemoteConfiguration,
    MapRemoteTool, MirrorConfiguration, MirrorPublicState, MirrorTool, NoCachingConfiguration,
    NoCachingTool, RecordingRulesTool, RewriteConfiguration, RewriteTool, ThrottlingConfiguration,
    ThrottlingTool, ToolPipeline,
};
use plugin_host::{PacketFilterConfiguration, PacketFilterRuntime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ApiError, ControlState, ErrorCode, EventMessage, LocalizedApiError};
use crate::localization::RequestLocale;

// 公开顺序只列出当前已注册且可通过控制 API 配置的工具。规划中的工具若提前出现在快照，
// 客户端会把它们渲染成可操作能力，但对应 GET/PUT 必然返回 toolNotFound，形成虚假入口。
const pipelineOrder: [&str; 13] = [
    "recordingRules",
    "dnsSpoofing",
    "blockList",
    "noCaching",
    "blockCookies",
    "mapRemote",
    "mapLocal",
    "rewrite",
    "breakpoints",
    "throttling",
    "mirror",
    "autoSave",
    "packetFilters",
];

/// 聚合 M3 工具的公开状态；该结构与 Web/MCP 的单一快照契约保持一致，不携带断点草稿正文。
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsPublicState {
    pipelineOrder: Vec<String>,
    recordingRules: RecordingRuleConfiguration,
    packetFilters: PacketFilterConfiguration,
    blockList: BlockListConfiguration,
    noCaching: NoCachingConfiguration,
    blockCookies: BlockCookiesConfiguration,
    dnsSpoofing: DnsSpoofingConfiguration,
    mapLocal: MapLocalConfiguration,
    mapRemote: MapRemoteConfiguration,
    rewrite: RewriteConfiguration,
    breakpoints: BreakpointsConfiguration,
    throttling: http_proxy_core::ThrottlingPublicState,
    mirror: MirrorPublicState,
    autoSave: AutoSavePublicState,
    suspendedBreakpointCount: usize,
}

/// 保存所有工具的可恢复配置；运行计数、错误和断点队列不进入配置文件。
///
/// 运行上下文：控制器启动时先从统一配置文件读取该结构，再构造共享数据面工具；每次热更新也会
/// 先生成完整候选快照，避免只持久化单个工具后覆盖其它规则。
/// 失败语义：反序列化或任一工具语义校验失败会阻止控制器启动，禁止用默认值静默覆盖用户规则。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub(super) struct PersistedToolsConfiguration {
    recordingRules: RecordingRuleConfiguration,
    packetFilters: PacketFilterConfiguration,
    blockList: BlockListConfiguration,
    noCaching: NoCachingConfiguration,
    blockCookies: BlockCookiesConfiguration,
    dnsSpoofing: DnsSpoofingConfiguration,
    mapLocal: MapLocalConfiguration,
    mapRemote: MapRemoteConfiguration,
    rewrite: RewriteConfiguration,
    breakpoints: BreakpointsConfiguration,
    throttling: ThrottlingConfiguration,
    mirror: MirrorConfiguration,
    autoSave: AutoSaveConfiguration,
}

impl PersistedToolsConfiguration {
    /// 克隆录制会话启动前需要的规则配置；返回值与随后构造 ToolRuntime 的持久快照来自同一文件版本。
    pub(super) fn recordingRules(&self) -> RecordingRuleConfiguration {
        self.recordingRules.clone()
    }
}

/// 管理所有可热更新工具实例及其固定流水线注册关系；配置更新只替换实例内快照，不重启监听器。
#[derive(Clone)]
pub(super) struct ToolRuntime {
    pipeline: ToolPipeline,
    recordingRules: RecordingRuleRuntime,
    packetFilters: PacketFilterRuntime,
    blockList: Arc<BlockListTool>,
    noCaching: Arc<NoCachingTool>,
    blockCookies: Arc<BlockCookiesTool>,
    dnsSpoofing: Arc<DnsSpoofingTool>,
    mapLocal: Arc<MapLocalTool>,
    mapRemote: Arc<MapRemoteTool>,
    rewrite: Arc<RewriteTool>,
    breakpoints: Arc<BreakpointsTool>,
    throttling: Arc<ThrottlingTool>,
    mirror: Arc<MirrorTool>,
    autoSave: Arc<AutoSaveTool>,
    updateLock: Arc<parking_lot::Mutex<()>>,
}

impl ToolRuntime {
    /// 按默认安全配置创建全量工具链；Map Local 根目录来自用户数据目录，规则只能在该根内解析相对路径。
    pub(super) fn new(
        mappingRoot: &Path,
        recording: RecordingSession,
        packetFilters: PacketFilterRuntime,
        configuration: PersistedToolsConfiguration,
    ) -> Result<Self, String> {
        let recordingRules = recording.recordingRules();
        recordingRules
            .replaceConfiguration(configuration.recordingRules.clone())
            .map_err(|error| format!("recordingRules:{error}"))?;
        packetFilters
            .replaceConfiguration(configuration.packetFilters.clone())
            .map_err(|error| format!("packetFilters:{error}"))?;
        let blockList = Arc::new(
            BlockListTool::new(configuration.blockList)
                .map_err(|error| format!("blockList:{}", error.code()))?,
        );
        let noCaching = Arc::new(
            NoCachingTool::new(configuration.noCaching)
                .map_err(|error| format!("noCaching:{}", error.code()))?,
        );
        let blockCookies = Arc::new(
            BlockCookiesTool::new(configuration.blockCookies)
                .map_err(|error| format!("blockCookies:{}", error.code()))?,
        );
        let dnsSpoofing = Arc::new(
            DnsSpoofingTool::new(configuration.dnsSpoofing)
                .map_err(|error| format!("dnsSpoofing:{error}"))?,
        );
        let mapLocal = Arc::new(
            MapLocalTool::new(configuration.mapLocal, mappingRoot)
                .map_err(|error| format!("mapLocal:{}", error.code()))?,
        );
        let mapRemote = Arc::new(
            MapRemoteTool::new(configuration.mapRemote)
                .map_err(|error| format!("mapRemote:{}", error.code()))?,
        );
        let rewrite = Arc::new(
            RewriteTool::new(configuration.rewrite)
                .map_err(|error| format!("rewrite:{}", error.code()))?,
        );
        let breakpoints = Arc::new(
            BreakpointsTool::new(configuration.breakpoints)
                .map_err(|error| format!("breakpoints:{}", error.code()))?,
        );
        let throttling = Arc::new(
            ThrottlingTool::new(configuration.throttling)
                .map_err(|error| format!("throttling:{}", error.code()))?,
        );
        let mirror = Arc::new(
            MirrorTool::new(configuration.mirror)
                .map_err(|error| format!("mirror:{}", error.code()))?,
        );
        let autoSave = Arc::new(
            AutoSaveTool::new(configuration.autoSave, recording)
                .map_err(|error| format!("autoSave:{}", error.code()))?,
        );
        let pipeline = ToolPipeline::new();
        pipeline
            .register(Arc::new(RecordingRulesTool::new(recordingRules.clone())))
            .map_err(|error| format!("pipeline:{error}"))?;
        for tool in [
            blockList.clone() as Arc<dyn http_proxy_core::PipelineTool>,
            noCaching.clone() as Arc<dyn http_proxy_core::PipelineTool>,
            blockCookies.clone() as Arc<dyn http_proxy_core::PipelineTool>,
            mapRemote.clone() as Arc<dyn http_proxy_core::PipelineTool>,
            mapLocal.clone() as Arc<dyn http_proxy_core::PipelineTool>,
            rewrite.clone() as Arc<dyn http_proxy_core::PipelineTool>,
            breakpoints.clone() as Arc<dyn http_proxy_core::PipelineTool>,
            throttling.clone() as Arc<dyn http_proxy_core::PipelineTool>,
            mirror.clone() as Arc<dyn http_proxy_core::PipelineTool>,
        ] {
            pipeline
                .register(tool)
                .map_err(|error| format!("pipeline:{error}"))?;
        }
        Ok(Self {
            pipeline,
            recordingRules,
            packetFilters,
            blockList,
            noCaching,
            blockCookies,
            dnsSpoofing,
            mapLocal,
            mapRemote,
            rewrite,
            breakpoints,
            throttling,
            mirror,
            autoSave,
            updateLock: Arc::new(parking_lot::Mutex::new(())),
        })
    }

    /// 返回与数据面共享的流水线句柄；克隆仅增加共享引用，不复制规则或暂停队列。
    pub(super) fn pipeline(&self) -> ToolPipeline {
        self.pipeline.clone()
    }

    /// 返回 Map Local 的受管文件根目录；浏览器导入端点只在该目录内落盘，失败时不暴露进程工作目录。
    pub(super) fn mapLocalMappingRoot(&self) -> &Path {
        self.mapLocal.mappingRoot()
    }

    /// 返回数据面共享的 DNS 映射器；克隆只增加引用计数，热更新后所有监听器立即读取新快照。
    pub(super) fn dnsSpoofing(&self) -> Arc<DnsSpoofingTool> {
        self.dnsSpoofing.clone()
    }

    /// 生成控制快照中的工具总览；调用期间只读取各工具的短暂配置快照。
    pub(super) fn publicState(&self) -> ToolsPublicState {
        ToolsPublicState {
            pipelineOrder: pipelineOrder
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            recordingRules: self.recordingRules.configuration(),
            packetFilters: self.packetFilters.configuration(),
            blockList: self.blockList.configuration(),
            noCaching: self.noCaching.configuration(),
            blockCookies: self.blockCookies.configuration(),
            dnsSpoofing: self.dnsSpoofing.configuration(),
            mapLocal: self.mapLocal.configuration(),
            mapRemote: self.mapRemote.configuration(),
            rewrite: self.rewrite.configuration(),
            breakpoints: self.breakpoints.configuration(),
            throttling: self.throttling.publicState(),
            mirror: self.mirror.publicState(),
            autoSave: self.autoSave.publicState(),
            suspendedBreakpointCount: self.breakpoints.suspendedBreakpoints().len(),
        }
    }

    /// 克隆全部可持久化工具配置；运行统计和队列状态刻意排除在外。
    pub(super) fn persistedConfiguration(&self) -> PersistedToolsConfiguration {
        PersistedToolsConfiguration {
            recordingRules: self.recordingRules.configuration(),
            packetFilters: self.packetFilters.configuration(),
            blockList: self.blockList.configuration(),
            noCaching: self.noCaching.configuration(),
            blockCookies: self.blockCookies.configuration(),
            dnsSpoofing: self.dnsSpoofing.configuration(),
            mapLocal: self.mapLocal.configuration(),
            mapRemote: self.mapRemote.configuration(),
            rewrite: self.rewrite.configuration(),
            breakpoints: self.breakpoints.configuration(),
            throttling: self.throttling.configuration(),
            mirror: self.mirror.configuration(),
            autoSave: self.autoSave.configuration(),
        }
    }

    /// 返回指定工具配置；响应只用于单工具读取，工具总览仍以 snapshot.tools 为权威来源。
    pub(super) fn configuration(&self, toolId: &str) -> Result<Value, ToolControlError> {
        let value = match toolId {
            "recordingRules" => serde_json::to_value(self.recordingRules.configuration()),
            "packetFilters" => serde_json::to_value(self.packetFilters.configuration()),
            "blockList" => serde_json::to_value(self.blockList.configuration()),
            "noCaching" => serde_json::to_value(self.noCaching.configuration()),
            "blockCookies" => serde_json::to_value(self.blockCookies.configuration()),
            "dnsSpoofing" => serde_json::to_value(self.dnsSpoofing.configuration()),
            "mapLocal" => serde_json::to_value(self.mapLocal.configuration()),
            "mapRemote" => serde_json::to_value(self.mapRemote.configuration()),
            "rewrite" => serde_json::to_value(self.rewrite.configuration()),
            "breakpoints" => serde_json::to_value(self.breakpoints.configuration()),
            "throttling" => serde_json::to_value(self.throttling.publicState()),
            "mirror" => serde_json::to_value(self.mirror.publicState()),
            "autoSave" => serde_json::to_value(self.autoSave.publicState()),
            _ => return Err(ToolControlError::UnknownTool),
        };
        value.map_err(|_| ToolControlError::Operation)
    }

    /// 验证并热替换单个工具配置；同一时刻仅允许一个更新，避免两个控制客户端交叉覆盖规则集。
    pub(super) fn update<Persist>(
        &self,
        toolId: &str,
        configuration: Value,
        persist: Persist,
    ) -> Result<ToolsPublicState, ToolControlError>
    where
        Persist: FnOnce(PersistedToolsConfiguration) -> Result<(), std::io::Error>,
    {
        let _updateGuard = self.updateLock.lock();
        let persistedConfiguration = self.prepareUpdate(toolId, configuration.clone())?;
        // 磁盘快照先于运行时发布；候选已用与数据面相同的校验器验证，因此成功返回时两侧必然一致。
        persist(persistedConfiguration).map_err(|_| ToolControlError::Persistence)?;
        match toolId {
            "recordingRules" => self.replaceRecordingRules(configuration)?,
            "packetFilters" => self.replacePacketFilters(configuration)?,
            "blockList" => self.replaceBlockList(configuration)?,
            "noCaching" => self.replaceNoCaching(configuration)?,
            "blockCookies" => self.replaceBlockCookies(configuration)?,
            "dnsSpoofing" => self.replaceDnsSpoofing(configuration)?,
            "mapLocal" => self.replaceMapLocal(configuration)?,
            "mapRemote" => self.replaceMapRemote(configuration)?,
            "rewrite" => self.replaceRewrite(configuration)?,
            "breakpoints" => self.replaceBreakpoints(configuration)?,
            "throttling" => self.replaceThrottling(configuration)?,
            "mirror" => self.replaceMirror(configuration)?,
            "autoSave" => self.replaceAutoSave(configuration)?,
            _ => return Err(ToolControlError::UnknownTool),
        }
        Ok(self.publicState())
    }

    /// 解析并校验单工具候选，然后生成包含其它工具现值的全量持久化快照。
    fn prepareUpdate(
        &self,
        toolId: &str,
        configuration: Value,
    ) -> Result<PersistedToolsConfiguration, ToolControlError> {
        let mut candidate = self.persistedConfiguration();
        match toolId {
            "recordingRules" => {
                let configuration: RecordingRuleConfiguration = decodeConfiguration(configuration)?;
                RecordingRuleRuntime::new(configuration.clone())
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.recordingRules = configuration;
            }
            "packetFilters" => {
                let configuration: PacketFilterConfiguration = decodeConfiguration(configuration)?;
                PacketFilterRuntime::new(configuration.clone())
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.packetFilters = configuration;
            }
            "blockList" => {
                let configuration: BlockListConfiguration = decodeConfiguration(configuration)?;
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.blockList = configuration;
            }
            "noCaching" => {
                let configuration: NoCachingConfiguration = decodeConfiguration(configuration)?;
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.noCaching = configuration;
            }
            "blockCookies" => {
                let configuration: BlockCookiesConfiguration = decodeConfiguration(configuration)?;
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.blockCookies = configuration;
            }
            "dnsSpoofing" => {
                let configuration: DnsSpoofingConfiguration = decodeConfiguration(configuration)?;
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.dnsSpoofing = configuration;
            }
            "mapLocal" => {
                let configuration: MapLocalConfiguration = decodeConfiguration(configuration)?;
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.mapLocal = configuration;
            }
            "mapRemote" => {
                let configuration: MapRemoteConfiguration = decodeConfiguration(configuration)?;
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.mapRemote = configuration;
            }
            "rewrite" => {
                let configuration: RewriteConfiguration = decodeConfiguration(configuration)?;
                RewriteTool::validate(&configuration)
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.rewrite = configuration;
            }
            "breakpoints" => {
                let configuration: BreakpointsConfiguration = decodeConfiguration(configuration)?;
                BreakpointsTool::validate(&configuration)
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.breakpoints = configuration;
            }
            "throttling" => {
                let preservesUserPresets = configuration
                    .as_object()
                    .is_some_and(|object| !object.contains_key("userPresets"));
                let mut configuration: ThrottlingConfiguration =
                    decodeConfiguration(configuration)?;
                if preservesUserPresets {
                    configuration.userPresets = candidate.throttling.userPresets;
                }
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.throttling = configuration;
            }
            "mirror" => {
                let configuration: MirrorConfiguration = decodeConfiguration(configuration)?;
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.mirror = configuration;
            }
            "autoSave" => {
                let configuration: AutoSaveConfiguration = decodeConfiguration(configuration)?;
                configuration
                    .validate()
                    .map_err(|_| ToolControlError::InvalidConfiguration)?;
                candidate.autoSave = configuration;
            }
            _ => return Err(ToolControlError::UnknownTool),
        }
        Ok(candidate)
    }

    /// 返回当前暂停断点的稳定快照；草稿只经该专用端点和 WebSocket 断点事件传递。
    pub(super) fn suspendedBreakpoints(&self) -> Vec<http_proxy_core::SuspendedBreakpoint> {
        self.breakpoints.suspendedBreakpoints()
    }

    /// 订阅暂停队列版本变化；控制层收到变化后重新读取完整队列并向 UI 推送一致快照。
    pub(super) fn subscribeSuspendedChanges(&self) -> tokio::sync::watch::Receiver<u64> {
        self.breakpoints.subscribeSuspendedChanges()
    }

    /// 校验草稿并恢复指定暂停事务；草稿无效时保留原暂停项供用户修正后再次提交。
    pub(super) fn continueBreakpoint(
        &self,
        transactionId: &str,
        draft: EditableHttpMessage,
    ) -> Result<(), ToolControlError> {
        self.breakpoints
            .continueBreakpoint(transactionId, draft)
            .map_err(mapBreakpointError)
    }

    /// 中止指定暂停事务并释放其队列槽位；流水线将向请求方返回确定性的失败响应。
    pub(super) fn abortBreakpoint(&self, transactionId: &str) -> Result<(), ToolControlError> {
        self.breakpoints
            .abortBreakpoint(transactionId)
            .map_err(mapBreakpointError)
    }

    /// 在服务停止前解除全部暂停项，保证监听器关闭不会遗留永久等待的连接任务。
    pub(super) fn releaseBreakpoints(&self) -> usize {
        self.breakpoints.releaseAll()
    }

    /// 停止服务前等待镜像队列排空；超时由控制层转为稳定停止失败而不会丢失已提交写入状态。
    pub(super) async fn flushMirror(&self) -> Result<(), ToolControlError> {
        self.mirror
            .flush(std::time::Duration::from_secs(5))
            .await
            .map_err(|_| ToolControlError::Operation)
    }

    /// 立即保存当前录制会话；该入口与控制 API 和 MCP 共用，避免复制一套导出与轮转逻辑。
    pub(super) async fn saveNow(
        &self,
        recording: &RecordingSession,
    ) -> Result<AutoSavePublicState, ToolControlError> {
        self.autoSave
            .saveNow(recording)
            .await
            .map_err(|_| ToolControlError::Operation)
    }

    /// 解析并替换访问列表配置，解析或语义校验失败均不改变当前运行状态。
    fn replaceBlockList(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.blockList
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换无缓存配置，响应与请求方向的头部策略由工具内部保持原子一致。
    fn replaceNoCaching(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.noCaching
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换 Cookie 剥离配置，两个方向的开关统一作为同一份热更新快照提交。
    fn replaceBlockCookies(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.blockCookies
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并原子替换 DNS 映射规则；非法主机模式或 IP 不得影响当前生效配置。
    fn replaceDnsSpoofing(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.dnsSpoofing
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换本地映射规则；路径边界由 MapLocalTool 在更新前完整校验。
    fn replaceMapLocal(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.mapLocal
            .updateConfiguration(configuration)
            .map(|_| ())
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换远程映射规则；原始 URL 记录逻辑始终保留在流水线上下文中。
    fn replaceMapRemote(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.mapRemote
            .updateConfiguration(configuration)
            .map(|_| ())
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换预编译重写规则；正则或 HTTP 字段非法时拒绝整份配置而非部分生效。
    fn replaceRewrite(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.rewrite
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换断点规则；关闭总开关时工具会主动释放已有暂停项。
    fn replaceBreakpoints(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.breakpoints
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换节流配置；Web 的只读预设字段不会覆盖服务端保存的用户预设。
    fn replaceThrottling(&self, configuration: Value) -> Result<(), ToolControlError> {
        let preservesUserPresets = configuration
            .as_object()
            .is_some_and(|object| !object.contains_key("userPresets"));
        let mut configuration: ThrottlingConfiguration = decodeConfiguration(configuration)?;
        if preservesUserPresets {
            configuration.userPresets = self.throttling.configuration().userPresets;
        }
        self.throttling
            .updateConfiguration(configuration)
            .map(|_| ())
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换镜像配置；目录和队列边界在工具内部完整校验，失败不影响当前写入器。
    fn replaceMirror(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.mirror
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并替换自动保存配置；后台调度器读取同一配置锁，更新无需重启代理。
    fn replaceAutoSave(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.autoSave
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并原子替换录制规则；已有连接沿用建立时裁决，新事务立即读取新快照。
    fn replaceRecordingRules(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.recordingRules
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }

    /// 解析并原子替换最终写线滤镜；现有连接的下一块 TCP/UDP 数据立即读取新快照。
    fn replacePacketFilters(&self, configuration: Value) -> Result<(), ToolControlError> {
        let configuration = decodeConfiguration(configuration)?;
        self.packetFilters
            .replaceConfiguration(configuration)
            .map_err(|_| ToolControlError::InvalidConfiguration)
    }
}

/// 归类控制层可恢复的工具操作失败，避免把规则正文、文件路径或断点草稿写入外部错误响应。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolControlError {
    UnknownTool,
    InvalidConfiguration,
    BreakpointNotFound,
    Operation,
    Persistence,
}

/// 统一解析未知工具 JSON；所有工具配置均启用 deny_unknown_fields，避免协议漂移被静默吞掉。
fn decodeConfiguration<T>(configuration: Value) -> Result<T, ToolControlError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(configuration).map_err(|_| ToolControlError::InvalidConfiguration)
}

/// 将断点工具的错误压缩为控制 API 的稳定语义，找不到暂停项与草稿/调度失败保持可区分。
fn mapBreakpointError(error: BreakpointError) -> ToolControlError {
    match error {
        // 接收端已关闭表示承载暂停报文的代理任务已结束，控制面语义与过期/不存在一致；
        // 将其归为 404 可避免把正常连接取消误报为服务端操作故障。
        BreakpointError::NotFound | BreakpointError::ResolutionClosed => {
            ToolControlError::BreakpointNotFound
        }
        _ => ToolControlError::Operation,
    }
}

impl ControlState {
    /// 返回工具总览并以单独事件通知所有已连接控制客户端；工具配置更新不影响正在转发的旧请求。
    fn updateTool(&self, toolId: &str, configuration: Value) -> Result<ToolsPublicState, ApiError> {
        let tools = self
            .tools
            .update(toolId, configuration, |persistedConfiguration| {
                self.processSelection
                    .replaceToolsConfiguration(persistedConfiguration)
            })
            .map_err(mapToolUpdateError)?;
        let eventTools = tools.clone();
        self.publishRevisioned(|serverInstanceId, revision| EventMessage::Tools {
            serverInstanceId,
            revision,
            tools: Box::new(eventTools),
        });
        Ok(tools)
    }

    /// 返回单工具公开配置；读取操作不会增加全局 revision，也不会改变运行中的数据面。
    fn toolConfiguration(&self, toolId: &str) -> Result<Value, ApiError> {
        self.tools.configuration(toolId).map_err(mapToolReadError)
    }

    /// 返回断点队列完整快照；该端点是读取可编辑报文草稿的唯一控制边界。
    fn suspendedBreakpoints(&self) -> Vec<http_proxy_core::SuspendedBreakpoint> {
        self.tools.suspendedBreakpoints()
    }

    /// 恢复指定断点并立即发布刷新后的队列；发送端关闭等调度错误不会泄露草稿内容。
    fn continueBreakpoint(
        &self,
        transactionId: &str,
        draft: EditableHttpMessage,
    ) -> Result<(), ApiError> {
        self.tools
            .continueBreakpoint(transactionId, draft)
            .map_err(mapBreakpointOperationError)?;
        self.publishBreakpointQueue();
        Ok(())
    }

    /// 中止指定断点并立即发布刷新后的队列；调用完成后该暂停槽位不再占用代理资源。
    fn abortBreakpoint(&self, transactionId: &str) -> Result<(), ApiError> {
        self.tools
            .abortBreakpoint(transactionId)
            .map_err(mapBreakpointOperationError)?;
        self.publishBreakpointQueue();
        Ok(())
    }

    /// 发布当前断点队列；自动监视任务和显式 continue/abort 均复用同一事件生成路径。
    pub(super) fn publishBreakpointQueue(&self) {
        let suspended = self.tools.suspendedBreakpoints();
        self.publishRevisioned(|serverInstanceId, revision| EventMessage::Breakpoints {
            serverInstanceId,
            revision,
            suspended,
        });
    }

    /// 获取共享工具流水线供 HTTP 监听器在启动时注入；同一对象跨服务启停保持配置和断点队列身份。
    pub(super) fn toolPipeline(&self) -> ToolPipeline {
        self.tools.pipeline()
    }

    /// 在服务生命周期结束前中止所有暂停连接并向界面广播空队列，避免端点停止后仍显示过期草稿。
    pub(super) fn releaseBreakpointQueue(&self) {
        if self.tools.releaseBreakpoints() > 0 {
            self.publishBreakpointQueue();
        }
    }

    /// 手动触发一次自动保存并发布工具状态；导出由 AutoSaveTool 串行化，不会阻塞 HTTP 数据面。
    async fn saveAutoSave(&self) -> Result<AutoSavePublicState, ApiError> {
        let state = self
            .tools
            .saveNow(&self.recording)
            .await
            .map_err(mapToolUpdateError)?;
        let tools = self.tools.publicState();
        self.publishRevisioned(|serverInstanceId, revision| EventMessage::Tools {
            serverInstanceId,
            revision,
            tools: Box::new(tools),
        });
        Ok(state)
    }

    /// 停止数据面前排空镜像写队列；失败返回稳定控制错误，调用方仍可获得监听器停止诊断。
    pub(super) async fn flushMirrorWrites(&self) -> Result<(), ApiError> {
        self.tools.flushMirror().await.map_err(mapToolUpdateError)
    }
}

/// 将工具更新失败映射到更新端点的精确 HTTP 语义；无效配置保留 400，未知工具保留 404。
fn mapToolUpdateError(error: ToolControlError) -> ApiError {
    match error {
        ToolControlError::UnknownTool => ApiError::notFound(ErrorCode::ToolNotFound),
        ToolControlError::InvalidConfiguration => {
            ApiError::badRequest(ErrorCode::InvalidToolConfiguration)
        }
        ToolControlError::BreakpointNotFound => ApiError::notFound(ErrorCode::BreakpointNotFound),
        ToolControlError::Operation => ApiError::internal(ErrorCode::ToolOperationFailed),
        ToolControlError::Persistence => {
            ApiError::internal(ErrorCode::ConfigurationPersistenceFailed)
        }
    }
}

/// 将单工具读取失败映射为稳定 404/500，读取端点不会尝试修复或创建未知工具。
fn mapToolReadError(error: ToolControlError) -> ApiError {
    match error {
        ToolControlError::UnknownTool => ApiError::notFound(ErrorCode::ToolNotFound),
        _ => ApiError::internal(ErrorCode::ToolOperationFailed),
    }
}

/// 将断点完成或中止失败映射到专用错误码，调用方能够区分过期事务与运行期调度异常。
fn mapBreakpointOperationError(error: ToolControlError) -> ApiError {
    match error {
        ToolControlError::BreakpointNotFound => ApiError::notFound(ErrorCode::BreakpointNotFound),
        _ => ApiError::internal(ErrorCode::BreakpointOperationFailed),
    }
}

/// 约束 HAR 导出请求；格式明确写出以避免未来新增格式时静默改变浏览器下载语义。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HarExportApiRequest {
    format: HarExportFormat,
    includeBodies: bool,
    #[serde(default)]
    transactionIds: Vec<String>,
}

/// 当前只开放 HAR 1.2 导出；新格式必须以新枚举值和独立测试显式加入。
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum HarExportFormat {
    Har,
}

/// 将 M3 工具与导出端点附加到统一控制路由，所有处理器共享相同 ControlState 和本地化错误边界。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router
        .route("/api/v1/tools/{toolId}", get(getTool).put(updateTool))
        .route("/api/v1/tools/autoSave/saveNow", post(saveAutoSave))
        .route(
            "/api/v1/breakpoints/suspended",
            get(listSuspendedBreakpoints),
        )
        .route(
            "/api/v1/breakpoints/suspended/{transactionId}/continue",
            post(continueBreakpoint),
        )
        .route(
            "/api/v1/breakpoints/suspended/{transactionId}/abort",
            post(abortBreakpoint),
        )
        .route("/api/v1/recording/export", post(exportHar))
}

/// 返回指定工具的公开配置；节流响应包含只读预设目录，便于客户端选择而不重复维护内置值。
async fn getTool(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    AxumPath(toolId): AxumPath<String>,
) -> Result<Json<Value>, LocalizedApiError> {
    state
        .toolConfiguration(&toolId)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 校验并热更新指定工具；请求语法、未知字段及工具语义失败都使用本地化结构化错误响应。
async fn updateTool(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    AxumPath(toolId): AxumPath<String>,
    updateResult: Result<Json<Value>, JsonRejection>,
) -> Result<Json<ToolsPublicState>, LocalizedApiError> {
    let Json(configuration) = updateResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidToolRequest).withLocale(locale))?;
    state
        .updateTool(&toolId, configuration)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 立即导出当前录制会话；成功返回最新自动保存状态，失败使用同一组本地化工具错误契约。
async fn saveAutoSave(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<AutoSavePublicState>, LocalizedApiError> {
    state
        .saveAutoSave()
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 返回当前全部暂停断点；队列按事务标识稳定排序，便于多客户端对比增量事件。
async fn listSuspendedBreakpoints(
    State(state): State<ControlState>,
) -> Json<Vec<http_proxy_core::SuspendedBreakpoint>> {
    Json(state.suspendedBreakpoints())
}

/// 使用校验后的可编辑草稿继续指定断点；成功响应为空，以契合前端和 MCP 的无正文完成契约。
async fn continueBreakpoint(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    AxumPath(transactionId): AxumPath<String>,
    draftResult: Result<Json<EditableHttpMessage>, JsonRejection>,
) -> Result<StatusNoContent, LocalizedApiError> {
    let Json(draft) = draftResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidToolRequest).withLocale(locale))?;
    state
        .continueBreakpoint(&transactionId, draft)
        .map(|_| StatusNoContent)
        .map_err(|error| error.withLocale(locale))
}

/// 中止指定断点；成功后代理任务立刻解除等待并向客户端返回由数据面生成的失败响应。
async fn abortBreakpoint(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    AxumPath(transactionId): AxumPath<String>,
) -> Result<StatusNoContent, LocalizedApiError> {
    state
        .abortBreakpoint(&transactionId)
        .map(|_| StatusNoContent)
        .map_err(|error| error.withLocale(locale))
}

/// 导出当前或选中事务的 HAR 1.2 文档；正文只在请求明确要求时读取，避免普通导出放大磁盘 I/O。
async fn exportHar(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    requestResult: Result<Json<HarExportApiRequest>, JsonRejection>,
) -> Result<Response, LocalizedApiError> {
    let Json(request) = requestResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidExportRequest).withLocale(locale))?;
    let HarExportFormat::Har = request.format;
    let archive = buildHar(&state.recording, request)
        .await
        .map_err(|error| error.withLocale(locale))?;
    let bytes = serde_json::to_vec(&archive)
        .map_err(|_| ApiError::internal(ErrorCode::ExportOperationFailed).withLocale(locale))?;
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/har+json"),
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"recording.har\""),
    );
    Ok(response)
}

/// 委托 capture-core 构建 HAR，并把事务不存在与导出运行失败映射到控制层稳定错误码。
async fn buildHar(
    recording: &RecordingSession,
    request: HarExportApiRequest,
) -> Result<capture_core::HarArchive, ApiError> {
    recording
        .buildHarExport(HarExportRequest {
            includeBodies: request.includeBodies,
            transactionIds: request.transactionIds,
        })
        .await
        .map_err(|error| match error {
            capture_core::HarExportError::Capture(
                capture_core::CaptureError::TransactionNotFound,
            ) => ApiError::notFound(ErrorCode::TransactionNotFound),
            _ => ApiError::internal(ErrorCode::ExportOperationFailed),
        })
}

/// 表示 204 无正文响应；显式类型避免 Axum 在断点完成接口中意外序列化 null。
struct StatusNoContent;

impl IntoResponse for StatusNoContent {
    /// 返回固定 204 响应，不携带 JSON 或文本正文，匹配客户端 requestEmpty 的协议约束。
    fn into_response(self) -> Response {
        axum::http::StatusCode::NO_CONTENT.into_response()
    }
}
