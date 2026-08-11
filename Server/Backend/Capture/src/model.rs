use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use location_core::{LocationPattern, ResolvedLocation};
use serde::{Deserialize, Serialize};

use crate::RecordingRuleConfiguration;

/// 表示 active RecordingSession 是否接纳新事务。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingState {
    Recording,
    Paused,
}

/// 约束事务数量与正文占用；所有值必须大于零。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingLimits {
    pub maxTransactions: usize,
    pub maxBodyBytes: usize,
    pub maxTotalBodyBytes: usize,
}

impl Default for RecordingLimits {
    /// 使用 JavaScript 安全整数的物理不可达边界；正文转入 spill 文件，正常录制不得裁剪或淘汰。
    fn default() -> Self {
        Self {
            // 录制必须以磁盘或系统资源耗尽作为显式失败边界，不能在正常配置下静默裁剪正文或淘汰事务。
            // 采用 JavaScript 安全整数上限可保持控制 API 的计数精确，同时让任何现实磁盘容量都先成为真实边界。
            maxTransactions: 9_007_199_254_740_991,
            maxBodyBytes: 9_007_199_254_740_991,
            maxTotalBodyBytes: 9_007_199_254_740_991,
        }
    }
}

impl RecordingLimits {
    /// 判断三个资源边界是否都能形成有效预算。
    pub const fn isValid(&self) -> bool {
        self.maxTransactions > 0 && self.maxBodyBytes > 0 && self.maxTotalBodyBytes > 0
    }
}

/// 聚合创建录制会话所需的资源与过滤配置，避免构造函数参数持续膨胀。
#[derive(Clone, Debug)]
pub struct RecordingConfiguration {
    pub limits: RecordingLimits,
    pub ignoreLocations: Vec<LocationPattern>,
    pub recordTunnelMetadata: bool,
    pub recordingRules: RecordingRuleConfiguration,
    pub memoryBodyThreshold: usize,
    /// 固定会话级持久元数据记账上限；不计入按需读取产生的瞬时响应副本。
    pub metadataMemoryBudgetBytes: usize,
    pub spillDirectory: PathBuf,
}

impl Default for RecordingConfiguration {
    /// 默认 spill 根目录位于当前用户临时目录，绝不写入源码树或安装目录。
    fn default() -> Self {
        Self {
            limits: RecordingLimits::default(),
            ignoreLocations: Vec::new(),
            recordTunnelMetadata: true,
            recordingRules: RecordingRuleConfiguration::default(),
            memoryBodyThreshold: 256 * 1024,
            metadataMemoryBudgetBytes: crate::metadataBudget::defaultMetadataMemoryBudgetBytes,
            spillDirectory: std::env::temp_dir().join("proxyCapture"),
        }
    }
}

/// 提供控制面可公开的录制状态；不暴露 spill 路径与正文引用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingSnapshot {
    pub recordingSessionId: String,
    pub state: RecordingState,
    pub startedAtMilliseconds: u64,
    pub transactionCount: usize,
    pub droppedCount: u64,
    pub totalBodyBytes: usize,
    /// 包含摘要、两侧头、正文引用及媒体实体二级索引的保守逻辑容量，删除会同步回收。
    pub totalMetadataBytes: usize,
    pub metadataMemoryBudgetBytes: usize,
    pub pendingCleanupCount: usize,
    pub limits: RecordingLimits,
    pub ignoreLocations: Vec<LocationPattern>,
    pub recordTunnelMetadata: bool,
}

/// 聚合一次控制面录制设置变更；所有字段会先完成校验，再在同一会话写锁内提交。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingSettingsUpdate {
    pub state: Option<RecordingState>,
    pub limits: Option<RecordingLimitsUpdate>,
    pub ignoreLocations: Option<Vec<LocationPattern>>,
    pub recordTunnelMetadata: Option<bool>,
}

/// 在单读锁内返回录制统计与一页摘要；控制层借此避免为有界响应克隆全量事务。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingPageView {
    pub recording: RecordingSnapshot,
    pub collectionToken: String,
    pub total: usize,
    pub offset: usize,
    pub transactions: Vec<TransactionSummary>,
}

/// 表示录制限额的部分更新；缺失字段在同一写锁内从当前权威值合并。
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingLimitsUpdate {
    pub maxTransactions: Option<usize>,
    pub maxBodyBytes: Option<usize>,
    pub maxTotalBodyBytes: Option<usize>,
}

