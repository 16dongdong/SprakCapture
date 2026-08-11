//! 提供录制接纳与连接拒绝共用的有序规则集。
//!
//! 规则采用“规则集顺序 → 集内规则顺序 → 首个命中”的确定性语义。配置在控制面完成整份编译后
//! 原子替换，数据面只读取预编译匹配器；更新不会重启监听器，也不会改变已经建立的连接。

use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::Arc,
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::BeginTransaction;

const maximumRuleSets: usize = 64;
const maximumRulesPerSet: usize = 1_024;
const maximumIdentifierBytes: usize = 128;
const maximumNameBytes: usize = 256;
const maximumValueBytes: usize = 4_096;

/// 定义一条规则命中后对数据面的处理方式。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingRuleAction {
    Record,
    DoNotRecord,
    Reject,
}

impl Default for RecordingRuleAction {
    /// 首次运行默认完整录制；规则工具未配置时不得改变既有抓包行为。
    fn default() -> Self {
        Self::Record
    }
}

/// 定义可视化编辑器支持的单条件匹配类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingRuleKind {
    Domain,
    DomainSuffix,
    DomainKeyword,
    DestinationIpCidr,
    ClientIpCidr,
    Port,
    ProcessName,
    Protocol,
    Method,
    Match,
}

/// 描述一条有序录制规则；`id` 仅用于稳定编辑，匹配语义只由 kind/value/action 决定。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingRule {
    pub id: String,
    pub enabled: bool,
    pub kind: RecordingRuleKind,
    pub value: String,
    pub action: RecordingRuleAction,
}

/// 聚合一组可整体启停的有序规则，便于按应用、环境或临时诊断场景管理。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingRuleSet {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub rules: Vec<RecordingRule>,
}

/// 保存录制规则工具的完整配置；第一条命中规则优先，未命中时使用 defaultAction。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RecordingRuleConfiguration {
    pub enabled: bool,
    pub defaultAction: RecordingRuleAction,
    pub ruleSets: Vec<RecordingRuleSet>,
}

/// 归类整份规则配置的稳定校验失败；控制层据此拒绝部分生效。
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum RecordingRuleError {
    #[error("recordingRuleTooManySets")]
    TooManySets,
    #[error("recordingRuleTooManyRules")]
    TooManyRules,
    #[error("recordingRuleInvalidIdentifier")]
    InvalidIdentifier,
    #[error("recordingRuleDuplicateIdentifier")]
    DuplicateIdentifier,
    #[error("recordingRuleInvalidName")]
    InvalidName,
    #[error("recordingRuleInvalidValue")]
    InvalidValue,
}

#[derive(Clone)]
enum CompiledMatcher {
    Domain(String),
    DomainSuffix(String),
    DomainKeyword(String),
    DestinationNetwork(IpNetwork),
    ClientNetwork(IpNetwork),
    Port { start: u16, end: u16 },
    ProcessName(String),
    Protocol(String),
    Method(String),
    Match,
}

#[derive(Clone)]
struct CompiledRule {
    matcher: CompiledMatcher,
    action: RecordingRuleAction,
}

#[derive(Clone)]
struct CompiledRuleConfiguration {
    source: RecordingRuleConfiguration,
    rules: Vec<CompiledRule>,
}

/// 缓存一笔事务在规则热路径中反复使用的规范化字段；每笔事务仅解析一次域名与地址，规则数量增长时不会重复分配。
struct CompiledInput<'a> {
    source: &'a BeginTransaction,
    normalizedDomain: String,
    destinationIp: Option<IpAddr>,
    clientIp: Option<IpAddr>,
}

impl<'a> CompiledInput<'a> {
    /// 从事务元数据构造只读匹配视图；无法解析为 IP 的字段保留为 None，仅对应 CIDR 条件不命中。
    fn new(source: &'a BeginTransaction) -> Self {
        let destinationIp = source.location.host.parse::<IpAddr>().ok();
        let clientIp = source
            .clientAddress
            .parse::<SocketAddr>()
            .map(|address| address.ip())
            .or_else(|_| source.clientAddress.parse::<IpAddr>())
            .ok();
        Self {
            source,
            normalizedDomain: normalizeDomain(&source.location.host),
            destinationIp,
            clientIp,
        }
    }
}

/// 保存已掩码的 IPv4/IPv6 网络；u128 表示避免匹配热路径反复分配。
#[derive(Clone, Copy)]
struct IpNetwork {
    address: u128,
    mask: u128,
    ipv6: bool,
}

