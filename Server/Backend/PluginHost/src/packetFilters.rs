//! 提供直接作用于最终线上字节的有序封包滤镜。
//!
//! 滤镜位于插件/mod处理之后、Socket 写入之前，支持搜索条件与替换输出采用不同长度。
//! 当替换比显式搜索条件更长时，搜索尾部自动视为通配位置；替换更短时会缩短当前命中字节段。

use std::{collections::HashSet, sync::Arc};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ConnectionMetadata, StreamDirection, TransportKind};

const MAXIMUM_RULES: usize = 256;
const MAXIMUM_IDENTIFIER_BYTES: usize = 64;
const MAXIMUM_NAME_BYTES: usize = 128;
const MAXIMUM_HOST_BYTES: usize = 253;
// 搜索与替换均对应 WPE 网格的 0000–01FF 偏移，后端必须拒绝超出界面容量的配置。
const MAXIMUM_PATTERN_TOKENS: usize = 512;
const MAXIMUM_PACKET_BYTES: usize = 16 * 1024 * 1024;

/// 标识一条规则适用的传输类型；Any 让同一字节规则同时作用于 TCP 与 UDP。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PacketFilterTransport {
    #[default]
    Any,
    Tcp,
    Udp,
}

/// 标识一条规则适用的线上方向；Up 表示客户端到服务器，Down 表示服务器到客户端。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PacketFilterDirection {
    #[default]
    Any,
    Up,
    Down,
}

/// 定义滤镜命中后的最终数据面动作；Modify 会把完整命中字节段替换为独立输出序列。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PacketFilterAction {
    #[default]
    Modify,
    Drop,
    Close,
}

/// 描述一条可持久化封包滤镜；空主机、端口和字节模式分别表示不限制对应字段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct PacketFilterRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: PacketFilterTransport,
    pub direction: PacketFilterDirection,
    pub host: String,
    pub port: Option<u16>,
    pub minimumLength: Option<usize>,
    pub maximumLength: Option<usize>,
    pub pattern: String,
    pub replacement: String,
    pub action: PacketFilterAction,
    pub replaceAll: bool,
    pub continueMatching: bool,
}

impl Default for PacketFilterRule {
    /// 创建禁用的替换草稿；编辑器必须补齐标识、名称和独立替换字节后才能提交。
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: false,
            transport: PacketFilterTransport::Any,
            direction: PacketFilterDirection::Any,
            host: String::new(),
            port: None,
            minimumLength: None,
            maximumLength: None,
            pattern: String::new(),
            replacement: String::new(),
            action: PacketFilterAction::Modify,
            replaceAll: false,
            continueMatching: false,
        }
    }
}

/// 保存封包滤镜总开关和有序规则；规则顺序就是线上执行顺序。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct PacketFilterConfiguration {
    pub enabled: bool,
    pub rules: Vec<PacketFilterRule>,
}

/// 归类配置编译错误；控制面据此拒绝持久化半有效规则。
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PacketFilterError {
    #[error("packetFilterTooManyRules")]
    TooManyRules,
    #[error("packetFilterInvalidIdentifier")]
    InvalidIdentifier,
    #[error("packetFilterDuplicateIdentifier")]
    DuplicateIdentifier,
    #[error("packetFilterInvalidName")]
    InvalidName,
    #[error("packetFilterInvalidHost")]
    InvalidHost,
    #[error("packetFilterInvalidLength")]
    InvalidLength,
    #[error("packetFilterInvalidPattern")]
    InvalidPattern,
    #[error("packetFilterInvalidReplacement")]
    InvalidReplacement,
}

/// 表示滤镜完成全部规则后的唯一线上决定；Forward 始终拥有最终字节。
#[derive(Debug, Eq, PartialEq)]
pub enum PacketFilterResult {
    Forward { bytes: Vec<u8> },
    Drop,
    Close,
}

/// 保存已解析的定长字节模式；`None` 代表搜索或替换位置上的单字节通配符。
#[derive(Clone)]
struct CompiledBytePattern {
    bytes: Vec<Option<u8>>,
}