/// 区分 HTTP、解密后的 HTTPS、WebSocket 与未解密 CONNECT 隧道。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransactionProtocol {
    Http,
    Https,
    Ws,
    Wss,
    Tunnel,
    Socks,
}

/// 表示事务从请求可见到最终结束的稳定状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransactionStatus {
    Pending,
    Complete,
    Failed,
    Blocked,
    Cancelled,
}

/// 记录足以驱动耗时列与瀑布图的绝对时间点。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionTimings {
    pub startAtMilliseconds: u64,
    pub dnsEndAtMilliseconds: Option<u64>,
    pub connectEndAtMilliseconds: Option<u64>,
    pub tlsEndAtMilliseconds: Option<u64>,
    pub requestSentAtMilliseconds: Option<u64>,
    pub responseStartAtMilliseconds: Option<u64>,
    pub endAtMilliseconds: Option<u64>,
}

/// 记录线上完整消息大小，而不是被限额截断后的本地存储大小。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionSizes {
    pub requestHeaderBytes: u64,
    pub requestBodyBytes: u64,
    pub responseHeaderBytes: u64,
    pub responseBodyBytes: u64,
}

/// 汇总工具和正文状态；正文只保留是否截断，不携带实际字节。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionFlags {
    pub mappedLocal: bool,
    pub mappedRemote: bool,
    pub rewritten: bool,
    pub breakpointHit: bool,
    pub throttled: bool,
    pub mitmDecrypted: bool,
    pub bodyTruncated: bool,
    /// 任一侧头因单项或全局预算仅保留前缀时为 true。
    pub headersTruncated: bool,
    pub fromCache: bool,
}

/// 保存未本地化的事务失败信息；控制层使用 messageKey 和 params 渲染最终 message。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionError {
    pub code: String,
    pub messageKey: String,
    pub params: BTreeMap<String, String>,
}

/// 列表与事件使用的完整事务摘要；结构中不存在请求或响应头体。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionSummary {
    pub transactionId: String,
    pub recordingSessionId: String,
    pub sequence: u64,
    pub protocol: TransactionProtocol,
    pub method: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub query: String,
    pub urlDisplay: String,
    pub status: TransactionStatus,
    pub statusCode: Option<u16>,
    pub clientAddress: String,
    pub clientProcessName: Option<String>,
    pub clientProcessId: Option<u32>,
    pub contentType: String,
    pub timings: TransactionTimings,
    pub sizes: TransactionSizes,
    pub flags: TransactionFlags,
    pub error: Option<TransactionError>,
    pub notes: String,
    pub tags: Vec<String>,
    pub appliedTools: Vec<String>,
}

/// 聚合新事务的不可变输入；Location 在进入录制层前必须已解析。
#[derive(Clone, Debug)]
pub struct BeginTransaction {
    pub protocol: TransactionProtocol,
    pub method: String,
    pub location: ResolvedLocation,
    pub clientAddress: String,
    pub clientProcessName: Option<String>,
    pub clientProcessId: Option<u32>,
    pub contentType: String,
    pub startAtMilliseconds: u64,
}

/// 表示 pending 事务可原子替换的协议观测字段；终态后禁止继续改写线上事实。
#[derive(Clone, Debug, Default)]
pub struct TransactionUpdate {
    /// 请求工具执行完成后的最终方法；用于让事务列表与实际写线请求保持一致。
    pub method: Option<String>,
    /// 请求工具执行完成后的最终目标；主机、端口、路径、查询与显示 URL 必须原子更新。
    pub location: Option<ResolvedLocation>,
    pub statusCode: Option<u16>,
    pub contentType: Option<String>,
    pub flags: Option<TransactionFlags>,
}

/// 表示用户在详情界面维护的标注字段；这些字段不改变协议终态，完成后仍允许编辑。
#[derive(Clone, Debug, Default)]
pub struct TransactionUserUpdate {
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
    pub appliedTools: Option<Vec<String>>,
}

