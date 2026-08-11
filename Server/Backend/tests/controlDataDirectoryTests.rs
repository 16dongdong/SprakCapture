#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    fs,
    sync::{Arc, Barrier},
    thread,
};

use tempfile::tempdir;

#[path = "../src/controlApi/dataDirectory.rs"]
mod dataDirectory;

/// 验证默认数据根严格位于代理服务可执行文件旁的 `data` 子目录。
///
/// 运行上下文：测试使用纯路径，不读取真实进程环境；失败表示安装到非系统盘时仍可能把配置
/// 写回用户目录。
#[test]
fn resolvesDataDirectoryBesideInstalledExecutable() {
    let installationRoot = tempdir().expect("应创建安装目录夹具");
    let executablePath = installationRoot.path().join("proxyService.exe");

    let resolved = dataDirectory::installationDataDirectory(&executablePath)
        .expect("绝对可执行文件路径应解析安装数据目录");

    assert_eq!(resolved, installationRoot.path().join("data"));
}

/// 验证旧版用户数据会整体迁入安装目录，而不是只复制主配置并遗失证书等受控资源。
///
/// 运行上下文：临时目录位于同一文件系统，覆盖生产优先采用的原子重命名路径；迁移失败时
/// 测试保留精确文件断言，避免后续重启生成一套与旧配置不一致的新数据。
#[test]
fn migratesCompleteLegacyDataDirectory() {
    let workspace = tempdir().expect("应创建迁移夹具根目录");
    let legacyDirectory = workspace.path().join("legacy");
    let installationDirectory = workspace.path().join("installed").join("data");
    fs::create_dir_all(legacyDirectory.join("certs")).expect("应创建旧版证书目录");
    fs::write(
        legacyDirectory.join("configuration.json"),
        br#"{"listenPort":1080}"#,
    )
    .expect("应写入旧版主配置");
    fs::write(
        legacyDirectory.join("certs").join("root.pem"),
        b"certificate",
    )
    .expect("应写入旧版证书夹具");

    dataDirectory::migrateLegacyDataDirectory(&legacyDirectory, &installationDirectory)
        .expect("完整旧版数据目录应迁入安装目录");

    assert!(!legacyDirectory.exists());
    assert_eq!(
        fs::read(installationDirectory.join("configuration.json")).expect("应读取迁移后的主配置"),
        br#"{"listenPort":1080}"#
    );
    assert_eq!(
        fs::read(installationDirectory.join("certs").join("root.pem")).expect("应读取迁移后的证书"),
        b"certificate"
    );
}

/// 验证未知的非空安装数据目录不会被旧配置迁移覆盖。
///
/// 运行上下文：安装失败或人工复制可能留下不完整目录；函数必须返回冲突并保留两侧文件，
/// 禁止通过兜底覆盖掩盖数据来源不明确的问题。
#[test]
fn rejectsNonEmptyInstallationDirectoryWithoutConfiguration() {
    let workspace = tempdir().expect("应创建冲突夹具根目录");
    let legacyDirectory = workspace.path().join("legacy");
    let installationDirectory = workspace.path().join("installed").join("data");
    fs::create_dir_all(&legacyDirectory).expect("应创建旧版目录");
    fs::create_dir_all(&installationDirectory).expect("应创建安装数据目录");
    fs::write(legacyDirectory.join("configuration.json"), b"{}").expect("应写入旧配置");
    fs::write(installationDirectory.join("unknown.bin"), b"occupied").expect("应写入冲突文件");

    let error = dataDirectory::migrateLegacyDataDirectory(&legacyDirectory, &installationDirectory)
        .expect_err("非空未知目录必须拒绝迁移");

    assert!(matches!(
        error,
        dataDirectory::DataDirectoryError::MigrationConflict { .. }
    ));
    assert!(legacyDirectory.join("configuration.json").is_file());
    assert!(installationDirectory.join("unknown.bin").is_file());
}

/// 验证跨卷迁移使用的复制阶段完整保留嵌套目录和文件正文。
///
/// 运行上下文：测试直接执行复制阶段，不依赖测试机是否提供第二块磁盘；失败表示跨卷安装时
/// 无法在原子提交前形成完整暂存目录。
#[test]
fn copiesCompleteDirectoryTreeForCrossVolumeMigration() {
    let workspace = tempdir().expect("应创建跨卷复制夹具根目录");
    let sourceDirectory = workspace.path().join("source");
    let targetDirectory = workspace.path().join("target");
    fs::create_dir_all(sourceDirectory.join("plugins").join("sample")).expect("应创建嵌套插件目录");
    fs::write(sourceDirectory.join("configuration.json"), b"configuration").expect("应写入源配置");
    fs::write(
        sourceDirectory
            .join("plugins")
            .join("sample")
            .join("plugin.json"),
        b"plugin",
    )
    .expect("应写入嵌套插件配置");

    dataDirectory::copyDirectoryTree(&sourceDirectory, &targetDirectory)
        .expect("跨卷复制阶段应完整生成暂存目录");

    assert_eq!(
        fs::read(targetDirectory.join("configuration.json")).expect("应读取复制后的主配置"),
        b"configuration"
    );
    assert_eq!(
        fs::read(
            targetDirectory
                .join("plugins")
                .join("sample")
                .join("plugin.json")
        )
        .expect("应读取复制后的插件配置"),
        b"plugin"
    );
}

/// 验证两个同时启动的守护实例只提交一份安装目录数据，后到实例复用权威结果。
///
/// 运行上下文：线程通过屏障同时进入迁移，模拟桌面守护重启和独立服务误启动的竞争；任一
/// 调用失败或最终配置不完整都表示启动竞态可能阻断正常升级。
#[test]
fn concurrentMigrationConvergesOnSingleInstallationDirectory() {
    let workspace = tempdir().expect("应创建并发迁移夹具根目录");
    let legacyDirectory = workspace.path().join("legacy");
    let installationDirectory = workspace.path().join("installed").join("data");
    fs::create_dir_all(&legacyDirectory).expect("应创建旧版目录");
    fs::write(legacyDirectory.join("configuration.json"), b"authoritative")
        .expect("应写入权威配置");
    let startBarrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let workerBarrier = Arc::clone(&startBarrier);
        let workerLegacy = legacyDirectory.clone();
        let workerInstallation = installationDirectory.clone();
        workers.push(thread::spawn(move || {
            workerBarrier.wait();
            dataDirectory::migrateLegacyDataDirectory(&workerLegacy, &workerInstallation)
        }));
    }

    startBarrier.wait();
    for worker in workers {
        worker
            .join()
            .expect("迁移线程不得 panic")
            .expect("并发迁移应收敛到同一权威目录");
    }
    assert_eq!(
        fs::read(installationDirectory.join("configuration.json"))
            .expect("应读取并发迁移后的权威配置"),
        b"authoritative"
    );
}
