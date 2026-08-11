//! 管理完整插件清单、开放运行时实例和持久配置的原子生命周期。
//!
//! legacy ABI 继续由 `PluginHost` 维护；本模块只处理完整 manifest。启用配置提交前先构造新
//! 运行时，提交后再原子发布到 `ExtensionKernel`，任何失败都保留旧实例和旧权威配置。

use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};

use parking_lot::{Mutex, RwLock};
use serde::Serialize;

use crate::{
    ExtensionConfigurationStore, ExtensionKernel, ExtensionManifest, ExtensionRuntime,
    ExtensionRuntimeKind, NativeExtensionRuntime, PluginHostError, PluginPlatformConfiguration,
    PluginUserConfiguration, ProcessExtensionRuntime,
};

const PLUGIN_MANIFEST_FILE_NAME: &str = "plugin.json";

/// 描述完整插件包的控制面状态；manifest 保留作者声明，错误只暴露稳定代码。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPackageSnapshot {
    pub manifest: ExtensionManifest,
    pub enabled: bool,
    pub running: bool,
    pub errorCode: Option<String>,
}

/// 保存一个已发现完整插件包及其版本目录；目录路径不进入公开快照。
struct ExtensionPackage {
    manifest: ExtensionManifest,
    directory: PathBuf,
    errorCode: Option<String>,
}

/// 统一管理完整插件包发现、启停、配置和热替换。
#[derive(Clone)]
pub struct ExtensionManager {
    rootDirectory: Option<Arc<PathBuf>>,
    configuration: ExtensionConfigurationStore,
    kernel: ExtensionKernel,
    packages: Arc<RwLock<BTreeMap<String, ExtensionPackage>>>,
    operationLock: Arc<Mutex<()>>,
}

impl ExtensionManager {
    /// 创建不访问磁盘的空管理器；禁用宿主使用它保持所有调用透明。
    pub fn memory(configuration: ExtensionConfigurationStore, kernel: ExtensionKernel) -> Self {
        Self {
            rootDirectory: None,
            configuration,
            kernel,
            packages: Arc::new(RwLock::new(BTreeMap::new())),
            operationLock: Arc::new(Mutex::new(())),
        }
    }

    /// 扫描插件根目录并恢复已启用完整插件。
    ///
    /// 运行上下文：由 `PluginHost::new` 在控制服务对外可见前调用，和 legacy 目录扫描共享根目录。
    /// 失败语义：目录不可读会阻止宿主启动；单插件清单或运行时失败记录在其快照中，不影响其他插件。
    pub fn open(
        rootDirectory: PathBuf,
        configuration: ExtensionConfigurationStore,
        kernel: ExtensionKernel,
    ) -> Result<Self, PluginHostError> {
        let manager = Self {
            rootDirectory: Some(Arc::new(rootDirectory)),
            configuration,
            kernel,
            packages: Arc::new(RwLock::new(BTreeMap::new())),
            operationLock: Arc::new(Mutex::new(())),
        };
        manager.discover()?;
        Ok(manager)
    }

    /// 返回按插件 ID 稳定排序的完整插件快照；运行状态来自内核实例而非配置猜测。
    pub fn snapshots(&self) -> Vec<ExtensionPackageSnapshot> {
        let running = self
            .kernel
            .snapshots()
            .into_iter()
            .map(|snapshot| snapshot.pluginId)
            .collect::<std::collections::BTreeSet<_>>();
        let configuration = self.configuration.snapshot();
        self.packages
            .read()
            .iter()
            .map(|(pluginId, package)| ExtensionPackageSnapshot {
                manifest: package.manifest.clone(),
                enabled: configuration
                    .plugins
                    .get(pluginId)
                    .is_some_and(|plugin| plugin.enabled),
                running: running.contains(pluginId),
                errorCode: package.errorCode.clone(),
            })
            .collect()
    }

    /// 原子更新完整插件的持久配置和运行实例。
    ///
    /// 运行上下文：控制 API 在串行生命周期操作中调用；启用时先完整加载新实例，再提交磁盘配置。
    /// 未安装插件允许预先保存禁用配置，便于作者先生成配置文件再部署二进制；启用配置必须已经发现插件包。
    /// 失败语义：加载或配置失败不改变磁盘与旧实例；极少数发布失败会恢复旧配置并保留旧实例。
    pub fn updateConfiguration(
        &self,
        pluginId: &str,
        configuration: PluginUserConfiguration,
    ) -> Result<PluginPlatformConfiguration, PluginHostError> {
        let _operationGuard = self.operationLock.lock();
        let package = self.package(pluginId).ok();
        if configuration.enabled && package.is_none() {
            return Err(PluginHostError::NotFound);
        }
        let preparedRuntime = if configuration.enabled {
            let (manifest, directory) = package.as_ref().expect("启用分支已确认完整插件包存在");
            Some(createRuntime(manifest, directory, &configuration)?)
        } else {
            None
        };
        let previous = self.configuration.snapshot().plugins.get(pluginId).cloned();
        let updated = self
            .configuration
            .updatePlugin(pluginId, configuration.clone())?;
        let publishResult = match preparedRuntime {
            Some(runtime) => {
                let (manifest, _) = package.expect("启用分支已确认完整插件包存在");
                self.kernel
                    .register(manifest, configuration.executionOptions(), runtime)
                    .map_err(|_| PluginHostError::InvalidConfiguration)
            }
            None => self
                .kernel
                .remove(pluginId)
                .map(|_| ())
                .map_err(|_| PluginHostError::InvalidConfiguration),
        };
        if let Err(error) = publishResult {
            restoreConfiguration(&self.configuration, pluginId, previous)?;
            return Err(error);
        }
        if let Some(package) = self.packages.write().get_mut(pluginId) {
            package.errorCode = None;
        }
        Ok(updated)
    }