impl IpNetwork {
    /// 解析 CIDR 并预计算掩码；前缀超出地址族宽度时返回 InvalidValue。
    fn parse(value: &str) -> Result<Self, RecordingRuleError> {
        let (addressText, prefixText) = value
            .split_once('/')
            .ok_or(RecordingRuleError::InvalidValue)?;
        let address =
            IpAddr::from_str(addressText.trim()).map_err(|_| RecordingRuleError::InvalidValue)?;
        let prefix = prefixText
            .trim()
            .parse::<u8>()
            .map_err(|_| RecordingRuleError::InvalidValue)?;
        let (numeric, bits, ipv6) = match address {
            IpAddr::V4(address) => (u32::from(address) as u128, 32_u8, false),
            IpAddr::V6(address) => (u128::from(address), 128_u8, true),
        };
        if prefix > bits {
            return Err(RecordingRuleError::InvalidValue);
        }
        let mask = if prefix == 0 {
            0
        } else {
            u128::MAX << (bits - prefix)
        };
        Ok(Self {
            address: numeric & mask,
            mask,
            ipv6,
        })
    }

    /// 判断地址是否属于同地址族网络；跨 IPv4/IPv6 永不命中。
    fn contains(self, address: IpAddr) -> bool {
        let (numeric, ipv6) = match address {
            IpAddr::V4(address) => (u32::from(address) as u128, false),
            IpAddr::V6(address) => (u128::from(address), true),
        };
        self.ipv6 == ipv6 && numeric & self.mask == self.address
    }
}

/// 提供可跨 HTTP、透明 TCP/TLS 与 UDP 录制路径共享的原子规则快照。
#[derive(Clone)]
pub struct RecordingRuleRuntime {
    compiled: Arc<RwLock<CompiledRuleConfiguration>>,
}

impl RecordingRuleRuntime {
    /// 编译初始配置；任一字段非法时拒绝构造，禁止服务带半份规则启动。
    pub fn new(configuration: RecordingRuleConfiguration) -> Result<Self, RecordingRuleError> {
        Ok(Self {
            compiled: Arc::new(RwLock::new(compileConfiguration(configuration)?)),
        })
    }

    /// 克隆当前公开配置；结果不包含预编译网络掩码或运行期引用。
    pub fn configuration(&self) -> RecordingRuleConfiguration {
        self.compiled.read().source.clone()
    }

    /// 验证并原子替换完整配置；失败时旧快照继续对新连接生效。
    pub fn replaceConfiguration(
        &self,
        configuration: RecordingRuleConfiguration,
    ) -> Result<(), RecordingRuleError> {
        let compiled = compileConfiguration(configuration)?;
        *self.compiled.write() = compiled;
        Ok(())
    }

    /// 对一个即将创建的事务执行首命中裁决；禁用工具时始终完整录制。
    pub fn decision(&self, input: &BeginTransaction) -> RecordingRuleAction {
        let compiled = self.compiled.read();
        if !compiled.source.enabled {
            return RecordingRuleAction::Record;
        }
        let compiledInput = CompiledInput::new(input);
        compiled
            .rules
            .iter()
            .find(|rule| rule.matcher.matches(&compiledInput))
            .map_or(compiled.source.defaultAction, |rule| rule.action)
    }
}

impl CompiledMatcher {
    /// 在不修改输入的情况下匹配事务元数据；域名、协议、方法和进程名均采用 ASCII 不区分大小写语义。
    fn matches(&self, input: &CompiledInput<'_>) -> bool {
        match self {
            Self::Domain(expected) => input.normalizedDomain == *expected,
            Self::DomainSuffix(expected) => {
                let host = input.normalizedDomain.as_str();
                host == expected
                    || host
                        .strip_suffix(expected)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            }
            Self::DomainKeyword(expected) => input.normalizedDomain.contains(expected),
            Self::DestinationNetwork(network) => input
                .destinationIp
                .is_some_and(|address| network.contains(address)),
            Self::ClientNetwork(network) => input
                .clientIp
                .is_some_and(|address| network.contains(address)),
            Self::Port { start, end } => (*start..=*end).contains(&input.source.location.port),
            Self::ProcessName(expected) => input
                .source
                .clientProcessName
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(expected)),
            Self::Protocol(expected) => input
                .source
                .location
                .protocol
                .eq_ignore_ascii_case(expected),
            Self::Method(expected) => input.source.method.eq_ignore_ascii_case(expected),
            Self::Match => true,
        }
    }
}

