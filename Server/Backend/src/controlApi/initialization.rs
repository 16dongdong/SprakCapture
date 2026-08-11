use std::path::{Path, PathBuf};

use capture_core::CaptureError;
use http_proxy_core::SslMitmError;
use plugin_host::PluginHostError;
use thiserror::Error;

use super::dataDirectory::{
    DataDirectoryError, installationDataDirectory, migrateLegacyDataDirectory,
};

const userDataOverrideVariable: &str = "CAPTURE_USER_DATA_DIR";

/// 聚合控制器启动阶段的目录、证书、插件和持久化配置错误。
///
/// 运行上下文：代理服务在创建任何运行时状态前完成初始化；任一错误都会阻止服务使用不完整的
/// 配置目录继续启动，并向桌面守护进程保留具体失败原因。
#[derive(Debug, Error)]
pub enum ControlInitializationError {
    #[error("初始化录制会话失败：{0}")]
    Capture(#[from] CaptureError),
    #[error("初始化 SSL 证书失败：{0}")]
    Ssl(#[from] SslMitmError),
    #[error("初始化工具映射目录失败：{0}")]
    ToolMappingDirectory(#[from] std::io::Error),
    #[error("初始化协议描述符目录失败：{0}")]
    ProtocolDescriptorDirectory(std::io::Error),
    #[error("初始化插件宿主失败：{0}")]
    PluginHost(#[from] PluginHostError),
    #[error("初始化工具配置失败：{detail}")]
    ToolConfiguration { detail: String },
    #[error("读取代理服务可执行文件路径失败：{0}")]
    ExecutablePath(std::io::Error),
    #[error("准备安装目录数据失败：{0}")]
    DataDirectory(#[from] DataDirectoryError),
    #[error("无法确定旧版用户数据目录")]
    LegacyUserDataDirectory,
}

/// 解析代理服务的权威数据根目录，并在首次升级时迁移旧版用户目录。
///
/// 运行上下文：桌面安装包和独立 `proxyService` 均调用此函数。未显式覆盖时，配置固定写入
/// `proxyService` 所在安装目录的 `data` 子目录，从而避免安装在非系统盘时仍回写 C 盘。
/// `CAPTURE_USER_DATA_DIR` 仅用于开发、便携部署和隔离测试；该覆盖不会触发旧目录迁移。
/// 可执行文件定位或迁移失败时返回精确错误，调用方不得退回用户目录继续运行。
pub fn defaultDataDirectory() -> Result<PathBuf, ControlInitializationError> {
    if let Some(directory) = std::env::var_os(userDataOverrideVariable) {
        return Ok(PathBuf::from(directory));
    }

    let executablePath =
        std::env::current_exe().map_err(ControlInitializationError::ExecutablePath)?;
    let installationDirectory = installationDataDirectory(&executablePath)?;
    let legacyDirectory = directories::ProjectDirs::from("com", "Sprak", "Sprak Capture")
        .ok_or(ControlInitializationError::LegacyUserDataDirectory)?
        .data_local_dir()
        .to_path_buf();
    migrateLegacyDataDirectory(&legacyDirectory, &installationDirectory)?;
    Ok(installationDirectory)
}

/// 从数据根目录构造证书子目录。
///
/// 运行上下文：SSL 管理器在启动时调用；参数是已经完成安装目录解析和旧数据迁移的数据根。
/// 本函数只构造路径，不创建目录；创建失败由证书加载流程返回。
pub fn certificateDirectory(dataDirectory: &Path) -> PathBuf {
    dataDirectory.join("certs")
}
