//! 提供运行中进程发现、按可执行文件路径持久化选择以及捕获配置热更新。
//!
//! PID 只在单次进程生命周期内有效，因此控制面保存稳定的可执行文件路径，并在每次读取或服务启动时
//! 重新解析当前 PID。这样应用重启后无需用户重新添加，同时不会把已经失效的 PID 写回 WinDivert 规则。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use capture_core::{RecordingSnapshot, RecordingState};
use http_proxy_core::{AuxiliaryListenerConfiguration, SslMitmConfiguration};
use location_core::LocationPattern;
use parking_lot::{Mutex, RwLock};
use process_capture_core::ProcessCaptureConfiguration;
use serde::{Deserialize, Serialize};
use sysinfo::System;

use super::{
    ApiError, ConfigurationUpdate, ControlState, ErrorCode, EventMessage, LocalizedApiError,
    ServiceState, mcpControl::McpConfiguration, processIcon::extractProcessIcon,
    protocolControl::PersistedProtocolConfiguration, toolControl::PersistedToolsConfiguration,
};
use crate::localization::RequestLocale;

const applicationConfigurationFileName: &str = "configuration.json";
const retiredRecordingRulesKey: &str = "recordingRules";

/// 描述一个当前可选择的运行中进程；同一路径的多个实例保留各自 PID，便于界面展示实际运行状态。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessCandidate {
    pub processId: u32,
    pub name: String,
    pub executablePath: String,
}

/// 返回进程选择器所需的完整视图；selectedPaths 即使对应程序当前未运行也会保留。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSelectionSnapshot {
    pub enabled: bool,
    pub selectedPaths: Vec<String>,
    pub resolvedProcessIds: Vec<u32>,
    pub processes: Vec<ProcessCandidate>,
    pub processIcons: BTreeMap<String, String>,
}

/// 接收进程管理页提交的稳定路径集合；PID 始终由后端从实时进程表解析，客户端不能伪造。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessSelectionUpdate {
    pub enabled: bool,
    pub selectedPaths: Vec<String>,
}

/// 持久化格式只保存跨重启稳定的数据，不保存短生命周期 PID 或 WinDivert 内部端口。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedProcessSelection {
    enabled: bool,
    selectedPaths: Vec<String>,
}

/// 保存跨控制进程稳定的录制偏好；事务、计数和会话标识属于运行态，不写入配置文件。
///
/// 运行上下文：该结构只承载控制面允许热更新的字段，录制资源上限由完整抓包契约固定，避免
/// 用户配置重新引入正文裁剪。反序列化失败会阻止启动，禁止使用损坏规则继续录制。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PersistedRecordingConfiguration {
    pub state: RecordingState,
    pub ignoreLocations: Vec<LocationPattern>,
    pub recordTunnelMetadata: bool,
}

impl Default for PersistedRecordingConfiguration {
    /// 返回首次运行的录制偏好；默认立即录制、不过滤目标并保留隧道元数据。
    fn default() -> Self {
        Self {
            state: RecordingState::Recording,
            ignoreLocations: Vec::new(),
            recordTunnelMetadata: true,
        }
    }
}

impl PersistedRecordingConfiguration {
    /// 从权威录制快照构造完整持久化候选；调用方在写盘前已完成规则语义校验。
    pub(super) fn fromSnapshot(snapshot: &RecordingSnapshot) -> Self {
        Self {
            state: snapshot.state,
            ignoreLocations: snapshot.ignoreLocations.clone(),
            recordTunnelMetadata: snapshot.recordTunnelMetadata,
        }
    }
}

/// 统一配置文件根结构；服务设置与进程路径在同一次写入中保持一致，后续配置域可向此结构追加。
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
struct PersistedApplicationConfiguration {
    service: Option<ConfigurationUpdate>,
    processSelection: PersistedProcessSelection,
    recording: PersistedRecordingConfiguration,
    auxiliaryListeners: AuxiliaryListenerConfiguration,
    ssl: SslMitmConfiguration,
    tools: PersistedToolsConfiguration,
    protocols: PersistedProtocolConfiguration,
    mcp: McpConfiguration,
}