/// 保存单条规则的无分配元数据条件和预解析字节模式，供数据面重复读取。
#[derive(Clone)]
struct CompiledRule {
    transport: PacketFilterTransport,
    direction: PacketFilterDirection,
    host: String,
    port: Option<u16>,
    minimumLength: Option<usize>,
    maximumLength: Option<usize>,
    pattern: Option<CompiledBytePattern>,
    replacement: Option<CompiledBytePattern>,
    action: PacketFilterAction,
    replaceAll: bool,
    continueMatching: bool,
}

/// 同时保存可回传的原始配置与只读执行数组，确保控制快照和数据面来自同一代际。
#[derive(Clone)]
struct CompiledConfiguration {
    source: PacketFilterConfiguration,
    rules: Arc<[CompiledRule]>,
}

/// 保存可在运行时原子替换的预编译规则快照；克隆不会复制字节模式。
#[derive(Clone)]
pub struct PacketFilterRuntime {
    compiled: Arc<RwLock<Arc<CompiledConfiguration>>>,
}

impl Default for PacketFilterRuntime {
    /// 创建透明关闭状态；未配置滤镜时数据面不执行任何模式扫描。
    fn default() -> Self {
        Self::new(PacketFilterConfiguration::default()).expect("默认封包滤镜配置必须始终有效")
    }
}

impl PacketFilterRuntime {
    /// 编译完整配置；任一规则非法时不创建部分可运行快照。
    pub fn new(configuration: PacketFilterConfiguration) -> Result<Self, PacketFilterError> {
        Ok(Self {
            compiled: Arc::new(RwLock::new(Arc::new(compileConfiguration(configuration)?))),
        })
    }

    /// 克隆当前公开配置；预编译模式和运行态引用不会进入持久化对象。
    pub fn configuration(&self) -> PacketFilterConfiguration {
        self.compiled.read().source.clone()
    }

    /// 验证并原子替换全部规则；失败时旧快照继续处理现有和新建连接。
    pub fn replaceConfiguration(
        &self,
        configuration: PacketFilterConfiguration,
    ) -> Result<(), PacketFilterError> {
        let compiled = Arc::new(compileConfiguration(configuration)?);
        *self.compiled.write() = compiled;
        Ok(())
    }

    /// 在最终 Socket 写入前执行有序规则；匹配和修改只操作调用方拥有的当前字节。
    pub fn process(
        &self,
        metadata: &ConnectionMetadata,
        direction: StreamDirection,
        mut bytes: Vec<u8>,
    ) -> PacketFilterResult {
        let compiled = self.compiled.read().clone();
        if !compiled.source.enabled {
            return PacketFilterResult::Forward { bytes };
        }
        for rule in compiled.rules.iter() {
            let Some(matches) = rule.matchingOffsets(metadata, direction, &bytes) else {
                continue;
            };
            match rule.action {
                PacketFilterAction::Modify => bytes = rule.applyReplacement(bytes, &matches),
                PacketFilterAction::Drop => return PacketFilterResult::Drop,
                PacketFilterAction::Close => return PacketFilterResult::Close,
            }
            if !rule.continueMatching {
                break;
            }
        }
        PacketFilterResult::Forward { bytes }
    }
}

impl CompiledRule {
    /// 校验元数据并返回当前规则的命中字节偏移；无字节模式时用零偏移表示条件命中。
    fn matchingOffsets(
        &self,
        metadata: &ConnectionMetadata,
        direction: StreamDirection,
        bytes: &[u8],
    ) -> Option<Vec<usize>> {
        if !self.matchesMetadata(metadata, direction, bytes.len()) {
            return None;
        }
        let Some(pattern) = &self.pattern else {
            return Some(vec![0]);
        };
        let offsets = pattern.findOffsets(bytes, self.replaceAll);
        (!offsets.is_empty()).then_some(offsets)
    }

    /// 匹配不会变化的连接字段与当前块长度；主机匹配支持精确值和 `*.` 后缀。
    fn matchesMetadata(
        &self,
        metadata: &ConnectionMetadata,
        direction: StreamDirection,
        length: usize,
    ) -> bool {
        let transportMatches = matches!(self.transport, PacketFilterTransport::Any)
            || matches!(
                (self.transport, metadata.transport),
                (PacketFilterTransport::Tcp, TransportKind::Tcp)
                    | (PacketFilterTransport::Udp, TransportKind::Udp)
            );
        let directionMatches = matches!(self.direction, PacketFilterDirection::Any)
            || matches!(
                (self.direction, direction),
                (PacketFilterDirection::Up, StreamDirection::ClientToServer)
                    | (PacketFilterDirection::Down, StreamDirection::ServerToClient)
            );
        transportMatches
            && directionMatches
            && self.matchesHost(&metadata.targetHost)
            && self.port.is_none_or(|port| port == metadata.targetPort)
            && self.minimumLength.is_none_or(|minimum| length >= minimum)
            && self.maximumLength.is_none_or(|maximum| length <= maximum)
    }

