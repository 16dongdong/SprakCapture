//! 提供可复用的带宽、延迟、可靠性和 MTU 节流计划，并将其接入 HTTP 工具流水线。
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use location_core::{LocationPattern, ResolvedLocation};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::{Instant, sleep};

use crate::{
    PipelineContext, PipelineDirective, PipelineError, PipelineTool, ToolId, ToolPhase,
    ToolRegistration,
    tools::{
        ToolError,
        locationScope::{matchesLocations, validateLocations},
    },
};

const maximumPresetIdLength: usize = 64;
const maximumPresetNameLength: usize = 96;
const builtInPresetCount: usize = 3;
const maximumPublicPresetCount: usize = 64;
const maximumUserPresetCount: usize = maximumPublicPresetCount - builtInPresetCount;
const maximumSafeJavaScriptInteger: u64 = 9_007_199_254_740_991;
const maximumLatencyMilliseconds: u64 = 300_000;
const minimumMtu: usize = 64;
const maximumMtu: usize = 65_535;
const kilobyte: u64 = 1024;
const megabyte: u64 = 1024 * 1024;
const defaultMtu: usize = 1500;
static nextPacerSeed: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct ThrottleProfile {
    pub downloadBytesPerSecond: u64,
    pub uploadBytesPerSecond: u64,
    pub latencyMilliseconds: u64,
    pub latencyJitterMilliseconds: u64,
    pub reliabilityPercent: u8,
    pub mtu: usize,
}

impl Default for ThrottleProfile {
    /// 构造关闭状态或默认网络参数，确保首次创建工具不会意外影响现有转发。
    fn default() -> Self {
        Self {
            downloadBytesPerSecond: 12 * megabyte,
            uploadBytesPerSecond: 3 * megabyte,
            latencyMilliseconds: 50,
            latencyJitterMilliseconds: 0,
            reliabilityPercent: 100,
            mtu: defaultMtu,
        }
    }
}

impl ThrottleProfile {
    /// 校验配置边界并在非法参数时返回结构化错误，调用方据此保持旧运行快照。
    pub fn validate(&self) -> Result<(), ThrottlingError> {
        // 控制面将速率编码为 JavaScript number；限制为安全整数可避免 u64 快照在 Web/MCP 端发生精度漂移。
        if self.downloadBytesPerSecond == 0
            || self.uploadBytesPerSecond == 0
            || self.downloadBytesPerSecond > maximumSafeJavaScriptInteger
            || self.uploadBytesPerSecond > maximumSafeJavaScriptInteger
        {
            return Err(ThrottlingError::InvalidRate);
        }
        if self.latencyMilliseconds > maximumLatencyMilliseconds
            || self.latencyJitterMilliseconds > maximumLatencyMilliseconds
        {
            return Err(ThrottlingError::InvalidLatency);
        }
        if self.reliabilityPercent > 100 {
            return Err(ThrottlingError::InvalidReliability);
        }
        if !(minimumMtu..=maximumMtu).contains(&self.mtu) {
            return Err(ThrottlingError::InvalidMtu);
        }
        Ok(())
    }