/// 维护统一持久化状态；更新锁串行化“克隆快照、原子写盘、发布内存”事务，但不覆盖服务启停或网络操作。
#[derive(Clone)]
pub(crate) struct ProcessSelectionStore {
    filePath: Arc<PathBuf>,
    iconCache: Arc<RwLock<BTreeMap<String, Option<String>>>>,
    state: Arc<RwLock<PersistedApplicationConfiguration>>,
    updateLock: Arc<Mutex<()>>,
}

impl ProcessSelectionStore {
    /// 从数据目录加载进程选择；文件不存在表示首次运行，格式损坏或读取失败会阻止后端启动。
    pub(crate) fn load(dataDirectory: &Path) -> Result<Self, std::io::Error> {
        let filePath = dataDirectory.join(applicationConfigurationFileName);
        let (state, removedRetiredConfiguration) = match fs::read(&filePath) {
            Ok(bytes) => decodeApplicationConfiguration(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (PersistedApplicationConfiguration::default(), false)
            }
            Err(error) => return Err(error),
        };
        let store = Self {
            filePath: Arc::new(filePath),
            iconCache: Arc::new(RwLock::new(BTreeMap::new())),
            state: Arc::new(RwLock::new(PersistedApplicationConfiguration {
                service: state.service,
                processSelection: normalizeSelection(state.processSelection),
                recording: state.recording,
                auxiliaryListeners: state.auxiliaryListeners,
                ssl: state.ssl,
                tools: state.tools,
                protocols: state.protocols,
                mcp: state.mcp,
            })),
            updateLock: Arc::new(Mutex::new(())),
        };
        if removedRetiredConfiguration {
            // 已删除工具的旧配置不能继续留在权威文件中，否则后续版本仍会携带不可达状态。
            // 迁移只删除这个已知字段，其余未知字段继续由严格反序列化拒绝，避免损坏配置被静默吞掉。
            store.write(&store.state.read().clone())?;
        }
        Ok(store)
    }

    /// 返回启动时使用的完整服务配置；缺少配置表示首次运行，应采用代码默认值。
    pub(super) fn serviceConfiguration(&self) -> Option<ConfigurationUpdate> {
        self.state.read().service.clone()
    }

    /// 返回启动阶段应恢复的 SSL 规则；返回值不包含证书私钥或运行计数。
    pub(super) fn sslConfiguration(&self) -> SslMitmConfiguration {
        self.state.read().ssl.clone()
    }

    /// 返回启动阶段应恢复的全量工具规则；调用方必须在构造数据面前完成语义校验。
    pub(super) fn toolsConfiguration(&self) -> PersistedToolsConfiguration {
        self.state.read().tools.clone()
    }

    /// 返回启动阶段应恢复的录制偏好；事务与正文仍由新会话重新创建，不跨进程伪造旧状态。
    pub(super) fn recordingConfiguration(&self) -> PersistedRecordingConfiguration {
        self.state.read().recording.clone()
    }

    /// 返回反向代理与端口转发的完整持久化配置；实际绑定地址仍由运行中的监听器快照提供。
    pub(super) fn auxiliaryListenerConfiguration(&self) -> AuxiliaryListenerConfiguration {
        self.state.read().auxiliaryListeners.clone()
    }

    /// 返回 Protobuf 路由和正文校验器配置；描述符原始字节仍由受控描述符目录单独保存。
    pub(super) fn protocolConfiguration(&self) -> PersistedProtocolConfiguration {
        self.state.read().protocols.clone()
    }

    /// 返回集成 MCP 的持久化开关与端口；运行状态由控制进程重新绑定后生成，不能直接从文件恢复。
    pub(super) fn mcpConfiguration(&self) -> McpConfiguration {
        self.state.read().mcp.clone()
    }

    /// 把已保存路径解析为当前 PID，并生成可直接交给 WinDivert 的运行时配置。
    pub(super) fn runtimeConfiguration(&self, proxyPort: u16) -> ProcessCaptureConfiguration {
        let state = self.state.read().processSelection.clone();
        Self::runtimeConfigurationForSelection(&state, proxyPort)
    }

    /// 按 WinDivert 记录的 PID 查询当前本机进程身份；仅用于透明连接建立时写入事务来源。
    ///
    /// 运行上下文：PID 是易变的会话证据，因此每次查询都刷新系统进程表，禁止复用可能已被系统回收的旧映射。
    /// 参数 `processId` 来自 SOCKET/FLOW 事件；进程已退出或路径不可读取时返回 `None`，但不影响流量转发。
    pub(crate) fn processIdentity(&self, processId: u32) -> Option<ProcessCandidate> {
        runningProcesses()
            .into_iter()
            .find(|process| process.processId == processId)
    }