    /// 对规范化主机执行精确或子域后缀匹配；空条件始终命中。
    fn matchesHost(&self, host: &str) -> bool {
        if self.host.is_empty() {
            return true;
        }
        let candidate = normalizeHost(host);
        if let Some(suffix) = self.host.strip_prefix("*.") {
            return candidate
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1);
        }
        candidate == self.host
    }

    /// 按原始块上的互不重叠偏移执行变长替换；显式 `??` 保留命中段对应位置字节。
    /// 运行上下文：偏移在修改前一次性计算，按顺序重建输出可避免前一次变长影响后续命中位置。
    fn applyReplacement(&self, bytes: Vec<u8>, offsets: &[usize]) -> Vec<u8> {
        let patternLength = self
            .pattern
            .as_ref()
            .expect("Modify 规则在编译阶段必须生成搜索模式")
            .bytes
            .len();
        let replacement = self
            .replacement
            .as_ref()
            .expect("Modify 规则在编译阶段必须生成替换模式");
        let removedBytes = patternLength * offsets.len();
        let insertedBytes = replacement.bytes.len() * offsets.len();
        let mut output = Vec::with_capacity(bytes.len() - removedBytes + insertedBytes);
        let mut sourceOffset = 0_usize;
        for &offset in offsets {
            output.extend_from_slice(&bytes[sourceOffset..offset]);
            for (index, replacementByte) in replacement.bytes.iter().enumerate() {
                output.push(replacementByte.unwrap_or(bytes[offset + index]));
            }
            sourceOffset = offset + patternLength;
        }
        output.extend_from_slice(&bytes[sourceOffset..]);
        output
    }
}

impl CompiledBytePattern {
    /// 扫描当前字节块并返回首个或全部不重叠命中；通配字节不会产生额外分配。
    fn findOffsets(&self, bytes: &[u8], replaceAll: bool) -> Vec<usize> {
        if bytes.len() < self.bytes.len() {
            return Vec::new();
        }
        let mut offsets = Vec::new();
        let mut offset = 0_usize;
        while offset + self.bytes.len() <= bytes.len() {
            if self
                .bytes
                .iter()
                .zip(&bytes[offset..offset + self.bytes.len()])
                .all(|(expected, actual)| expected.is_none_or(|expected| expected == *actual))
            {
                offsets.push(offset);
                if !replaceAll {
                    break;
                }
                offset += self.bytes.len();
            } else {
                offset += 1;
            }
        }
        offsets
    }
}

/// 编译并校验完整配置；禁用规则仍校验，防止再次启用时发布陈旧坏规则。
fn compileConfiguration(
    configuration: PacketFilterConfiguration,
) -> Result<CompiledConfiguration, PacketFilterError> {
    if configuration.rules.len() > MAXIMUM_RULES {
        return Err(PacketFilterError::TooManyRules);
    }
    let mut identifiers = HashSet::with_capacity(configuration.rules.len());
    let mut rules = Vec::with_capacity(configuration.rules.len());
    for rule in &configuration.rules {
        validateText(&rule.id, MAXIMUM_IDENTIFIER_BYTES)
            .map_err(|_| PacketFilterError::InvalidIdentifier)?;
        if !identifiers.insert(rule.id.as_str()) {
            return Err(PacketFilterError::DuplicateIdentifier);
        }
        validateText(&rule.name, MAXIMUM_NAME_BYTES).map_err(|_| PacketFilterError::InvalidName)?;
        let host = validateHost(&rule.host)?;
        validateLengths(rule.minimumLength, rule.maximumLength)?;
        let (pattern, replacement) = compileBytePatterns(rule)?;
        if rule.enabled {
            rules.push(CompiledRule {
                transport: rule.transport,
                direction: rule.direction,
                host,
                port: rule.port,
                minimumLength: rule.minimumLength,
                maximumLength: rule.maximumLength,
                pattern,
                replacement,
                action: rule.action,
                replaceAll: rule.replaceAll,
                continueMatching: rule.continueMatching,
            });
        }
    }
    Ok(CompiledConfiguration {
        source: configuration,
        rules: rules.into(),
    })
}