    /// 按数据流方向返回对应带宽上限，上传和下载预算始终彼此独立。
    pub const fn rateFor(&self, direction: ThrottleDirection) -> u64 {
        match direction {
            ThrottleDirection::Upload => self.uploadBytesPerSecond,
            ThrottleDirection::Download => self.downloadBytesPerSecond,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct ThrottlePreset {
    pub id: String,
    pub name: String,
    pub downloadBytesPerSecond: u64,
    pub uploadBytesPerSecond: u64,
    pub latencyMilliseconds: u64,
    pub latencyJitterMilliseconds: u64,
    pub reliabilityPercent: u8,
    pub mtu: usize,
}

impl ThrottlePreset {
    /// 返回不含展示标识的节流参数副本，供运行计划和令牌桶复用。
    pub fn profile(&self) -> ThrottleProfile {
        ThrottleProfile {
            downloadBytesPerSecond: self.downloadBytesPerSecond,
            uploadBytesPerSecond: self.uploadBytesPerSecond,
            latencyMilliseconds: self.latencyMilliseconds,
            latencyJitterMilliseconds: self.latencyJitterMilliseconds,
            reliabilityPercent: self.reliabilityPercent,
            mtu: self.mtu,
        }
    }

    /// 校验配置边界并在非法参数时返回结构化错误，调用方据此保持旧运行快照。
    pub fn validate(&self) -> Result<(), ThrottlingError> {
        if self.id.trim().is_empty() || self.id.len() > maximumPresetIdLength {
            return Err(ThrottlingError::InvalidPresetId);
        }
        if self.name.trim().is_empty() || self.name.len() > maximumPresetNameLength {
            return Err(ThrottlingError::InvalidPresetName);
        }
        self.profile().validate()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct ThrottlingConfiguration {
    pub enabled: bool,
    pub activePresetId: Option<String>,
    pub custom: ThrottleProfile,
    pub locations: Vec<LocationPattern>,
    #[serde(default)]
    pub userPresets: Vec<ThrottlePreset>,
}

impl Default for ThrottlingConfiguration {
    /// 构造关闭状态或默认网络参数，确保首次创建工具不会意外影响现有转发。
    fn default() -> Self {
        Self {
            enabled: false,
            activePresetId: None,
            custom: ThrottleProfile::default(),
            locations: Vec::new(),
            userPresets: Vec::new(),
        }
    }
}

impl ThrottlingConfiguration {
    /// 校验配置边界并在非法参数时返回结构化错误，调用方据此保持旧运行快照。
    pub fn validate(&self) -> Result<(), ThrottlingError> {
        self.custom.validate()?;
        validateLocations(&self.locations)?;
        // 公开快照同时包含内置预设；先限制用户项数量，避免构造预设去重集合和前端快照出现无界增长。
        if self.userPresets.len() > maximumUserPresetCount {
            return Err(ThrottlingError::TooManyUserPresets);
        }
        let mut presetIds = BTreeSet::new();
        for preset in &self.userPresets {
            preset.validate()?;
            if isBuiltInPresetId(&preset.id) || !presetIds.insert(preset.id.clone()) {
                return Err(ThrottlingError::DuplicatePresetId);
            }
        }
        if let Some(presetId) = &self.activePresetId
            && self.presetById(presetId).is_none()
        {
            return Err(ThrottlingError::UnknownPresetId);
        }
        Ok(())
    }

    /// 解析当前自定义或选中预设的有效参数，未知预设返回稳定错误。
    pub fn effectiveProfile(&self) -> Result<ThrottleProfile, ThrottlingError> {
        let Some(presetId) = &self.activePresetId else {
            return Ok(self.custom.clone());
        };
        self.presetById(presetId)
            .map(|preset| preset.profile())
            .ok_or(ThrottlingError::UnknownPresetId)
    }

    /// 从内置与用户预设中查找稳定标识，避免暴露配置锁的内部引用。
    fn presetById(&self, presetId: &str) -> Option<ThrottlePreset> {
        builtInThrottlePresets()
            .into_iter()
            .chain(self.userPresets.iter().cloned())
            .find(|preset| preset.id == presetId)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct ThrottlingPublicState {
    pub enabled: bool,
    pub activePresetId: Option<String>,
    pub custom: ThrottleProfile,
    pub locations: Vec<LocationPattern>,
    pub presets: Vec<ThrottlePreset>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub enum ThrottleDirection {
    Upload,
    Download,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub enum ThrottleChunkAction {
    Forward,
    Drop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct ThrottleChunk {
    pub byteCount: usize,
    pub action: ThrottleChunkAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct ThrottlePlan {
    profile: ThrottleProfile,
}

impl ThrottlePlan {
    /// 创建并校验运行对象；参数非法时不产生可注册的半初始化实例。
    pub fn new(profile: ThrottleProfile) -> Result<Self, ThrottlingError> {
        profile.validate()?;
        Ok(Self { profile })
    }

    /// 返回不含展示标识的节流参数副本，供运行计划和令牌桶复用。
    pub fn profile(&self) -> &ThrottleProfile {
        &self.profile
    }

    /// 为指定方向创建独立调度器，使连接两侧不会竞争同一令牌预算。
    pub fn createPacer(
        &self,
        direction: ThrottleDirection,
    ) -> Result<ThrottlePacer, ThrottlingError> {
        ThrottlePacer::new(self.profile.clone(), direction)
    }

    /// 使用显式种子创建可重放调度器，供确定性测试和诊断复现使用。
    pub fn createPacerWithSeed(
        &self,
        direction: ThrottleDirection,
        seed: u64,
    ) -> Result<ThrottlePacer, ThrottlingError> {
        ThrottlePacer::newWithSeed(self.profile.clone(), direction, seed)
    }
}

#[derive(Debug)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct TokenBucket {
    rateBytesPerSecond: u64,
    capacityBytes: usize,
    availableBytes: f64,
    lastRefillAt: Instant,
}

impl TokenBucket {
    /// 创建并校验运行对象；参数非法时不产生可注册的半初始化实例。
    pub fn new(rateBytesPerSecond: u64, capacityBytes: usize) -> Result<Self, ThrottlingError> {
        if rateBytesPerSecond == 0 {
            return Err(ThrottlingError::InvalidRate);
        }
        if !(minimumMtu..=maximumMtu).contains(&capacityBytes) {
            return Err(ThrottlingError::InvalidMtu);
        }
        Ok(Self {
            rateBytesPerSecond,
            capacityBytes,
            availableBytes: capacityBytes as f64,
            lastRefillAt: Instant::now(),
        })
    }

    /// 返回当前令牌桶允许的最大分块字节数，调用方必须据此切分数据帧。
    pub const fn maximumChunkBytes(&self) -> usize {
        self.capacityBytes
    }

    /// 等待并消耗指定字节数的令牌；无效分块大小返回结构化错误。
    pub async fn acquire(&mut self, byteCount: usize) -> Result<(), ThrottlingError> {
        if byteCount == 0 || byteCount > self.capacityBytes {
            return Err(ThrottlingError::InvalidChunkSize);
        }
        loop {
            self.refill();
            if self.availableBytes >= byteCount as f64 {
                self.availableBytes -= byteCount as f64;
                return Ok(());
            }
            let missingBytes = byteCount as f64 - self.availableBytes;
            let waitSeconds = missingBytes / self.rateBytesPerSecond as f64;
            sleep(Duration::from_secs_f64(waitSeconds.max(f64::EPSILON))).await;
        }
    }

    /// 依据单调时钟补充令牌并限制突发容量，避免空闲连接积累无上限预算。
    fn refill(&mut self) {
        let elapsed = self.lastRefillAt.elapsed().as_secs_f64();
        self.lastRefillAt = Instant::now();
        self.availableBytes = (self.availableBytes + elapsed * self.rateBytesPerSecond as f64)
            .min(self.capacityBytes as f64);
    }
}

#[derive(Debug)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct ThrottlePacer {
    profile: ThrottleProfile,
    direction: ThrottleDirection,
    tokenBucket: TokenBucket,
    initialLatencyPending: bool,
    randomState: u64,
}

impl ThrottlePacer {
    /// 创建并校验运行对象；参数非法时不产生可注册的半初始化实例。
    pub fn new(
        profile: ThrottleProfile,
        direction: ThrottleDirection,
    ) -> Result<Self, ThrottlingError> {
        let seed = nextPacerSeed.fetch_add(1, Ordering::Relaxed)
            ^ profile.rateFor(direction).rotate_left(17)
            ^ directionSeed(direction);
        Self::newWithSeed(profile, direction, seed)
    }

    /// 构造带确定性随机状态的节流调度器，保证可靠性和抖动测试可复现。
    pub fn newWithSeed(
        profile: ThrottleProfile,
        direction: ThrottleDirection,
        seed: u64,
    ) -> Result<Self, ThrottlingError> {
        profile.validate()?;
        let tokenBucket = TokenBucket::new(profile.rateFor(direction), profile.mtu)?;
        Ok(Self {
            profile,
            direction,
            tokenBucket,
            initialLatencyPending: true,
            randomState: normalizeSeed(seed),
        })
    }

    /// 返回该调度器所属方向，供调用方将传输失败映射到准确的数据面。
    pub const fn direction(&self) -> ThrottleDirection {
        self.direction
    }

    /// 返回当前令牌桶允许的最大分块字节数，调用方必须据此切分数据帧。
    pub const fn maximumChunkBytes(&self) -> usize {
        self.tokenBucket.maximumChunkBytes()
    }

    /// 申请下一次分块传输计划，依次执行首包延迟、令牌桶限速和可靠性决策。
    pub async fn nextChunk(
        &mut self,
        availableBytes: usize,
    ) -> Result<Option<ThrottleChunk>, ThrottlingError> {
        if availableBytes == 0 {
            return Ok(None);
        }
        self.applyInitialLatency().await;
        let byteCount = availableBytes.min(self.maximumChunkBytes());
        self.tokenBucket.acquire(byteCount).await?;
        Ok(Some(ThrottleChunk {
            byteCount,
            action: self.nextAction(),
        }))
    }

    /// 仅在方向首个实际分块前注入固定延迟和抖动，空帧不改变网络模拟语义。
    async fn applyInitialLatency(&mut self) {
        if !self.initialLatencyPending {
            return;
        }
        self.initialLatencyPending = false;
        let delayMilliseconds = self
            .profile
            .latencyMilliseconds
            .saturating_add(self.nextJitterMilliseconds());
        if delayMilliseconds > 0 {
            sleep(Duration::from_millis(delayMilliseconds)).await;
        }
    }

    /// 依据可靠性百分比决定转发或丢弃当前分块，丢弃同样消耗链路带宽。
    fn nextAction(&mut self) -> ThrottleChunkAction {
        let reliabilityPercent = self.profile.reliabilityPercent;
        match reliabilityPercent {
            100 => ThrottleChunkAction::Forward,
            0 => ThrottleChunkAction::Drop,
            _ if self.nextRandomPercent() < reliabilityPercent => ThrottleChunkAction::Forward,
            _ => ThrottleChunkAction::Drop,
        }
    }

    /// 生成有界延迟抖动，使用实例随机状态确保同一连接的行为可重放。
    fn nextJitterMilliseconds(&mut self) -> u64 {
        if self.profile.latencyJitterMilliseconds == 0 {
            return 0;
        }
        self.nextRandomU64() % (self.profile.latencyJitterMilliseconds + 1)
    }

    /// 生成零到九十九的采样值，用于可靠性百分比决策。
    fn nextRandomPercent(&mut self) -> u8 {
        (self.nextRandomU64() % 100) as u8
    }

    /// 推进轻量随机状态；该序列只用于网络模拟，不承担安全用途。
    fn nextRandomU64(&mut self) -> u64 {
        self.randomState ^= self.randomState << 13;
        self.randomState ^= self.randomState >> 7;
        self.randomState ^= self.randomState << 17;
        self.randomState
    }
}

#[derive(Clone)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub struct ThrottlingTool {
    configuration: Arc<RwLock<ThrottlingConfiguration>>,
}

impl ThrottlingTool {
    /// 创建并校验运行对象；参数非法时不产生可注册的半初始化实例。
    pub fn new(configuration: ThrottlingConfiguration) -> Result<Self, ThrottlingError> {
        configuration.validate()?;
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
        })
    }

    /// 生成控制面读取的权威快照，合并内置与用户预设且不泄漏内部锁。
    pub fn publicState(&self) -> ThrottlingPublicState {
        let configuration = self.configuration();
        let mut presets = builtInThrottlePresets();
        presets.extend(configuration.userPresets.iter().cloned());
        ThrottlingPublicState {
            enabled: configuration.enabled,
            activePresetId: configuration.activePresetId,
            custom: configuration.custom,
            locations: configuration.locations,
            presets,
        }
    }

    /// 返回当前完整配置副本，供控制面读取或构造原子热更新请求。
    pub fn configuration(&self) -> ThrottlingConfiguration {
        self.configuration.read().clone()
    }

    /// 完成校验后原子替换配置，失败时保留全部旧运行状态。
    pub fn updateConfiguration(
        &self,
        configuration: ThrottlingConfiguration,
    ) -> Result<ThrottlingConfiguration, ThrottlingError> {
        configuration.validate()?;
        *self.configuration.write() = configuration;
        Ok(self.configuration())
    }

    /// 仅在全局开关开启且位置命中时生成不可变计划，空范围匹配全部流量。
    pub fn planFor(
        &self,
        location: &ResolvedLocation,
    ) -> Result<Option<ThrottlePlan>, ThrottlingError> {
        let configuration = self.configuration();
        if !configuration.enabled || !matchesLocations(&configuration.locations, location)? {
            return Ok(None);
        }
        ThrottlePlan::new(configuration.effectiveProfile()?).map(Some)
    }
}

#[async_trait]
impl PipelineTool for ThrottlingTool {
    /// 声明请求和响应流水线槽位，运行期依据最新配置决定是否参与。
    fn registration(&self) -> ToolRegistration {
        ToolRegistration::new(
            ToolId::Throttling,
            vec![ToolPhase::Request, ToolPhase::Response],
            self.configuration().enabled,
        )
    }

    /// 在请求阶段保存上传节流计划并标记事务，供转发器包装请求正文流。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let Some(plan) = self
            .planFor(&context.location)
            .map_err(throttlingPipelineError)?
        else {
            return Ok(PipelineDirective::Continue);
        };
        context.requestThrottlePlan = Some(plan);
        context.flags.throttled = true;
        Ok(PipelineDirective::Applied)
    }

    /// 在响应阶段保存下载节流计划并标记事务，覆盖上游和本地合成响应。
    async fn onResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let Some(plan) = self
            .planFor(&context.location)
            .map_err(throttlingPipelineError)?
        else {
            return Ok(PipelineDirective::Continue);
        };
        context.responseThrottlePlan = Some(plan);
        context.flags.throttled = true;
        Ok(PipelineDirective::Applied)
    }
}

/// 返回独立的内置网络预设列表，调用方可以安全用于控制面展示。
pub fn builtInThrottlePresets() -> Vec<ThrottlePreset> {
    vec![
        ThrottlePreset {
            id: "lte".to_owned(),
            name: "LTE".to_owned(),
            downloadBytesPerSecond: 12 * megabyte,
            uploadBytesPerSecond: 3 * megabyte,
            latencyMilliseconds: 50,
            latencyJitterMilliseconds: 10,
            reliabilityPercent: 100,
            mtu: defaultMtu,
        },
        ThrottlePreset {
            id: "3g".to_owned(),
            name: "3G".to_owned(),
            downloadBytesPerSecond: 400 * kilobyte,
            uploadBytesPerSecond: 100 * kilobyte,
            latencyMilliseconds: 200,
            latencyJitterMilliseconds: 30,
            reliabilityPercent: 99,
            mtu: defaultMtu,
        },
        ThrottlePreset {
            id: "edge".to_owned(),
            name: "EDGE".to_owned(),
            downloadBytesPerSecond: 40 * kilobyte,
            uploadBytesPerSecond: 20 * kilobyte,
            latencyMilliseconds: 400,
            latencyJitterMilliseconds: 75,
            reliabilityPercent: 97,
            mtu: defaultMtu,
        },
    ]
}

/// 判断预设标识是否被内置档位保留，防止用户配置覆盖跨版本语义。
fn isBuiltInPresetId(presetId: &str) -> bool {
    builtInThrottlePresets()
        .iter()
        .any(|preset| preset.id == presetId)
}

/// 将零种子规范化为非零常量，避免随机状态机停留在全零序列。
const fn normalizeSeed(seed: u64) -> u64 {
    if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    }
}

/// 为上传和下载混入不同固定盐值，防止两侧产生相同可靠性序列。
const fn directionSeed(direction: ThrottleDirection) -> u64 {
    match direction {
        ThrottleDirection::Upload => 0xA24B_AED4_963E_E407,
        ThrottleDirection::Download => 0x9FB2_1C65_1E98_DF25,
    }
}

/// 将工具层失败映射为带固定槽位的流水线错误，便于控制面精确定位。
fn throttlingPipelineError(error: ThrottlingError) -> PipelineError {
    PipelineError::ToolFailed {
        toolId: ToolId::Throttling,
        code: error.code().to_owned(),
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
/// 描述节流模块的数据模型或执行器；字段共同维护配置、流水线和流式传输之间的稳定契约。
pub enum ThrottlingError {
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("error.throttling.invalidRate")]
    InvalidRate,
    #[error("error.throttling.invalidLatency")]
    InvalidLatency,
    #[error("error.throttling.invalidMtu")]
    InvalidMtu,
    #[error("error.throttling.invalidReliability")]
    InvalidReliability,
    #[error("error.throttling.invalidPresetId")]
    InvalidPresetId,
    #[error("error.throttling.invalidPresetName")]
    InvalidPresetName,
    #[error("error.throttling.tooManyUserPresets")]
    TooManyUserPresets,
    #[error("error.throttling.duplicatePresetId")]
    DuplicatePresetId,
    #[error("error.throttling.unknownPresetId")]
    UnknownPresetId,
    #[error("error.throttling.invalidChunkSize")]
    InvalidChunkSize,
}

impl ThrottlingError {
    /// 返回跨控制 API、日志和测试稳定的机器错误码。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Tool(error) => error.code(),
            Self::InvalidRate => "throttlingInvalidRate",
            Self::InvalidLatency => "throttlingInvalidLatency",
            Self::InvalidMtu => "throttlingInvalidMtu",
            Self::InvalidReliability => "throttlingInvalidReliability",
            Self::InvalidPresetId => "throttlingInvalidPresetId",
            Self::InvalidPresetName => "throttlingInvalidPresetName",
            Self::TooManyUserPresets => "throttlingTooManyUserPresets",
            Self::DuplicatePresetId => "throttlingDuplicatePresetId",
            Self::UnknownPresetId => "throttlingUnknownPresetId",
            Self::InvalidChunkSize => "throttlingInvalidChunkSize",
        }
    }

    /// 返回由语言包渲染的稳定消息键，底层工具不生成用户可见文本。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::Tool(error) => error.messageKey(),
            Self::InvalidRate => "error.throttling.invalidRate",
            Self::InvalidLatency => "error.throttling.invalidLatency",
            Self::InvalidMtu => "error.throttling.invalidMtu",
            Self::InvalidReliability => "error.throttling.invalidReliability",
            Self::InvalidPresetId => "error.throttling.invalidPresetId",
            Self::InvalidPresetName => "error.throttling.invalidPresetName",
            Self::TooManyUserPresets => "error.throttling.tooManyUserPresets",
            Self::DuplicatePresetId => "error.throttling.duplicatePresetId",
            Self::UnknownPresetId => "error.throttling.unknownPresetId",
            Self::InvalidChunkSize => "error.throttling.invalidChunkSize",
        }
    }
}
