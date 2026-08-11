#![allow(non_snake_case)]

use std::{env, fs, path::Path, process::Command, sync::Arc, time::Duration};

use plugin_host::{
    EngineRequirements, EventEnvelope, ExtensionConfigurationStore, ExtensionLimits,
    ExtensionManifest, ExtensionModule, ExtensionRuntime, ExtensionRuntimeKind,
    ExtensionRuntimeManifest, FailurePolicy, InterceptionMode, ModuleKind, PluginHost,
    PluginHostError, PluginUserConfiguration, ProcessExtensionRuntime, RuntimeInvocation, Stage,
    StageContext, StageSubscription,
};
use serde_json::json;
use tempfile::tempdir;

const WORKER_SOURCE: &str = r###"
use std::io::{self, BufRead, Write};

fn number_after(line: &str, marker: &str) -> Option<u64> {
    let start = line.find(marker)? + marker.len();
    let length = line[start..].find(|character: char| !character.is_ascii_digit()).unwrap_or(line.len() - start);
    line[start..start + length].parse().ok()
}

fn string_after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let start = line.find(marker)? + marker.len();
    let length = line[start..].find('"')?;
    Some(&line[start..start + length])
}

fn main() {
    let input = io::stdin();
    let mut lines = input.lock().lines();
    let Some(Ok(initialize)) = lines.next() else { return; };
    if !initialize.contains("\"type\":\"initialize\"") || !initialize.contains("\"apiVersion\":2") { return; }
    println!("{{\"type\":\"ready\",\"apiVersion\":2}}");
    io::stdout().flush().unwrap();
    for line in lines.map_while(Result::ok) {
        if line.contains("\"type\":\"stop\"") { return; }
        if !line.contains("\"deadlineUnixMs\":9007199254740991") { return; }
        let Some(request_id) = number_after(&line, "\"requestId\":") else { return; };
        let Some(event_id) = string_after(&line, "\"eventId\":\"") else { return; };
        if event_id == "event-invalid-protocol" {
            println!("{{\"type\":\"ready\",\"apiVersion\":2}}");
            io::stdout().flush().unwrap();
            continue;
        }
        println!("{{\"type\":\"result\",\"requestId\":{request_id},\"action\":{{\"eventId\":\"{event_id}\",\"action\":\"modify\",\"patch\":[],\"annotations\":[{{\"worker\":true}}],\"output\":{{\"bytes\":[4,5,6]}}}}}}");
        io::stdout().flush().unwrap();
    }
}
"###;