    /// 删除完整插件配置并立即卸载运行实例；插件包仍保留在目录中。
    pub fn removeConfiguration(
        &self,
        pluginId: &str,
    ) -> Result<PluginPlatformConfiguration, PluginHostError> {
        let _operationGuard = self.operationLock.lock();
        let updated = self.configuration.removePlugin(pluginId)?;
        self.kernel
            .remove(pluginId)
            .map_err(|_| PluginHostError::InvalidConfiguration)?;
        Ok(updated)
    }

    /// 扫描所有目录中的完整 manifest，并按持久启用状态恢复运行时。
    fn discover(&self) -> Result<(), PluginHostError> {
        let Some(rootDirectory) = self.rootDirectory.as_ref() else {
            return Ok(());
        };
        let configuration = self.configuration.snapshot();
        for entry in fs::read_dir(rootDirectory.as_ref()).map_err(PluginHostError::Directory)? {
            let entry = entry.map_err(PluginHostError::Directory)?;
            if !entry
                .file_type()
                .map_err(PluginHostError::Directory)?
                .is_dir()
            {
                continue;
            }
            let directory = entry.path();
            let manifestPath = directory.join(PLUGIN_MANIFEST_FILE_NAME);
            if !manifestPath.is_file() {
                continue;
            }
            let bytes = fs::read(&manifestPath).map_err(PluginHostError::Directory)?;
            if !isCompleteManifest(&bytes) {
                continue;
            }
            let manifest = match ExtensionManifest::parse(&bytes) {
                Ok(manifest) => manifest,
                Err(_) => continue,
            };
            let pluginId = manifest.id.clone();
            let mut package = ExtensionPackage {
                manifest: manifest.clone(),
                directory: directory.clone(),
                errorCode: None,
            };
            if let Some(userConfiguration) = configuration.plugins.get(&pluginId)
                && userConfiguration.enabled
            {
                let result =
                    createRuntime(&manifest, &directory, userConfiguration).and_then(|runtime| {
                        self.kernel
                            .register(
                                manifest.clone(),
                                userConfiguration.executionOptions(),
                                runtime,
                            )
                            .map_err(|_| PluginHostError::InvalidConfiguration)
                    });
                if let Err(error) = result {
                    package.errorCode = Some(extensionErrorCode(&error).to_owned());
                }
            }
            self.packages.write().insert(pluginId, package);
        }
        Ok(())
    }

    /// 读取已发现包的清单和目录；返回值克隆后不跨运行时加载持有注册表锁。
    fn package(&self, pluginId: &str) -> Result<(ExtensionManifest, PathBuf), PluginHostError> {
        self.packages
            .read()
            .get(pluginId)
            .map(|package| (package.manifest.clone(), package.directory.clone()))
            .ok_or(PluginHostError::NotFound)
    }
}

/// 判断磁盘 JSON 是否属于完整 manifest；legacy 清单没有 `manifestVersion`。
pub(crate) fn isCompleteManifest(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("manifestVersion")
                .and_then(|value| value.as_u64())
        })
        .is_some()
}

/// 为作者选择的运行时创建生产实例；Native、Sidecar 与 Native Worker 共享同一阶段内核。
fn createRuntime(
    manifest: &ExtensionManifest,
    directory: &std::path::Path,
    configuration: &PluginUserConfiguration,
) -> Result<Arc<dyn ExtensionRuntime>, PluginHostError> {
    match manifest.runtime.kind {
        ExtensionRuntimeKind::Native => {
            NativeExtensionRuntime::load(manifest, directory, &configuration.configuration)
                .map(|runtime| Arc::new(runtime) as Arc<dyn ExtensionRuntime>)
        }
        ExtensionRuntimeKind::Sidecar | ExtensionRuntimeKind::NativeWorker => {
            ProcessExtensionRuntime::load(manifest, directory, &configuration.configuration)
                .map(|runtime| Arc::new(runtime) as Arc<dyn ExtensionRuntime>)
        }
        _ => Err(PluginHostError::UnsupportedRuntime),
    }
}

/// 恢复发布失败前的权威配置；恢复失败优先返回磁盘错误，避免伪称旧配置仍可靠。
fn restoreConfiguration(
    store: &ExtensionConfigurationStore,
    pluginId: &str,
    previous: Option<PluginUserConfiguration>,
) -> Result<(), PluginHostError> {
    match previous {
        Some(configuration) => store.updatePlugin(pluginId, configuration).map(|_| ()),
        None => store.removePlugin(pluginId).map(|_| ()),
    }
}

/// 将完整插件加载错误压缩为控制面稳定码；动态库路径和系统错误不离开宿主日志。
fn extensionErrorCode(error: &PluginHostError) -> &'static str {
    match error {
        PluginHostError::UnsupportedRuntime => "extensionRuntimeUnsupported",
        PluginHostError::MissingEntry => "extensionEntryMissing",
        PluginHostError::Worker(_) => "extensionWorkerStartFailed",
        PluginHostError::InvalidConfiguration => "extensionConfigurationInvalid",
        _ => "extensionRuntimeLoadFailed",
    }
}
