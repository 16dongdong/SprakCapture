use std::{
    fs,
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
};

use thiserror::Error;
use uuid::Uuid;

const applicationDataDirectoryName: &str = "data";
const configurationFileName: &str = "configuration.json";

/// 表示安装目录定位、旧数据迁移或迁移提交阶段的确定性失败。
#[derive(Debug, Error)]
pub enum DataDirectoryError {
    #[error("可执行文件路径没有有效的安装目录：{executablePath}")]
    MissingInstallationDirectory { executablePath: PathBuf },
    #[error("安装数据目录没有有效的父目录：{directory}")]
    MissingDataParent { directory: PathBuf },
    #[error("目标数据目录已有内容但缺少 configuration.json，拒绝覆盖：{directory}")]
    MigrationConflict { directory: PathBuf },
    #[error("迁移数据目录失败（{operation}，{path}）：{source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// 根据代理服务可执行文件定位安装目录中的 `data` 子目录。
///
/// 运行上下文：路径来自 `current_exe` 或桌面安装器生成的绝对路径；返回值承载配置、证书、
/// 插件和录制文件。参数缺少有效父目录时返回错误，禁止退回系统盘用户目录。
pub(crate) fn installationDataDirectory(
    executablePath: &Path,
) -> Result<PathBuf, DataDirectoryError> {
    let Some(parentDirectory) = executablePath
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
    else {
        return Err(DataDirectoryError::MissingInstallationDirectory {
            executablePath: executablePath.to_path_buf(),
        });
    };
    Ok(parentDirectory.join(applicationDataDirectoryName))
}

/// 在安装目录首次启用时迁移旧版用户数据目录。
///
/// 运行上下文：仅默认路径解析调用。已有安装目录配置始终优先；旧目录不存在时保持无操作。
/// 同卷优先使用目录重命名，跨卷则先完整复制到安装目录旁的唯一暂存目录，再原子提交。
/// 复制或提交失败时不会发布半成品目录；迁移成功但旧备份清理失败只输出诊断，新目录仍是
/// 唯一权威位置，避免因为备份被占用而回退到 C 盘继续写入。
pub(crate) fn migrateLegacyDataDirectory(
    legacyDirectory: &Path,
    installationDirectory: &Path,
) -> Result<(), DataDirectoryError> {
    if legacyDirectory == installationDirectory
        || installationDirectory.join(configurationFileName).is_file()
        || !legacyDirectory.join(configurationFileName).is_file()
    {
        return Ok(());
    }

    let installationParent =
        installationDirectory
            .parent()
            .ok_or_else(|| DataDirectoryError::MissingDataParent {
                directory: installationDirectory.to_path_buf(),
            })?;
    fs::create_dir_all(installationParent).map_err(|source| DataDirectoryError::Io {
        operation: "创建安装目录",
        path: installationParent.to_path_buf(),
        source,
    })?;
    prepareEmptyDestination(installationDirectory)?;
    if fs::rename(legacyDirectory, installationDirectory).is_ok() {
        return Ok(());
    }
    if installationDirectory.join(configurationFileName).is_file() {
        return Ok(());
    }

    let stagingDirectory = installationDirectory.with_file_name(format!(
        ".{applicationDataDirectoryName}-migration-{}",
        Uuid::new_v4()
    ));
    if let Err(error) = copyDirectoryTree(legacyDirectory, &stagingDirectory) {
        let _ = fs::remove_dir_all(&stagingDirectory);
        if installationDirectory.join(configurationFileName).is_file() {
            return Ok(());
        }
        return Err(error);
    }
    if let Err(source) = fs::rename(&stagingDirectory, installationDirectory) {
        // 两个守护进程同时升级时只允许首个原子提交；后到实例确认权威配置已存在后清理自己的
        // 暂存目录并继续，避免把正常竞争误报成启动故障，也不允许覆盖首个实例的完整结果。
        let installationWasCommitted = installationDirectory.join(configurationFileName).is_file();
        let _ = fs::remove_dir_all(&stagingDirectory);
        if !installationWasCommitted {
            return Err(DataDirectoryError::Io {
                operation: "提交安装目录数据",
                path: installationDirectory.to_path_buf(),
                source,
            });
        }
    }

    if let Err(error) = fs::remove_dir_all(legacyDirectory) {
        eprintln!(
            "安装目录数据迁移已完成，但清理旧版用户数据备份失败（{}）：{}",
            legacyDirectory.display(),
            error
        );
    }
    Ok(())
}

/// 确保迁移目标不存在或仅为空目录，避免覆盖未知安装数据。
///
/// 运行上下文：迁移提交前调用；非空且没有权威配置的目录表示异常中间状态，函数返回冲突
/// 错误并保留全部文件。读取或删除空目录失败时返回对应文件系统错误。
fn prepareEmptyDestination(installationDirectory: &Path) -> Result<(), DataDirectoryError> {
    if !installationDirectory.exists() {
        return Ok(());
    }
    if installationDirectory.join(configurationFileName).is_file() {
        return Ok(());
    }
    let mut entries =
        fs::read_dir(installationDirectory).map_err(|source| DataDirectoryError::Io {
            operation: "检查安装目录数据",
            path: installationDirectory.to_path_buf(),
            source,
        })?;
    if entries
        .next()
        .transpose()
        .map_err(|source| DataDirectoryError::Io {
            operation: "读取安装目录数据",
            path: installationDirectory.to_path_buf(),
            source,
        })?
        .is_some()
    {
        return Err(DataDirectoryError::MigrationConflict {
            directory: installationDirectory.to_path_buf(),
        });
    }
    fs::remove_dir(installationDirectory).map_err(|source| DataDirectoryError::Io {
        operation: "移除空的安装数据目录",
        path: installationDirectory.to_path_buf(),
        source,
    })
}

/// 将完整目录树复制到尚未发布的迁移暂存目录。
///
/// 运行上下文：仅跨卷迁移使用。普通文件逐个复制并同步，目录递归创建；符号链接及其它特殊
/// 文件会被明确拒绝，防止迁移越过旧数据根或生成无法复现的安装布局。
pub(crate) fn copyDirectoryTree(
    sourceDirectory: &Path,
    targetDirectory: &Path,
) -> Result<(), DataDirectoryError> {
    fs::create_dir(targetDirectory).map_err(|source| DataDirectoryError::Io {
        operation: "创建迁移暂存目录",
        path: targetDirectory.to_path_buf(),
        source,
    })?;
    let entries = fs::read_dir(sourceDirectory).map_err(|source| DataDirectoryError::Io {
        operation: "读取旧版用户数据目录",
        path: sourceDirectory.to_path_buf(),
        source,
    })?;
    for entryResult in entries {
        let entry = entryResult.map_err(|source| DataDirectoryError::Io {
            operation: "读取旧版用户数据项",
            path: sourceDirectory.to_path_buf(),
            source,
        })?;
        let sourcePath = entry.path();
        let targetPath = targetDirectory.join(entry.file_name());
        let fileType = entry.file_type().map_err(|source| DataDirectoryError::Io {
            operation: "读取旧版用户数据类型",
            path: sourcePath.clone(),
            source,
        })?;
        if fileType.is_dir() {
            copyDirectoryTree(&sourcePath, &targetPath)?;
            continue;
        }
        if !fileType.is_file() {
            return Err(DataDirectoryError::Io {
                operation: "拒绝迁移特殊文件",
                path: sourcePath,
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "数据目录包含符号链接或特殊文件",
                ),
            });
        }
        fs::copy(&sourcePath, &targetPath).map_err(|source| DataDirectoryError::Io {
            operation: "复制旧版用户数据文件",
            path: sourcePath,
            source,
        })?;
        OpenOptions::new()
            .write(true)
            .open(&targetPath)
            .and_then(|file| file.sync_all())
            .map_err(|source| DataDirectoryError::Io {
                operation: "同步迁移数据文件",
                path: targetPath,
                source,
            })?;
    }
    Ok(())
}