    /// 把规范化选择转换为当前进程表对应的 WinDivert 配置；路径未运行时仍保持空 PID 捕获器就绪。
    fn runtimeConfigurationForSelection(
        state: &PersistedProcessSelection,
        proxyPort: u16,
    ) -> ProcessCaptureConfiguration {
        // 停用状态只持久化路径，不把 PID 送入驱动校验；这也避免用户保存代理服务自身路径时
        // 因未启用的目标集合触发无关校验错误。
        let resolvedProcessIds = if state.enabled {
            resolveSelectedProcessIds(&state.selectedPaths, &runningProcesses())
        } else {
            Vec::new()
        };
        ProcessCaptureConfiguration {
            enabled: state.enabled,
            processIds: resolvedProcessIds.into_iter().collect(),
            proxyPort,
            ..ProcessCaptureConfiguration::default()
        }
    }

    /// 构造界面快照；每次调用刷新进程表，避免返回已经退出或 PID 已复用的陈旧记录。
    fn snapshot(&self) -> ProcessSelectionSnapshot {
        let state = self.state.read().processSelection.clone();
        let processes = runningProcesses();
        let processIcons = self.processIcons(
            processes
                .iter()
                .map(|process| process.executablePath.as_str())
                .chain(state.selectedPaths.iter().map(String::as_str)),
        );
        ProcessSelectionSnapshot {
            enabled: state.enabled,
            selectedPaths: state.selectedPaths.clone(),
            resolvedProcessIds: resolveSelectedProcessIds(&state.selectedPaths, &processes),
            processes,
            processIcons,
        }
    }