/// 编译并验证规则集顺序；禁用规则仍校验，避免稍后启用才暴露损坏配置。
fn compileConfiguration(
    configuration: RecordingRuleConfiguration,
) -> Result<CompiledRuleConfiguration, RecordingRuleError> {
    if configuration.ruleSets.len() > maximumRuleSets {
        return Err(RecordingRuleError::TooManySets);
    }
    let mut identifiers = HashSet::new();
    let mut compiledRules = Vec::new();
    for ruleSet in &configuration.ruleSets {
        validateIdentifier(&ruleSet.id, &mut identifiers)?;
        if ruleSet.name.trim().is_empty() || ruleSet.name.len() > maximumNameBytes {
            return Err(RecordingRuleError::InvalidName);
        }
        if ruleSet.rules.len() > maximumRulesPerSet {
            return Err(RecordingRuleError::TooManyRules);
        }
        for rule in &ruleSet.rules {
            validateIdentifier(&rule.id, &mut identifiers)?;
            let matcher = compileMatcher(rule.kind, &rule.value)?;
            if configuration.enabled && ruleSet.enabled && rule.enabled {
                compiledRules.push(CompiledRule {
                    matcher,
                    action: rule.action,
                });
            }
        }
    }
    Ok(CompiledRuleConfiguration {
        source: configuration,
        rules: compiledRules,
    })
}

/// 校验规则与规则集主键并保证整份配置内唯一；重复主键会破坏编辑和排序稳定性。
fn validateIdentifier(
    identifier: &str,
    identifiers: &mut HashSet<String>,
) -> Result<(), RecordingRuleError> {
    if identifier.is_empty()
        || identifier.len() > maximumIdentifierBytes
        || !identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(RecordingRuleError::InvalidIdentifier);
    }
    if !identifiers.insert(identifier.to_owned()) {
        return Err(RecordingRuleError::DuplicateIdentifier);
    }
    Ok(())
}

/// 将可视化字段编译成无歧义匹配器；端口范围与 CIDR 在更新阶段一次性解析。
fn compileMatcher(
    kind: RecordingRuleKind,
    value: &str,
) -> Result<CompiledMatcher, RecordingRuleError> {
    if value.len() > maximumValueBytes {
        return Err(RecordingRuleError::InvalidValue);
    }
    let value = value.trim();
    match kind {
        RecordingRuleKind::Domain => validateDomain(value).map(CompiledMatcher::Domain),
        RecordingRuleKind::DomainSuffix => {
            validateDomain(value.trim_start_matches('.')).map(CompiledMatcher::DomainSuffix)
        }
        RecordingRuleKind::DomainKeyword => {
            nonEmptyLowercase(value).map(CompiledMatcher::DomainKeyword)
        }
        RecordingRuleKind::DestinationIpCidr => {
            IpNetwork::parse(value).map(CompiledMatcher::DestinationNetwork)
        }
        RecordingRuleKind::ClientIpCidr => {
            IpNetwork::parse(value).map(CompiledMatcher::ClientNetwork)
        }
        RecordingRuleKind::Port => {
            let (start, end) = parsePortRange(value)?;
            Ok(CompiledMatcher::Port { start, end })
        }
        RecordingRuleKind::ProcessName => {
            nonEmptyLowercase(value).map(CompiledMatcher::ProcessName)
        }
        RecordingRuleKind::Protocol => nonEmptyLowercase(value).map(CompiledMatcher::Protocol),
        RecordingRuleKind::Method => nonEmptyLowercase(value).map(CompiledMatcher::Method),
        RecordingRuleKind::Match if value.is_empty() => Ok(CompiledMatcher::Match),
        RecordingRuleKind::Match => Err(RecordingRuleError::InvalidValue),
    }
}

/// 规范化域名比较值；尾点只表达 DNS 绝对名，不参与匹配。
fn normalizeDomain(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// 校验域名类字段的基本线格式；通配语义由独立 kind 表达，不允许混入值中。
fn validateDomain(value: &str) -> Result<String, RecordingRuleError> {
    let normalized = normalizeDomain(value);
    if normalized.is_empty()
        || normalized.len() > 253
        || normalized.starts_with('.')
        || normalized.ends_with('.')
        || normalized.contains('*')
        || normalized
            .split('.')
            .any(|label| label.is_empty() || label.len() > 63)
    {
        return Err(RecordingRuleError::InvalidValue);
    }
    Ok(normalized)
}

/// 返回非空 ASCII 小写值；协议、方法和进程名保留标点但拒绝控制字符。
fn nonEmptyLowercase(value: &str) -> Result<String, RecordingRuleError> {
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(RecordingRuleError::InvalidValue);
    }
    Ok(value.to_ascii_lowercase())
}

/// 解析单端口或闭区间；反向范围和零端口均拒绝整份配置。
fn parsePortRange(value: &str) -> Result<(u16, u16), RecordingRuleError> {
    let parsePort = |text: &str| {
        text.trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or(RecordingRuleError::InvalidValue)
    };
    let Some((start, end)) = value.split_once('-') else {
        let port = parsePort(value)?;
        return Ok((port, port));
    };
    let start = parsePort(start)?;
    let end = parsePort(end)?;
    if start > end {
        return Err(RecordingRuleError::InvalidValue);
    }
    Ok((start, end))
}
