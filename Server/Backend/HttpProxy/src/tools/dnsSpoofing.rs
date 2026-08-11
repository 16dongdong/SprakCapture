use std::{collections::HashSet, net::IpAddr, sync::Arc};

use location_core::{
    LocationMatchOptions, LocationPattern, ResolvedLocation, locationMatches,
    validateLocationPattern,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const maxDnsSpoofingRules: usize = 2_000;

/// 描述单条代理进程内的域名解析替换规则；`hostPattern` 使用统一 Location 通配语义。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DnsSpoofingRule {
    pub id: String,
    pub enabled: bool,
    pub hostPattern: String,
    pub ipAddress: String,
}

/// 保存 DNS 映射总开关和有序规则；多条命中时仅第一条生效，避免同一连接产生不确定目标。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct DnsSpoofingConfiguration {
    pub enabled: bool,
    pub rules: Vec<DnsSpoofingRule>,
}

impl DnsSpoofingConfiguration {
    /// 校验规则数量、标识、主机模式和目标地址；持久化层可在发布配置前复用完整语义检查。
    pub fn validate(&self) -> Result<(), DnsSpoofingError> {
        validateConfiguration(self)
    }
}

/// 表示 DNS 配置在进入热路径前发现的稳定校验错误；更新失败时旧快照保持不变。
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DnsSpoofingError {
    #[error("DNS 规则数量超过上限")]
    TooManyRules,
    #[error("DNS 规则标识无效")]
    InvalidRuleId,
    #[error("DNS 规则标识重复")]
    DuplicateRuleId,
    #[error("DNS 主机模式无效")]
    InvalidHostPattern,
    #[error("DNS 目标 IP 无效")]
    InvalidIpAddress,
}

/// 为 HTTP、HTTPS 与 SOCKS5 出站共享同一份可热更新 DNS 映射快照。
///
/// 运行上下文：控制面只在配置更新时取得写锁；每次出站解析取得短暂读锁并线性扫描规则。
/// 失败语义：无规则命中返回 `None`，调用方必须继续使用系统 DNS，而不是把它当作解析失败。
#[derive(Clone, Debug)]
pub struct DnsSpoofingTool {
    configuration: Arc<RwLock<DnsSpoofingConfiguration>>,
}

impl DnsSpoofingTool {
    /// 校验初始配置并创建可共享工具；非法规则不会进入运行时解析路径。
    pub fn new(configuration: DnsSpoofingConfiguration) -> Result<Self, DnsSpoofingError> {
        validateConfiguration(&configuration)?;
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
        })
    }

    /// 返回当前完整配置快照，供控制 API、WebSocket 和界面编辑器使用。
    pub fn configuration(&self) -> DnsSpoofingConfiguration {
        self.configuration.read().clone()
    }

    /// 完整校验后原子替换配置；失败时不会暴露部分规则或改变当前解析行为。
    pub fn replaceConfiguration(
        &self,
        configuration: DnsSpoofingConfiguration,
    ) -> Result<(), DnsSpoofingError> {
        validateConfiguration(&configuration)?;
        *self.configuration.write() = configuration;
        Ok(())
    }

    /// 按规则顺序解析主机名覆盖值；IP 字面量与未命中域名均交还调用方按原路径处理。
    pub fn resolveIp(&self, host: &str) -> Option<IpAddr> {
        if host.parse::<IpAddr>().is_ok() {
            return None;
        }
        let configuration = self.configuration.read();
        if !configuration.enabled {
            return None;
        }
        configuration.rules.iter().find_map(|rule| {
            if !rule.enabled || !hostMatches(&rule.hostPattern, host) {
                return None;
            }
            rule.ipAddress.parse().ok()
        })
    }
}

/// 在配置进入共享快照前验证数量、标识、主机模式和目标 IP，保证高频解析无需重复处理错误。
fn validateConfiguration(configuration: &DnsSpoofingConfiguration) -> Result<(), DnsSpoofingError> {
    if configuration.rules.len() > maxDnsSpoofingRules {
        return Err(DnsSpoofingError::TooManyRules);
    }
    let mut ruleIds = HashSet::with_capacity(configuration.rules.len());
    for rule in &configuration.rules {
        if rule.id.trim().is_empty() {
            return Err(DnsSpoofingError::InvalidRuleId);
        }
        if !ruleIds.insert(rule.id.as_str()) {
            return Err(DnsSpoofingError::DuplicateRuleId);
        }
        let pattern = LocationPattern {
            protocol: "*".to_owned(),
            host: rule.hostPattern.clone(),
            port: "*".to_owned(),
            path: String::new(),
            query: None,
        };
        if rule.hostPattern.trim().is_empty() || validateLocationPattern(&pattern).is_err() {
            return Err(DnsSpoofingError::InvalidHostPattern);
        }
        rule.ipAddress
            .parse::<IpAddr>()
            .map_err(|_| DnsSpoofingError::InvalidIpAddress)?;
    }
    Ok(())
}

/// 复用 Location 的线性通配匹配器，使 DNS、映射和其他工具对 `*`、`?` 与大小写保持一致。
fn hostMatches(pattern: &str, host: &str) -> bool {
    let pattern = LocationPattern {
        protocol: "*".to_owned(),
        host: pattern.to_owned(),
        port: "*".to_owned(),
        path: String::new(),
        query: None,
    };
    let candidate = ResolvedLocation {
        protocol: "socks".to_owned(),
        host: host.to_owned(),
        port: 1,
        path: String::new(),
        query: String::new(),
        display: host.to_owned(),
    };
    locationMatches(&pattern, &candidate, LocationMatchOptions::default()).unwrap_or(false)
}
