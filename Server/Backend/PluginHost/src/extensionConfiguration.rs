//! 持久化完整插件平台的启停、顺序、配置、运行说明和版本选择。
//!
//! 插件包目录保持只读，所有用户意图集中写入一个宿主权威文件。每次更新先同步临时文件并
//! 原子替换，再发布内存快照，保证控制 API 返回成功时磁盘与运行态表达同一配置。

use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

use parking_lot::{Mutex, RwLock};
use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{
    ExtensionLimits, ExtensionMatch, FailurePolicy, PluginExecutionOptions, PluginHostError,
    replacePluginFile,
};

const PLATFORM_CONFIGURATION_FILE_NAME: &str = "extensionPlatform.json";
const PLATFORM_CONFIGURATION_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_PLUGINS: usize = 2_048;
const MAXIMUM_SECRET_REFERENCES: usize = 256;
const MAXIMUM_CONFIGURATION_BYTES: usize = 4 * 1024 * 1024;

/// 保存一个插件跨版本保留的用户意图；秘密仅保存宿主秘密库引用，不保存明文。
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginUserConfiguration {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub activeVersion: Option<String>,
    #[serde(default)]
    pub moduleOrder: Vec<String>,
    #[serde(default)]
    pub subscriptionOverrides: BTreeMap<String, ExtensionMatch>,
    #[serde(default)]
    pub failurePolicy: FailurePolicy,
    #[serde(default)]
    pub limits: Option<ExtensionLimits>,
    #[serde(default)]
    pub configurationSchemaVersion: Option<String>,
    #[serde(default)]
    pub configuration: JsonValue,
    #[serde(default)]
    pub secretReferences: BTreeMap<String, String>,
    #[serde(default = "defaultAutomaticRestart")]
    pub automaticRestart: bool,
}

impl Default for PluginUserConfiguration {
    /// 创建默认禁用的用户配置；安装插件本身不会隐式执行第三方代码。
    fn default() -> Self {
        Self {
            enabled: false,
            activeVersion: None,
            moduleOrder: Vec::new(),
            subscriptionOverrides: BTreeMap::new(),
            failurePolicy: FailurePolicy::FailClosed,
            limits: None,
            configurationSchemaVersion: None,
            configuration: JsonValue::Object(Default::default()),
            secretReferences: BTreeMap::new(),
            automaticRestart: true,
        }
    }
}

impl PluginUserConfiguration {
    /// 生成扩展内核使用的执行覆盖；`limits` 只透传为作者说明，不产生宿主执行门禁。
    pub fn executionOptions(&self) -> PluginExecutionOptions {
        PluginExecutionOptions {
            moduleOrder: self.moduleOrder.clone(),
            subscriptionOverrides: self.subscriptionOverrides.clone(),
            failurePolicy: self.failurePolicy,
            limits: self.limits.clone(),
        }
    }
}

/// 保存完整插件平台配置文件；Schema 版本用于未来执行一次性、可回滚的数据迁移。
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginPlatformConfiguration {
    pub schemaVersion: u32,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginUserConfiguration>,
}