    /// 为进程路径生成去重图标表；缓存成功与失败结果，避免每次刷新重复占用 Shell/GDI 资源。
    fn processIcons<'a>(
        &self,
        executablePaths: impl Iterator<Item = &'a str>,
    ) -> BTreeMap<String, String> {
        let mut iconCache = self.iconCache.write();
        let mut processIcons = BTreeMap::new();
        for executablePath in executablePaths {
            let pathKey = executablePath.to_lowercase();
            let iconDataUrl = iconCache.entry(pathKey.clone()).or_insert_with(|| {
                extractProcessIcon(Path::new(executablePath))
                    .map(|pngBytes| format!("data:image/png;base64,{}", STANDARD.encode(pngBytes)))
            });
            if let Some(iconDataUrl) = iconDataUrl {
                processIcons.insert(pathKey, iconDataUrl.clone());
            }
        }
        processIcons
    }

    /// 在内存与磁盘中替换完整选择；写入成功后才发布新内存状态，失败时旧选择保持不变。
    fn replace(&self, update: ProcessSelectionUpdate) -> Result<(), std::io::Error> {
        let _updateGuard = self.updateLock.lock();
        let selection = normalizeSelection(PersistedProcessSelection {
            enabled: update.enabled,
            selectedPaths: update.selectedPaths,
        });
        if selection.enabled && selection.selectedPaths.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "启用进程捕获时至少需要选择一个可执行文件路径",
            ));
        }
        let mut applicationConfiguration = self.state.read().clone();
        applicationConfiguration.processSelection = selection;
        self.write(&applicationConfiguration)?;
        *self.state.write() = applicationConfiguration;
        Ok(())
    }

    /// 替换核心服务配置并保留进程选择；完整写入成功后才更新共享内存视图。
    pub(super) fn replaceServiceConfiguration(
        &self,
        configuration: ConfigurationUpdate,
    ) -> Result<(), std::io::Error> {
        self.replaceConfiguration(|state| state.service = Some(configuration))
    }

    /// 原子持久化 SSL 匹配规则并在落盘成功后发布内存快照；证书材料仍由证书目录独立管理。
    pub(super) fn replaceSslConfiguration(
        &self,
        configuration: SslMitmConfiguration,
    ) -> Result<(), std::io::Error> {
        self.replaceConfiguration(|state| state.ssl = configuration)
    }

    /// 原子持久化所有工具规则；全量快照避免并发更新不同工具时互相覆盖。
    pub(super) fn replaceToolsConfiguration(
        &self,
        configuration: PersistedToolsConfiguration,
    ) -> Result<(), std::io::Error> {
        self.replaceConfiguration(|state| state.tools = configuration)
    }

    /// 原子持久化反向代理与端口转发规则；写盘失败时继续保留原配置，禁止重启后规则静默消失。
    pub(super) fn replaceAuxiliaryListenerConfiguration(
        &self,
        configuration: AuxiliaryListenerConfiguration,
    ) -> Result<(), std::io::Error> {
        self.replaceConfiguration(|state| state.auxiliaryListeners = configuration)
    }

    /// 原子持久化协议查看与校验配置；描述符注册、路由和校验器开关由同一快照恢复。
    pub(super) fn replaceProtocolConfiguration(
        &self,
        configuration: PersistedProtocolConfiguration,
    ) -> Result<(), std::io::Error> {
        self.replaceConfiguration(|state| state.protocols = configuration)
    }

    /// 原子持久化录制偏好；写盘成功后才发布内存快照，避免重启恢复旧值。
    pub(super) fn replaceRecordingConfiguration(
        &self,
        configuration: PersistedRecordingConfiguration,
    ) -> Result<(), std::io::Error> {
        self.replaceConfiguration(|state| state.recording = configuration)
    }

    /// 原子写入 MCP 配置；调用方必须先完成端口绑定或关闭，写盘失败时返回原始 I/O 错误。
    pub(super) fn replaceMcpConfiguration(
        &self,
        configuration: McpConfiguration,
    ) -> Result<(), std::io::Error> {
        self.replaceConfiguration(|state| state.mcp = configuration)
    }

    /// 串行提交单个配置域；回调只修改内存候选，磁盘原子替换成功后才发布完整新快照。
    fn replaceConfiguration(
        &self,
        update: impl FnOnce(&mut PersistedApplicationConfiguration),
    ) -> Result<(), std::io::Error> {
        let _updateGuard = self.updateLock.lock();
        let mut applicationConfiguration = self.state.read().clone();
        update(&mut applicationConfiguration);
        self.write(&applicationConfiguration)?;
        *self.state.write() = applicationConfiguration;
        Ok(())
    }

    /// 将统一配置序列化到用户数据目录；写入错误直接上报，禁止内存状态与磁盘状态静默分叉。
    fn write(
        &self,
        applicationConfiguration: &PersistedApplicationConfiguration,
    ) -> Result<(), std::io::Error> {
        if let Some(parent) = self.filePath.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(applicationConfiguration)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let nextPath = self.filePath.with_extension("json.next");
        {
            let mut nextFile = File::create(&nextPath)?;
            nextFile.write_all(&bytes)?;
            nextFile.sync_all()?;
        }
        replaceConfigurationFile(&nextPath, self.filePath.as_ref())?;
        Ok(())
    }
}

/// 解码统一配置并移除已经退役的录制规则字段；仅启动加载路径调用，成功后调用方立即原子写回清理结果。
///
/// 运行上下文：旧安装可能仍保存 `tools.recordingRules`，而当前工具模型已彻底删除该能力。
/// 参数 `bytes` 是配置文件完整正文；JSON 损坏、其它未知字段或现有字段类型错误均返回 InvalidData。
/// 失败语义：只有精确命中的退役字段会被删除，任何其它不兼容内容仍阻止服务启动。
fn decodeApplicationConfiguration(
    bytes: &[u8],
) -> Result<(PersistedApplicationConfiguration, bool), std::io::Error> {
    let mut document: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let removedRetiredConfiguration = document
        .get_mut("tools")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|tools| tools.remove(retiredRecordingRulesKey))
        .is_some();
    let configuration = serde_json::from_value(document)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok((configuration, removedRetiredConfiguration))
}

