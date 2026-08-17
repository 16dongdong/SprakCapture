use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tempfile::TempDir;
use tokio::{fs, process::Command, sync::mpsc, time::sleep};
use uuid::Uuid;

#[cfg(target_os = "windows")]
use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt, process::Stdio, time::Duration};

use super::*;

/// 进程重启必须删除上次中断遗留的 APK，同时恢复不含秘密的生成记录。
#[tokio::test]
async fn loadRemovesOrphanedApkAndKeepsMetadataRecord() {
    let directory = TempDir::new().expect("创建客户端记录测试目录");
    let packageDirectory = directory.path().join(packageDirectoryName);
    std::fs::create_dir_all(&packageDirectory).expect("创建客户端产物目录");
    let id = Uuid::new_v4();
    let artifact = ClientPackageArtifact {
        id,
        applicationId: "abc.def.ghi".to_owned(),
        applicationName: "Abc".to_owned(),
        createdAtMilliseconds: 1,
        fileName: packageFileName(id),
        sizeBytes: 1024,
        sha256: "0".repeat(64),
    };
    std::fs::write(
        metadataPath(&packageDirectory, id),
        serde_json::to_vec(&artifact).expect("序列化生成记录"),
    )
    .expect("写入生成记录");
    let orphanedApk = packageDirectory.join(&artifact.fileName);
    std::fs::write(&orphanedApk, b"PK\x03\x04orphaned-secret-package").expect("写入中断遗留 APK");

    let manager = ClientPackageManager::load(directory.path()).expect("恢复客户端生成记录");
    assert!(!orphanedApk.exists(), "启动后不得保留含凭据 APK");
    let snapshot = manager.snapshot().await;
    assert_eq!(snapshot.packages.len(), 1);
    assert_eq!(snapshot.packages[0].id, id);
}

/// 客户端中途停止读取时发送任务必须退出、关闭文件并释放下一次生成资格。
#[tokio::test]
async fn droppedDownloadReceiverRemovesTemporaryPackage() {
    let directory = TempDir::new().expect("创建临时 APK 测试目录");
    let manager = ClientPackageManager::load(directory.path()).expect("创建客户端包管理器");
    let packagePath = manager.packageDirectory.join("generated.apk");
    fs::write(
        &packagePath,
        vec![0x5a; clientDownloadChunkBytes * (clientDownloadBufferChunks + 2)],
    )
    .await
    .expect("写入临时 APK");
    let operationActive = Arc::new(AtomicBool::new(true));
    let temporaryPackage = TemporaryClientPackage::new(
        packagePath.clone(),
        manager,
        ClientPackageOperationLease {
            operationActive: Arc::clone(&operationActive),
            releaseOnDrop: true,
        },
    );
    let file = fs::File::open(&packagePath).await.expect("打开临时 APK");
    let (sender, mut receiver) = mpsc::channel(1);
    let task = tokio::spawn(streamTemporaryPackage(file, temporaryPackage, sender));
    receiver
        .recv()
        .await
        .expect("读取首个 APK 分块")
        .expect("首个分块有效");
    drop(receiver);
    task.await.expect("流式任务应完成清理");

    assert!(!packagePath.exists(), "客户端断流后必须删除临时 APK");
    assert!(!operationActive.load(Ordering::Acquire));
}

/// 规则 URL 必须使用只在已认证 SOCKS5 内部解析的保留域名，避免服务器依赖公网 NAT 回流。
#[test]
fn clientRulesUrlUsesInternalSocksMapping() {
    assert_eq!(
        clientRulesUrl(19_090),
        "http://client-rules.internal.invalid:19090/api/v1/client/routing.txt"
    );
}