/// 校验标识和名称是有界非空文本；控制字符不允许进入配置文件或界面。
fn validateText(value: &str, maximumBytes: usize) -> Result<(), ()> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > maximumBytes || trimmed.chars().any(char::is_control) {
        return Err(());
    }
    Ok(())
}

/// 规范化并校验主机条件；只接受精确主机或单个前导 `*.` 通配后缀。
fn validateHost(value: &str) -> Result<String, PacketFilterError> {
    let normalized = normalizeHost(value);
    if normalized.len() > MAXIMUM_HOST_BYTES
        || normalized.chars().any(char::is_control)
        || normalized.contains('*') && !normalized.starts_with("*.")
        || normalized.matches('*').count() > 1
        || normalized == "*."
    {
        return Err(PacketFilterError::InvalidHost);
    }
    Ok(normalized)
}

/// 校验块长度范围且限制扫描预算；零长度和反向区间会被拒绝。
fn validateLengths(
    minimum: Option<usize>,
    maximum: Option<usize>,
) -> Result<(), PacketFilterError> {
    if minimum.is_some_and(|value| value == 0 || value > MAXIMUM_PACKET_BYTES)
        || maximum.is_some_and(|value| value == 0 || value > MAXIMUM_PACKET_BYTES)
        || matches!((minimum, maximum), (Some(minimum), Some(maximum)) if minimum > maximum)
    {
        return Err(PacketFilterError::InvalidLength);
    }
    Ok(())
}

/// 解析由十六进制字节和 `??` 组成的模式；空字符串表示只匹配元数据。
fn parsePattern(value: &str) -> Result<Option<CompiledBytePattern>, PacketFilterError> {
    parseBytePattern(value, true)
        .map_err(|_| PacketFilterError::InvalidPattern)
        .map(|bytes| bytes.map(|bytes| CompiledBytePattern { bytes }))
}

/// 编译搜索与替换序列；较长替换会把搜索尾部扩展为通配条件，保持网格偏移语义。
/// 失败语义：修改规则任一序列为空或非法时返回对应字段错误，非修改动作禁止隐藏替换值。
fn compileBytePatterns(
    rule: &PacketFilterRule,
) -> Result<(Option<CompiledBytePattern>, Option<CompiledBytePattern>), PacketFilterError> {
    let mut pattern = parsePattern(&rule.pattern)?;
    if rule.action != PacketFilterAction::Modify {
        let replacement = rule
            .replacement
            .trim()
            .is_empty()
            .then_some(None)
            .ok_or(PacketFilterError::InvalidReplacement);
        return replacement.map(|replacement| (pattern, replacement));
    }
    let patternBytes = pattern
        .as_mut()
        .ok_or(PacketFilterError::InvalidReplacement)?;
    let replacement = parseBytePattern(&rule.replacement, true)
        .map_err(|_| PacketFilterError::InvalidReplacement)?
        .ok_or(PacketFilterError::InvalidReplacement)?;
    if replacement.len() > patternBytes.bytes.len() {
        patternBytes.bytes.resize(replacement.len(), None);
    }
    Ok((pattern, Some(CompiledBytePattern { bytes: replacement })))
}

/// 解析空格分隔字节；允许通配符时 `??` 表示匹配或保留当前位置。
fn parseBytePattern(value: &str, allowWildcard: bool) -> Result<Option<Vec<Option<u8>>>, ()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let tokens: Vec<_> = trimmed.split_ascii_whitespace().collect();
    if tokens.is_empty() || tokens.len() > MAXIMUM_PATTERN_TOKENS {
        return Err(());
    }
    tokens
        .into_iter()
        .map(|token| {
            if allowWildcard && token == "??" {
                Ok(None)
            } else if token.len() == 2 {
                u8::from_str_radix(token, 16).map(Some).map_err(|_| ())
            } else {
                Err(())
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// 将域名匹配输入规范化为小写无尾点形式；IP 文本保持等价比较语义。
fn normalizeHost(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}