/// 以同目录临时文件替换权威配置；Windows 使用写穿透替换，Unix 同步父目录提交名称。
#[cfg(windows)]
fn replaceConfigurationFile(nextPath: &Path, destinationPath: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let nextWide = nextPath
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destinationWide = destinationPath
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(nextWide.as_ptr()),
            PCWSTR(destinationWide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    Ok(())
}

/// 在支持原子覆盖的文件系统上提交配置并同步父目录；失败时保留旧权威文件。
#[cfg(not(windows))]
fn replaceConfigurationFile(nextPath: &Path, destinationPath: &Path) -> Result<(), std::io::Error> {
    fs::rename(nextPath, destinationPath)?;
    if let Some(parent) = destinationPath.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

impl ControlState {
    /// 返回实时进程清单与已保存路径；只读操作不推进 revision，也不改变服务状态。
    async fn processSelectionSnapshot(&self) -> ProcessSelectionSnapshot {
        self.processSelection.snapshot()
    }

    /// 持久化路径选择，并在现有内部双栈入口上热启停 WinDivert 捕获器。
    ///
    /// 运行上下文：融合服务启动后始终持有不可公开访问的内部入口；路径增删、捕获启停只改变
    /// WinDivert 句柄和实时 PID 集，不重建公开 HTTP/SOCKS 监听器，也不打断现有代理长连接。
    /// 持久化或驱动启停失败返回结构化控制错误；服务已停止时只保存下次启动配置。
    async fn replaceProcessSelection(
        &self,
        update: ProcessSelectionUpdate,
    ) -> Result<ProcessSelectionSnapshot, ApiError> {
        let _operationGuard = self.serviceOperationLock.lock().await;
        let proxyPort = self.configuration.read().await.listenPort;
        let (serviceState, internalCaptureAddress) = {
            let service = self.service.lock().await;
            (
                service.state,
                service
                    .runningServer
                    .as_ref()
                    .and_then(|server| server.internalCaptureAddress()),
            )
        };
        let captureEnabledNow = self.processCaptureConfiguration.read().await.enabled;
        self.processSelection.replace(update).map_err(|error| {
            ApiError::internal(ErrorCode::ProcessSelectionOperationFailed)
                .withParam("detail", error.to_string())
        })?;
        let mut pendingConfiguration = self.processSelection.runtimeConfiguration(proxyPort);
        if serviceState == ServiceState::Running {
            let internalCaptureAddress =
                internalCaptureAddress.expect("运行中的融合服务必须保留内部双栈捕获入口");
            pendingConfiguration.proxyAddress = internalCaptureAddress.ip();
            pendingConfiguration.proxyPort = internalCaptureAddress.port();
            let captureOperation = match (captureEnabledNow, pendingConfiguration.enabled) {
                (true, true) => self
                    .processCapture
                    .updateProcessIds(pendingConfiguration.processIds.clone()),
                (false, true) => self.processCapture.start(pendingConfiguration.clone()),
                (true, false) => self.processCapture.stop(),
                (false, false) => Ok(()),
            };
            captureOperation.map_err(|error| {
                ApiError::internal(ErrorCode::ProcessSelectionOperationFailed)
                    .withParam("detail", error.to_string())
            })?;
        }
        let socks5Configuration = self.configuration.read().await.clone();
        let httpConfiguration = self.httpConfiguration.read().await.clone();
        let multiAccountConfiguration = self.multiAccountConfiguration.read().await.clone();
        let multiAccount = self
            .accountService
            .publicState(&multiAccountConfiguration)
            .await;
        let mut processCaptureGuard = self.processCaptureConfiguration.write().await;
        *processCaptureGuard = pendingConfiguration.clone();
        // 配置写锁释放前推进投影代际并发布对应载荷；并发快照只能读取提交前状态或重试后读取提交后状态。
        self.publishProjectionRevisioned(|serverInstanceId, revision| {
            EventMessage::Configuration {
                serverInstanceId,
                revision,
                configuration: Box::new(super::PublicConfiguration::fromInternal(
                    super::ConfigurationProjectionSource {
                        socks5: &socks5Configuration,
                        http: &httpConfiguration,
                        processCapture: &pendingConfiguration,
                        startServiceOnLaunch: self
                            .startServiceOnLaunch
                            .load(std::sync::atomic::Ordering::Acquire),
                    },
                    multiAccount,
                )),
            }
        });
        drop(processCaptureGuard);
        if serviceState == ServiceState::Running {
            self.publishRuntimeViews().await;
        }
        Ok(self.processSelection.snapshot())
    }
}

/// 周期解析持久化路径对应的全部当前 PID，并在同一 WinDivert 运行代际内原子替换目标集合。
///
/// 运行上下文：任务覆盖 `ControlState` 整个生命周期并监听控制面 shutdown；数据面停止或配置操作持锁时
/// 跳过当前周期，不创建无主任务。新增实例在一秒内加入，已有连接由捕获核心强制关闭后自动重连。
pub(super) async fn synchronizeSelectedProcessIds(state: ControlState) {
    let mut shutdownReceiver = state.shutdownSender.subscribe();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = super::waitForControlShutdown(&mut shutdownReceiver) => return,
            _ = interval.tick() => {
                let Ok(_operationGuard) = state.serviceOperationLock.try_lock() else {
                    continue;
                };
                if state.service.lock().await.state != ServiceState::Running {
                    continue;
                }
                let proxyPort = state.configuration.read().await.listenPort;
                let desired = state.processSelection.runtimeConfiguration(proxyPort);
                if !desired.enabled {
                    continue;
                }
                // 即使 PID 集未变化也调用轻量热更新入口：上一次 TCB 删除失败的 PID 会保存在捕获核心，
                // 后续周期继续重试；无待重试项时该调用只比较两个有序集合，不访问系统连接表。
                if let Err(error) = state.processCapture.updateProcessIds(desired.processIds.clone()) {
                    eprintln!("进程捕获 PID 动态同步失败：{error}");
                } else {
                    state
                        .processCaptureConfiguration
                        .write()
                        .await
                        .processIds = desired.processIds;
                }
                // WinDivert 的流表和数据包计数不会产生 SOCKS 会话事件；即使 PID 集没有变化也必须
                // 独立发布实时快照，否则工作台会在透明流量持续经过时一直显示零。
                state.publishProcessCaptureView().await;
            }
        }
    }
}