/// 按字段更新传输进度；RecordingSession 在一次写锁内合并，避免并发阶段覆盖彼此结果。
#[derive(Clone, Debug, Default)]
pub struct TransactionProgressUpdate {
    pub requestHeaderBytes: Option<u64>,
    pub requestBodyBytes: Option<u64>,
    pub responseHeaderBytes: Option<u64>,
    pub responseBodyBytes: Option<u64>,
    pub dnsEndAtMilliseconds: Option<u64>,
    pub connectEndAtMilliseconds: Option<u64>,
    pub tlsEndAtMilliseconds: Option<u64>,
    pub requestSentAtMilliseconds: Option<u64>,
    pub responseStartAtMilliseconds: Option<u64>,
}

/// 聚合成功完成时的状态码和结束时间，保证 commit 只做一次终态迁移。
#[derive(Clone, Debug)]
pub struct TransactionCompletion {
    pub statusCode: u16,
    pub endAtMilliseconds: u64,
    pub contentType: String,
}

/// 区分请求与响应的头和正文资源。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageSide {
    Request,
    Response,
}

impl MessageSide {
    /// 返回只用于内部 spill 文件名的稳定短标识。
    pub(crate) const fn fileLabel(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

/// 保留重复头与原始顺序，避免 HashMap 合并 Set-Cookie 等有线语义的字段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeaderField {
    pub name: String,
    pub value: String,
}

/// 描述待写入的正文副本；正常捕获必须保持 `originalBytes == bytes.len()`。
/// `originalBytes` 大于实际字节只保留给导入的旧截断夹具，不是运行时录制路径。
#[derive(Clone, Debug)]
pub struct BodyWrite {
    pub bytes: Vec<u8>,
    pub originalBytes: u64,
    pub contentType: String,
    pub encoding: String,
}

/// 描述按需正文响应的公开元信息，不泄漏本机 spill 文件路径。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BodyHandleMeta {
    pub transactionId: String,
    pub side: MessageSide,
    pub contentType: String,
    pub encoding: String,
    pub storedBytes: usize,
    pub originalBytes: u64,
    pub truncated: bool,
}

/// 描述原始流中一次成功转发的方向片段；storedOffsetBytes 指向同侧聚合正文，
/// 因而查看器可在不复制每个高频片段的前提下精确读取该片段的已录制字节。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StreamPacket {
    pub sequence: u64,
    pub capturedAtMilliseconds: u64,
    pub storedOffsetBytes: usize,
    pub storedBytes: usize,
    pub originalBytes: u64,
    pub truncated: bool,
    #[serde(default)]
    pub action: StreamPacketAction,
    #[serde(default)]
    pub modifications: Vec<StreamPacketModification>,
}

/// 标识流片段经过最终写线规则后的结果；查看器据此显示普通、替换、丢弃或关闭连接。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamPacketAction {
    #[default]
    Forward,
    Replace,
    Drop,
    Close,
}

/// 描述单包最终写线正文中的一段变化；原值和新值均完整保留，前端无需根据规则反推差异。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StreamPacketModification {
    pub offsetBytes: usize,
    pub originalBytes: Vec<u8>,
    pub modifiedBytes: Vec<u8>,
}

/// 在线性化读锁内返回事务摘要、两侧头、正文元信息和有界流片段索引；正文实际字节仍按需读取。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionDetailRecord {
    pub transaction: TransactionSummary,
    pub requestHeaders: Vec<HeaderField>,
    pub responseHeaders: Vec<HeaderField>,
    pub requestBody: Option<BodyHandleMeta>,
    pub responseBody: Option<BodyHandleMeta>,
    pub requestPackets: Vec<StreamPacket>,
    pub responsePackets: Vec<StreamPacket>,
}

/// 保存同一强实体版本下可参与媒体 Range 规划的最小只读候选。
///
/// 该结构只包含规划连续区间所需的序号、闭区间和正文元信息，不复制事务摘要、响应头或
/// 正文字节。调用方必须在规划结束后仅为最终选中的 transactionId 建立稳定正文租约。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResponseRangeCandidate {
    pub transactionId: String,
    pub sequence: u64,
    pub start: u64,
    pub end: u64,
    pub body: BodyHandleMeta,
}

/// 返回按需读取的正文元信息和原始字节；控制 API 再负责 base64 等传输编码。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyResponse {
    pub meta: BodyHandleMeta,
    pub bytes: Vec<u8>,
}

/// 获取 Unix 毫秒时间；系统时钟早于 epoch 时返回零，避免时间字段溢出。
pub fn currentTimeMilliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}