impl Default for PluginPlatformConfiguration {
    /// 创建当前 Schema 的空配置；不会为未安装插件生成幽灵条目。
    fn default() -> Self {
        Self {
            schemaVersion: PLATFORM_CONFIGURATION_SCHEMA_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

/// 提供线程安全且原子持久化的插件平台配置存储。
#[derive(Clone)]
pub struct ExtensionConfigurationStore {
    directory: Option<Arc<std::path::PathBuf>>,
    configuration: Arc<RwLock<PluginPlatformConfiguration>>,
    operationLock: Arc<Mutex<()>>,
}

impl ExtensionConfigurationStore {
    /// 创建仅驻留内存的配置存储；独立库测试和禁用宿主不会访问用户目录。
    pub fn memory() -> Self {
        Self {
            directory: None,
            configuration: Arc::new(RwLock::new(PluginPlatformConfiguration::default())),
            operationLock: Arc::new(Mutex::new(())),
        }
    }

    /// 从插件根目录加载权威配置；损坏、未知版本或超出预算时阻止宿主启动。
    pub fn open(directory: &Path) -> Result<Self, PluginHostError> {
        fs::create_dir_all(directory).map_err(PluginHostError::Directory)?;
        let path = directory.join(PLATFORM_CONFIGURATION_FILE_NAME);
        let configuration = if path.exists() {
            let bytes = fs::read(path).map_err(PluginHostError::State)?;
            if bytes.len() > MAXIMUM_CONFIGURATION_BYTES {
                return Err(PluginHostError::InvalidConfiguration);
            }
            serde_json::from_slice(bytes.as_slice()).map_err(PluginHostError::StateFormat)?
        } else {
            PluginPlatformConfiguration::default()
        };
        validateConfiguration(&configuration)?;
        Ok(Self {
            directory: Some(Arc::new(directory.to_path_buf())),
            configuration: Arc::new(RwLock::new(configuration)),
            operationLock: Arc::new(Mutex::new(())),
        })
    }

    /// 返回当前不可变配置快照；调用方修改返回值不会影响权威状态。
    pub fn snapshot(&self) -> PluginPlatformConfiguration {
        self.configuration.read().clone()
    }

    /// 原子替换单插件配置；磁盘提交失败时内存继续保留旧配置。
    pub fn updatePlugin(
        &self,
        pluginId: &str,
        configuration: PluginUserConfiguration,
    ) -> Result<PluginPlatformConfiguration, PluginHostError> {
        validatePluginIdentifier(pluginId)?;
        validatePluginConfiguration(&configuration)?;
        self.commit(|document| {
            document.plugins.insert(pluginId.to_owned(), configuration);
        })
    }

    /// 删除已卸载插件的用户配置；不存在的条目按幂等成功处理。
    pub fn removePlugin(
        &self,
        pluginId: &str,
    ) -> Result<PluginPlatformConfiguration, PluginHostError> {
        validatePluginIdentifier(pluginId)?;
        self.commit(|document| {
            document.plugins.remove(pluginId);
        })
    }

    /// 在单一操作锁内构造、校验、持久化并发布新快照；闭包不得执行外部 I/O。
    fn commit(
        &self,
        mutate: impl FnOnce(&mut PluginPlatformConfiguration),
    ) -> Result<PluginPlatformConfiguration, PluginHostError> {
        let _operationGuard = self.operationLock.lock();
        let mut next = self.configuration.read().clone();
        mutate(&mut next);
        validateConfiguration(&next)?;
        let bytes = serde_json::to_vec_pretty(&next).map_err(PluginHostError::StateFormat)?;
        if bytes.len() > MAXIMUM_CONFIGURATION_BYTES {
            return Err(PluginHostError::InvalidConfiguration);
        }
        if let Some(directory) = self.directory.as_ref() {
            replacePluginFile(directory, PLATFORM_CONFIGURATION_FILE_NAME, &bytes)?;
        }
        *self.configuration.write() = next.clone();
        Ok(next)
    }
}

/// 校验配置文件版本、插件数量和每个插件的独立资源边界。
fn validateConfiguration(
    configuration: &PluginPlatformConfiguration,
) -> Result<(), PluginHostError> {
    if configuration.schemaVersion != PLATFORM_CONFIGURATION_SCHEMA_VERSION
        || configuration.plugins.len() > MAXIMUM_PLUGINS
    {
        return Err(PluginHostError::InvalidConfiguration);
    }
    for (pluginId, plugin) in &configuration.plugins {
        validatePluginIdentifier(pluginId)?;
        validatePluginConfiguration(plugin)?;
    }
    Ok(())
}

/// 校验用户配置中可能混淆身份或破坏配置格式的集合；不对插件公开能力实施授权门禁。
fn validatePluginConfiguration(
    configuration: &PluginUserConfiguration,
) -> Result<(), PluginHostError> {
    let uniqueModules = configuration
        .moduleOrder
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let configurationBytes = serde_json::to_vec(&configuration.configuration)
        .map_err(PluginHostError::StateFormat)?
        .len();
    if uniqueModules.len() != configuration.moduleOrder.len()
        || configurationBytes > MAXIMUM_CONFIGURATION_BYTES
        || configuration
            .activeVersion
            .as_deref()
            .is_some_and(|version| Version::parse(version).is_err())
        || configuration
            .configurationSchemaVersion
            .as_deref()
            .is_some_and(|version| version.is_empty() || version.len() > 128)
        || configuration
            .moduleOrder
            .iter()
            .any(|moduleId| !validConfigurationIdentifier(moduleId))
        || configuration.subscriptionOverrides.iter().any(|(key, _)| {
            key.len() > 320
                || key.split_once('.').is_none_or(|(moduleId, stage)| {
                    !validConfigurationIdentifier(moduleId) || !validConfigurationIdentifier(stage)
                })
        })
        || configuration.secretReferences.len() > MAXIMUM_SECRET_REFERENCES
        || configuration
            .secretReferences
            .iter()
            .any(|(field, reference)| {
                !validConfigurationIdentifier(field)
                    || !reference.starts_with("secret://")
                    || reference.len() > 2_048
            })
    {
        return Err(PluginHostError::InvalidConfiguration);
    }
    Ok(())
}

/// 校验配置键中的插件标识；该键必须与清单 ID 使用同一稳定字符集。
fn validatePluginIdentifier(pluginId: &str) -> Result<(), PluginHostError> {
    validConfigurationIdentifier(pluginId)
        .then_some(())
        .ok_or(PluginHostError::InvalidConfiguration)
}

/// 校验进入持久配置键的稳定标识；该字符集可安全用于模块、字段和阶段覆盖键。
fn validConfigurationIdentifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier.len() <= 128
        && identifier
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, b'.' | b'-'))
}

/// 返回自动重启的持久化默认值；显式 false 在反序列化时保持不变。
const fn defaultAutomaticRestart() -> bool {
    true
}
