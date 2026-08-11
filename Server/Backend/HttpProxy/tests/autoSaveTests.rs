#![allow(non_snake_case)]

use capture_core::{RecordingConfiguration, RecordingSession};
use http_proxy_core::tools::{AutoSaveConfiguration, AutoSaveFormat, AutoSaveTool};

/// 创建仅供自动保存测试使用的隔离录制会话，spill 正文和归档目录均由临时目录在测试结束时删除。
async fn createRecording(directory: &tempfile::TempDir) -> RecordingSession {
    RecordingSession::new(RecordingConfiguration {
        spillDirectory: directory.path().join("capture"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("测试录制会话必须创建成功")
}

/// 验证手动自动保存写出原生归档并在连续保存时按 maxFiles 轮转，不依赖仓库内构建或输出目录。
#[tokio::test]
async fn autoSaveWritesNativeArchiveAndRotates() {
    let directory = tempfile::tempdir().expect("自动保存临时目录必须创建");
    let recording = createRecording(&directory).await;
    let archiveDirectory = directory.path().join("archives");
    let autoSave = AutoSaveTool::new(
        AutoSaveConfiguration {
            enabled: true,
            directory: archiveDirectory.to_string_lossy().into_owned(),
            intervalSeconds: 0,
            everyNTransactions: 1,
            format: AutoSaveFormat::Native,
            maxFiles: 1,
            includeBodies: false,
        },
        recording.clone(),
    )
    .expect("自动保存配置必须有效");

    let first = autoSave
        .saveNow(&recording)
        .await
        .expect("首次手动保存必须成功");
    let firstPath = first.lastSavedPath.expect("首次保存必须返回路径");
    let firstArchive = tokio::fs::read(&firstPath)
        .await
        .expect("首次归档必须可读取");
    let firstDocument: serde_json::Value =
        serde_json::from_slice(&firstArchive).expect("原生归档必须是 JSON");
    assert_eq!(
        firstDocument["format"],
        serde_json::Value::String("capture-recording-v1".to_owned())
    );

    let second = autoSave
        .saveNow(&recording)
        .await
        .expect("第二次手动保存必须成功");
    let secondPath = second.lastSavedPath.expect("第二次保存必须返回路径");
    assert_ne!(firstPath, secondPath, "连续保存必须生成独立归档文件名");
    let archives = std::fs::read_dir(&archiveDirectory)
        .expect("归档目录必须可读")
        .collect::<Result<Vec<_>, _>>()
        .expect("归档目录项必须可枚举");
    assert_eq!(archives.len(), 1, "轮转必须只保留 maxFiles 个归档");
    assert!(std::path::Path::new(&secondPath).exists());
}