/// 公网节点判定必须排除局域网、文档和链路本地地址，避免自动生成只能在开发网络使用的 APK。
#[test]
fn packagedNodeRequiresExternallyRoutableAddress() {
    assert!(isPublicClientNodeAddress("8.8.8.8".parse().unwrap()));
    assert!(isPublicClientNodeAddress(
        "2001:4860:4860::8888".parse().unwrap()
    ));
    for address in [
        "127.0.0.1",
        "192.168.1.8",
        "169.254.10.8",
        "192.0.2.8",
        "::1",
        "fd00::8",
        "fe80::8",
        "fec0::8",
        "2001:db8::8",
        "3fff::8",
        "::ffff:127.0.0.1",
        "::ffff:192.168.1.8",
        "::ffff:192.0.2.8",
        "::ffff:100.64.0.8",
    ] {
        assert!(!isPublicClientNodeAddress(address.parse().unwrap()));
    }
    assert!(isPublicClientNodeAddress(
        "::ffff:8.8.8.8".parse().expect("解析映射公网地址")
    ));
}

/// 固定部署环境值也必须经过同一公网判定，不能用显式配置绕过私网和保留地址边界。
#[test]
fn configuredClientNodeMustAlsoBePublic() {
    assert_eq!(
        parsePublicClientNodeAddress("8.8.8.8").expect("公网地址应被接受"),
        "8.8.8.8"
    );
    for address in ["192.168.1.8", "192.0.2.8", "fd00::8", "not-an-ip"] {
        assert_eq!(
            parsePublicClientNodeAddress(address),
            Err(ErrorCode::ClientNodeUnavailable)
        );
    }
}

/// 默认安装身份必须稳定落在 3 到 6 位小写英文包名段和非全大写软件名边界，避免重新引入固定品牌前缀。
#[test]
fn randomClientIdentityUsesShortEnglishWords() {
    for id in [Uuid::nil(), Uuid::new_v4(), Uuid::max()] {
        let applicationId = randomApplicationId(id);
        let segments = applicationId.split('.').collect::<Vec<_>>();
        assert_eq!(segments.len(), 3);
        assert!(segments.iter().all(|segment| {
            (3..=6).contains(&segment.len())
                && segment.bytes().all(|byte| byte.is_ascii_lowercase())
        }));
        let applicationName = randomApplicationName(id);
        assert!((3..=6).contains(&applicationName.len()));
        assert!(
            applicationName
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic())
        );
        assert!(
            !applicationName
                .bytes()
                .all(|byte| byte.is_ascii_uppercase())
        );
    }
}

/// 自定义身份只允许全空白表示随机；非空值的边界空白必须显式拒绝，不能由控制层静默裁剪。
#[test]
fn customClientIdentityRejectsBoundaryWhitespace() {
    let mut request = ClientPackageDownloadRequest {
        username: "fixture".to_owned(),
        password: "fixture-password".to_owned(),
        applicationId: Some(" abc.def.ghi ".to_owned()),
        applicationName: Some(" Abc ".to_owned()),
        iconBase64: None,
    };
    assert!(normalizeClientPackageCustomization(&mut request).is_err());

    request.applicationId = Some("   ".to_owned());
    request.applicationName = Some("\t".to_owned());
    assert!(normalizeClientPackageCustomization(&mut request).is_ok());
    assert!(request.applicationId.is_none());
    assert!(request.applicationName.is_none());
}

/// 未确认退出的打包器必须永久占用当前进程的生成门闩；已确认退出的失败才允许下一任务进入。
#[test]
fn unconfirmedPackagerExitKeepsOperationBlocked() {
    let confirmedState = Arc::new(AtomicBool::new(true));
    finishFailedOperation(
        ClientPackageOperationLease {
            operationActive: Arc::clone(&confirmedState),
            releaseOnDrop: true,
        },
        true,
    );
    assert!(!confirmedState.load(Ordering::Acquire));

    let unknownState = Arc::new(AtomicBool::new(true));
    finishFailedOperation(
        ClientPackageOperationLease {
            operationActive: Arc::clone(&unknownState),
            releaseOnDrop: true,
        },
        false,
    );
    assert!(unknownState.load(Ordering::Acquire));
}

