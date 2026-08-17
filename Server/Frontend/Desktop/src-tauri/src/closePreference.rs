use std::{
    error::Error,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

const applicationDataDirectoryName: &str = "data";
const desktopPreferenceFileName: &str = "desktopPreferences.json";
const maximumPreferenceFileBytes: u64 = 64 * 1024;

/// 表示用户关闭主窗口时选择的稳定动作；序列化值属于安装目录配置合同，不能随界面文案变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MainWindowCloseAction {
    EnterTray,
    ExitApplication,
}

/// 保存桌面外壳独占的偏好；代理配置由后端管理，因此关闭行为单独持久化，避免两个进程并发覆盖同一文件。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopPreferences {
    mainWindowCloseAction: Option<MainWindowCloseAction>,
}

/// 表示偏好路径、格式或原子提交失败；错误保留文件位置，便于定位安装目录权限和损坏配置。
#[derive(Debug)]
pub struct ClosePreferenceError {
    message: String,
}

impl ClosePreferenceError {
    /// 使用操作、路径和底层原因构造稳定中文诊断，不把读取、解析和写盘错误合并为同一种失败。
    fn new(operation: &str, path: &Path, source: impl fmt::Display) -> Self {
        Self {
            message: format!("{operation} `{}` 失败：{source}", path.display()),
        }
    }

    /// 构造内存状态锁损坏诊断；此时不能继续读写偏好，避免向磁盘提交不确定状态。
    fn poisoned() -> Self {
        Self {
            message: "桌面关闭偏好状态锁已损坏".to_owned(),
        }
    }
}

impl fmt::Display for ClosePreferenceError {
    /// 输出包含操作对象的完整中文错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ClosePreferenceError {}

/// 管理安装目录中的桌面偏好；内存值只在原子写盘成功后更新，保证当前行为与重启后行为一致。
pub struct ClosePreferenceStore {
    filePath: PathBuf,
    rememberedAction: Mutex<Option<MainWindowCloseAction>>,
}

impl ClosePreferenceStore {
    /// 从安装资源目录加载关闭偏好；文件不存在表示首次使用，格式或大小异常会阻止静默采用错误动作。
    pub fn load(installationDirectory: &Path) -> Result<Self, ClosePreferenceError> {
        let filePath = preferenceFilePath(installationDirectory);
        let rememberedAction = readPreferences(&filePath)?.mainWindowCloseAction;
        Ok(Self {
            filePath,
            rememberedAction: Mutex::new(rememberedAction),
        })
    }

    /// 返回当前已记住的动作；未选择或未勾选“记住”时返回 `None`，调用方必须再次显示询问框。
    pub fn rememberedAction(&self) -> Result<Option<MainWindowCloseAction>, ClosePreferenceError> {
        self.rememberedAction
            .lock()
            .map(|action| *action)
            .map_err(|_| ClosePreferenceError::poisoned())
    }

    /// 原子保存用户确认的动作；写盘失败时保留原内存值并返回错误，禁止本次运行与下次启动行为分叉。
    pub fn remember(&self, action: MainWindowCloseAction) -> Result<(), ClosePreferenceError> {
        let mut rememberedAction = self
            .rememberedAction
            .lock()
            .map_err(|_| ClosePreferenceError::poisoned())?;
        let preferences = DesktopPreferences {
            mainWindowCloseAction: Some(action),
        };
        writePreferences(&self.filePath, &preferences)?;
        *rememberedAction = Some(action);
        drop(rememberedAction);
        Ok(())
    }
}

/// 返回安装目录内固定配置位置；桌面偏好与后端数据同处 `data`，不会写入系统盘用户目录。
fn preferenceFilePath(installationDirectory: &Path) -> PathBuf {
    installationDirectory
        .join(applicationDataDirectoryName)
        .join(desktopPreferenceFileName)
}

/// 读取并校验偏好文件；限制文件大小可避免损坏配置导致桌面启动时无界分配。
fn readPreferences(filePath: &Path) -> Result<DesktopPreferences, ClosePreferenceError> {
    let metadata = match fs::metadata(filePath) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DesktopPreferences::default());
        }
        Err(error) => {
            return Err(ClosePreferenceError::new(
                "读取桌面偏好属性",
                filePath,
                error,
            ));
        }
    };
    if !metadata.is_file() || metadata.len() > maximumPreferenceFileBytes {
        return Err(ClosePreferenceError::new(
            "校验桌面偏好",
            filePath,
            "目标不是普通小型配置文件",
        ));
    }
    let bytes = fs::read(filePath)
        .map_err(|error| ClosePreferenceError::new("读取桌面偏好", filePath, error))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| ClosePreferenceError::new("解析桌面偏好", filePath, error))
}

/// 在同目录同步临时文件后原子替换权威配置；崩溃只会留下 `.next`，不会截断已生效的选择。
fn writePreferences(
    filePath: &Path,
    preferences: &DesktopPreferences,
) -> Result<(), ClosePreferenceError> {
    let parentDirectory = filePath.parent().ok_or_else(|| {
        ClosePreferenceError::new("定位桌面偏好目录", filePath, "配置路径缺少父目录")
    })?;
    fs::create_dir_all(parentDirectory)
        .map_err(|error| ClosePreferenceError::new("创建桌面偏好目录", parentDirectory, error))?;
    let bytes = serde_json::to_vec_pretty(preferences)
        .map_err(|error| ClosePreferenceError::new("序列化桌面偏好", filePath, error))?;
    let nextPath = filePath.with_extension("json.next");
    {
        let mut nextFile = fs::File::create(&nextPath)
            .map_err(|error| ClosePreferenceError::new("创建桌面偏好临时文件", &nextPath, error))?;
        nextFile
            .write_all(&bytes)
            .and_then(|()| nextFile.sync_all())
            .map_err(|error| ClosePreferenceError::new("写入桌面偏好临时文件", &nextPath, error))?;
    }
    replacePreferenceFile(&nextPath, filePath)
        .map_err(|error| ClosePreferenceError::new("提交桌面偏好", filePath, error))
}

/// Windows 使用写穿透覆盖，确保勾选“记住”后掉电重启仍能看到同一选择。
#[cfg(windows)]
fn replacePreferenceFile(nextPath: &Path, destinationPath: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

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
    let succeeded = unsafe {
        MoveFileExW(
            nextWide.as_ptr(),
            destinationWide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// 非 Windows 文件系统使用同目录原子重命名并同步目录项，保持测试和未来端口的持久化语义一致。
#[cfg(not(windows))]
fn replacePreferenceFile(nextPath: &Path, destinationPath: &Path) -> Result<(), std::io::Error> {
    fs::rename(nextPath, destinationPath)?;
    let parentDirectory = destinationPath
        .parent()
        .ok_or_else(|| std::io::Error::other("桌面偏好目标缺少父目录"))?;
    fs::File::open(parentDirectory)?.sync_all()
}