/// 编译不依赖外部 crate 的 JSONL worker；测试产物完全位于系统临时目录。
fn buildWorker(outputPath: &Path) {
    let sourcePath = outputPath.with_extension("rs");
    fs::write(&sourcePath, WORKER_SOURCE).expect("写入进程插件夹具");
    let output = Command::new(env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned()))
        .args([
            "--edition",
            "2024",
            sourcePath.to_str().expect("夹具路径 UTF-8"),
            "-o",
            outputPath.to_str().expect("worker 路径 UTF-8"),
        ])
        .output()
        .expect("启动 rustc");
    assert!(
        output.status.success(),
        "编译进程插件失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// 构造 Native Worker 清单；真实运行时通过同一清单加载任意语言生成的可执行文件。
fn manifest(entry: String) -> ExtensionManifest {
    ExtensionManifest {
        manifestVersion: 2,
        id: "example.process-worker".to_owned(),
        name: "进程插件".to_owned(),
        description: "验证 JSONL SDK 生产运行时".to_owned(),
        version: "1.0.0".to_owned(),
        publisher: "tests".to_owned(),
        engines: EngineRequirements {
            host: ">=1.0.0".to_owned(),
            api: "2.x".to_owned(),
        },
        runtime: ExtensionRuntimeManifest {
            kind: ExtensionRuntimeKind::NativeWorker,
            entry,
            protocolVersion: Some("2.0".to_owned()),
            arguments: Vec::new(),
        },
        modules: vec![ExtensionModule {
            id: "transform".to_owned(),
            kind: ModuleKind::StreamTransformer,
            subscriptions: vec![StageSubscription {
                stage: Stage::TcpChunk,
                order: 0,
                matchRule: Default::default(),
            }],
            contributes: Vec::new(),
        }],
        capabilities: Vec::new(),
        dependencies: Default::default(),
        limits: ExtensionLimits::default(),
        failurePolicy: FailurePolicy::FailClosed,
        configurationSchema: None,
        contributes: None,
        extensions: Default::default(),
    }
}

/// 构造一个真实 TCP 阶段调用；固定事件 ID 用于验证 SDK 不要求作者手写响应关联逻辑。
fn invocation(eventId: &str) -> RuntimeInvocation {
    RuntimeInvocation {
        pluginId: "example.process-worker".to_owned(),
        moduleId: "transform".to_owned(),
        moduleKind: ModuleKind::StreamTransformer,
        envelope: EventEnvelope {
            apiVersion: "2.0".to_owned(),
            eventId: eventId.to_owned(),
            stage: Stage::TcpChunk,
            serviceGeneration: 1,
            recordingGeneration: 1,
            pluginInstanceId: "example.process-worker@1.0.0#1".to_owned(),
            connectionId: Some("connection-worker".to_owned()),
            transactionId: None,
            deadlineUnixMs: u64::MAX,
            context: StageContext {
                interceptionMode: InterceptionMode::Intercept,
                ..Default::default()
            },
            payload: json!({ "bytes": [1, 2, 3] }),
        },
    }
}

/// 验证生产进程运行时完成初始化、无损数字边界、普通函数式调用和作者控制的停止生命周期。
#[tokio::test]
async fn invokesNativeWorkerThroughJsonLineProtocol() {
    let directory = tempdir().expect("创建进程插件目录");
    let workerName = format!("worker{}", env::consts::EXE_SUFFIX);
    buildWorker(&directory.path().join(&workerName));
    let mut impreciseManifest = manifest(workerName.clone());
    impreciseManifest.limits.maxStorageBytes = 9_007_199_254_740_992;
    assert!(matches!(
        ProcessExtensionRuntime::load(&impreciseManifest, directory.path(), &json!({})),
        Err(PluginHostError::Initialization)
    ));
    assert!(matches!(
        ProcessExtensionRuntime::load(
            &manifest(workerName.clone()),
            directory.path(),
            &json!({ "nested": { "unsafeInteger": 9_007_199_254_740_992_u64 } }),
        ),
        Err(PluginHostError::Initialization)
    ));
    let runtime = Arc::new(
        ProcessExtensionRuntime::load(
            &manifest(workerName),
            directory.path(),
            &json!({ "authorOption": true }),
        )
        .expect("加载进程插件"),
    );

    let (first, second) = tokio::join!(
        runtime.invoke(invocation("event-worker-1")),
        runtime.invoke(invocation("event-worker-2"))
    );
    assert_eq!(
        first.expect("首次调用").output,
        Some(json!({ "bytes": [4, 5, 6] }))
    );
    assert_eq!(
        second.expect("并发调用").output,
        Some(json!({ "bytes": [4, 5, 6] }))
    );
    let mut impreciseInvocation = invocation("event-imprecise-generation");
    impreciseInvocation.envelope.serviceGeneration = 9_007_199_254_740_992;
    assert_eq!(
        runtime
            .invoke(impreciseInvocation)
            .await
            .expect_err("JSONL 不得静默舍入代际")
            .to_string(),
        "extensionProcessIntegerOutOfRange"
    );
    assert_eq!(
        runtime
            .invoke(invocation("event-invalid-protocol"))
            .await
            .expect_err("重复 ready 必须终止响应协议"),
        "extensionProcessProtocolInvalid"
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            runtime.invoke(invocation("event-after-protocol-failure")),
        )
        .await
        .expect("协议失败后的调用必须立即返回")
        .expect_err("协议失败后的调用不能再次写入 worker"),
        "extensionProcessProtocolFailed"
    );
    runtime.stop();
    assert!(
        runtime
            .invoke(invocation("event-after-stop"))
            .await
            .is_err()
    );
}

/// 验证真实 PluginHost 启动时会恢复已启用 Native Worker，而不是只支持直接构造测试运行时。
#[tokio::test]
async fn restoresEnabledNativeWorkerThroughProductionManager() {
    let rootDirectory = tempdir().expect("创建插件根目录");
    let pluginDirectory = rootDirectory.path().join("example.process-worker");
    fs::create_dir(&pluginDirectory).expect("创建插件目录");
    let workerName = format!("worker{}", env::consts::EXE_SUFFIX);
    buildWorker(&pluginDirectory.join(&workerName));
    fs::write(
        pluginDirectory.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest(workerName)).expect("序列化 worker manifest"),
    )
    .expect("写入 worker manifest");
    let configuration =
        ExtensionConfigurationStore::open(rootDirectory.path()).expect("创建扩展配置存储");
    configuration
        .updatePlugin(
            "example.process-worker",
            PluginUserConfiguration {
                enabled: true,
                activeVersion: Some("1.0.0".to_owned()),
                failurePolicy: FailurePolicy::FailClosed,
                automaticRestart: true,
                ..Default::default()
            },
        )
        .expect("启用 worker 配置");

    let host = PluginHost::new(rootDirectory.path()).expect("创建生产插件宿主");
    let snapshots = host.extensionManager().snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].enabled);
    assert!(snapshots[0].running);
    let kernel = host.extensionKernel();
    kernel.setServiceGeneration(1);
    kernel.setRecordingGeneration(1);
    let result = kernel
        .dispatch(invocation("event-manager").envelope)
        .await
        .expect("生产内核调用 worker");
    assert_eq!(result.finalPayload, json!({ "bytes": [4, 5, 6] }));
}