/// Windows 文件锁解除后后台清理必须收敛，并在整个重试窗口持续占用单任务租约。
#[cfg(target_os = "windows")]
#[tokio::test]
async fn lockedTemporaryPackageRetriesBeforeReleasingLease() {
    let directory = TempDir::new().expect("创建锁文件测试目录");
    let manager = ClientPackageManager::load(directory.path()).expect("创建客户端包管理器");
    let packagePath = manager.packageDirectory.join("locked.apk");
    fs::write(&packagePath, b"credentialed-package")
        .await
        .expect("写入临时 APK");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&packagePath)
        .expect("独占打开临时 APK");
    let operationActive = Arc::new(AtomicBool::new(true));
    let temporaryPackage = TemporaryClientPackage::new(
        packagePath.clone(),
        manager,
        ClientPackageOperationLease {
            operationActive: Arc::clone(&operationActive),
            releaseOnDrop: true,
        },
    );
    let cleanupTask = tokio::spawn(temporaryPackage.remove());

    sleep(Duration::from_millis(100)).await;
    assert!(operationActive.load(Ordering::Acquire));
    assert!(packagePath.exists());
    drop(lock);
    cleanupTask.await.expect("后台清理任务正常结束");

    assert!(!packagePath.exists(), "文件锁解除后必须删除含凭据 APK");
    assert!(!operationActive.load(Ordering::Acquire));
}

/// 清理遍历遇到多个锁文件时必须继续删除其他文件，并把全部失败路径聚合到同一错误。
#[cfg(target_os = "windows")]
#[tokio::test]
async fn transientCleanupAggregatesLockedFilesAndContinuesTraversal() {
    let directory = TempDir::new().expect("创建聚合清理测试目录");
    let destination = directory.path().join("task.apk");
    let firstLocked = directory.path().join(".task.apk.raw.apk");
    let secondLocked = directory.path().join(".task.apk.signed.apk");
    let unrelated = directory.path().join("other-task.apk");
    for path in [&firstLocked, &secondLocked, &destination, &unrelated] {
        fs::write(path, b"apk").await.expect("写入清理候选 APK");
    }
    let firstLock = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&firstLocked)
        .expect("锁定第一个 APK");
    let secondLock = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&secondLocked)
        .expect("锁定第二个 APK");

    let error = removeTransientPackageFiles(&destination)
        .await
        .expect_err("锁文件必须产生聚合错误")
        .to_string();
    assert!(error.contains(".task.apk.raw.apk"));
    assert!(error.contains(".task.apk.signed.apk"));
    assert!(!destination.exists(), "清理不得因首个锁文件提前停止");
    assert!(unrelated.exists(), "本任务清理不得触碰其他任务 APK");

    drop(firstLock);
    drop(secondLock);
    removeTransientPackageFiles(&destination)
        .await
        .expect("解锁后清理全部残留");
}

/// 打包器超时后必须先终止并确认进程退出，随后 Windows 锁文件才能被完整清理。
#[cfg(target_os = "windows")]
#[tokio::test]
async fn timedOutPackagerIsReapedBeforeArtifactCleanup() {
    let directory = TempDir::new().expect("创建超时打包器测试目录");
    let destination = directory.path().join("timeout.apk");
    let lockedPackage = directory.path().join(".timeout.apk.raw.apk");
    let mut command = Command::new("powershell.exe");
    command
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(
            "$stream=[IO.File]::Open($env:PACKAGER_LOCK_FILE,[IO.FileMode]::Create,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None); Start-Sleep -Seconds 30",
        )
        .env("PACKAGER_LOCK_FILE", &lockedPackage)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let error = executeClientPackager(
        command,
        b"{}".to_vec().into(),
        // PowerShell 冷启动会受并行链接和杀毒扫描影响；超时窗口必须覆盖进程创建，避免尚未持锁就终止夹具。
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await
    .expect_err("长时间运行的打包器必须超时");
    assert!(error.processExited, "返回前必须确认超时进程已经退出");
    assert!(error.reason.contains("执行超时"));
    assert!(lockedPackage.exists(), "夹具必须实际创建并持有 APK");
    removeTransientPackageFiles(&destination)
        .await
        .expect("进程退出后必须能立即删除锁文件");
    assert!(!lockedPackage.exists());
}