/// 规范化路径集合；Windows 路径按不区分大小写去重，同时保留首个可读展示形式。
fn normalizeSelection(selection: PersistedProcessSelection) -> PersistedProcessSelection {
    let mut normalizedPaths = BTreeMap::new();
    for path in selection.selectedPaths {
        let trimmedPath = path.trim();
        if !trimmedPath.is_empty() {
            normalizedPaths
                .entry(trimmedPath.to_lowercase())
                .or_insert_with(|| trimmedPath.to_owned());
        }
    }
    PersistedProcessSelection {
        enabled: selection.enabled,
        selectedPaths: normalizedPaths.into_values().collect(),
    }
}

/// 刷新系统进程表并返回具备稳定可执行路径的进程；无法解析路径的系统进程不进入选择器。
fn runningProcesses() -> Vec<ProcessCandidate> {
    let system = System::new_all();
    let mut processes = system
        .processes()
        .iter()
        .filter_map(|(processId, process)| {
            let executablePath = process.exe()?.to_string_lossy().into_owned();
            if executablePath.is_empty() {
                return None;
            }
            Some(ProcessCandidate {
                processId: processId.as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                executablePath,
            })
        })
        .collect::<Vec<_>>();
    processes.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then(left.processId.cmp(&right.processId))
    });
    processes
}

/// 依据保存路径解析所有匹配实例 PID；路径比较遵循 Windows 不区分大小写语义并稳定升序去重。
fn resolveSelectedProcessIds(selectedPaths: &[String], processes: &[ProcessCandidate]) -> Vec<u32> {
    let selected = selectedPaths
        .iter()
        .map(|path| path.to_lowercase())
        .collect::<BTreeSet<_>>();
    processes
        .iter()
        .filter(|process| selected.contains(&process.executablePath.to_lowercase()))
        .map(|process| process.processId)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// 注册进程选择器读写端点；进程枚举为只读 GET，路径替换使用完整 PUT 契约。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router.route(
        "/api/v1/processes",
        get(getProcesses).put(updateProcessSelection),
    )
}

/// 返回实时进程选择快照；读取系统进程表不需要服务处于运行状态。
async fn getProcesses(State(state): State<ControlState>) -> Json<ProcessSelectionSnapshot> {
    Json(state.processSelectionSnapshot().await)
}

/// 校验并替换路径选择；JSON 结构错误与持久化或 WinDivert 热更新失败分别返回稳定控制错误。
async fn updateProcessSelection(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    update: Result<Json<ProcessSelectionUpdate>, JsonRejection>,
) -> Result<Json<ProcessSelectionSnapshot>, LocalizedApiError> {
    let update = update
        .map_err(|_| {
            ApiError::badRequest(ErrorCode::InvalidProcessSelectionRequest).withLocale(locale)
        })?
        .0;
    state
        .replaceProcessSelection(update)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}
