#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
// 集成测试按路径复用桌面偏好模块；测试目录位于系统临时区并在守卫析构时物理删除。
#![allow(dead_code)]

use std::{fs, path::PathBuf};

#[path = "../src/closePreference.rs"]
mod closePreference;

use closePreference::{ClosePreferenceStore, MainWindowCloseAction};

/// 为每个测试创建独立安装目录；Drop 保证断言失败时也不遗留配置夹具。
struct TemporaryInstallation {
    path: PathBuf,
}

impl TemporaryInstallation {
    /// 使用进程与时间戳生成系统临时目录，避免并行测试共享同一偏好文件。
    fn create(testName: &str) -> Self {
        let unique = format!(
            "sprak-close-preference-{testName}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时间早于 UNIX 纪元")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("创建临时安装目录失败");
        Self { path }
    }
}

impl Drop for TemporaryInstallation {
    /// 删除测试产生的安装目录和配置文件；失败立即暴露为测试线程 panic。
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).expect("清理临时安装目录失败");
        }
    }
}

/// 验证首次启动没有隐式选择，勾选记住后文件位于安装目录且重启可恢复同一动作。
#[test]
fn remembersCloseActionInsideInstallationDirectory() {
    let installation = TemporaryInstallation::create("round-trip");
    let store = ClosePreferenceStore::load(&installation.path).expect("加载首次偏好失败");
    assert_eq!(store.rememberedAction().expect("读取首次偏好失败"), None);

    store
        .remember(MainWindowCloseAction::EnterTray)
        .expect("保存关闭偏好失败");
    let preferencePath = installation
        .path
        .join("data")
        .join("desktopPreferences.json");
    assert!(preferencePath.is_file());

    let reopened = ClosePreferenceStore::load(&installation.path).expect("重新加载偏好失败");
    assert_eq!(
        reopened.rememberedAction().expect("读取已保存偏好失败"),
        Some(MainWindowCloseAction::EnterTray)
    );
}

/// 验证直接退出选择可覆盖已有值，原子替换后内存和磁盘同时观察到新动作。
#[test]
fn atomicallyReplacesRememberedCloseAction() {
    let installation = TemporaryInstallation::create("replace");
    let store = ClosePreferenceStore::load(&installation.path).expect("加载偏好失败");
    store
        .remember(MainWindowCloseAction::EnterTray)
        .expect("保存托盘偏好失败");
    store
        .remember(MainWindowCloseAction::ExitApplication)
        .expect("替换退出偏好失败");

    let reopened = ClosePreferenceStore::load(&installation.path).expect("重新加载偏好失败");
    assert_eq!(
        reopened.rememberedAction().expect("读取退出偏好失败"),
        Some(MainWindowCloseAction::ExitApplication)
    );
}

/// 验证损坏或超出预算的配置会显式失败，不会静默采用托盘或退出动作。
#[test]
fn rejectsInvalidPreferenceFiles() {
    let installation = TemporaryInstallation::create("invalid");
    let dataDirectory = installation.path.join("data");
    fs::create_dir_all(&dataDirectory).expect("创建数据目录失败");
    fs::write(dataDirectory.join("desktopPreferences.json"), b"not-json")
        .expect("写入损坏偏好失败");

    let Err(error) = ClosePreferenceStore::load(&installation.path) else {
        panic!("损坏偏好不应加载成功");
    };
    assert!(error.to_string().contains("解析桌面偏好"));
}
