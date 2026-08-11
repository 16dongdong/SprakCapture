#![allow(non_snake_case)]

use std::{env, fs, path::Path, process::Command, sync::Arc};

use plugin_host::{
    ConnectionMetadata, DataPlaneActionResult, EngineRequirements, EventEnvelope,
    ExtensionConfigurationStore, ExtensionLimits, ExtensionManifest, ExtensionModule,
    ExtensionRuntime, ExtensionRuntimeKind, ExtensionRuntimeManifest, FailurePolicy,
    InterceptionMode, ModuleKind, NativeExtensionRuntime, PluginHost, PluginUserConfiguration,
    RuntimeInvocation, Stage, StageContext, StageSubscription, StreamDirection, TransportKind,
};
use serde_json::json;
use tempfile::tempdir;

const NATIVE_EXTENSION_SOURCE: &str = r###"
use std::{ffi::c_void, ptr, slice, sync::atomic::{AtomicBool, Ordering}};

#[repr(C)]
struct ByteSlice { pointer: *const u8, length: usize }

#[repr(C)]
struct InitRequest { api_version: u32, manifest: ByteSlice, configuration: ByteSlice }

#[repr(C)]
struct OutputBuffer {
    pointer: *const u8,
    length: usize,
    release_context: *mut c_void,
    release: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize)>,
}

#[repr(C)]
struct Exports {
    api_version: u32,
    plugin_context: *mut c_void,
    invoke: Option<unsafe extern "C" fn(*mut c_void, ByteSlice, *mut OutputBuffer) -> i32>,
    stop: Option<unsafe extern "C" fn(*mut c_void)>,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}

unsafe extern "C" fn release_output(_: *mut c_void, pointer: *const u8, length: usize) {
    if !pointer.is_null() {
        let slice = ptr::slice_from_raw_parts_mut(pointer.cast_mut(), length);
        drop(unsafe { Box::from_raw(slice) });
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    request: ByteSlice,
    output: *mut OutputBuffer,
) -> i32 {
    if context.is_null() || output.is_null() || request.pointer.is_null() {
        return -1;
    }
    if unsafe { &*context.cast::<AtomicBool>() }.load(Ordering::Acquire) {
        return -2;
    }
    let request = unsafe { slice::from_raw_parts(request.pointer, request.length) };
    let marker = b"\"eventId\":\"";
    let Some(marker_offset) = request.windows(marker.len()).position(|part| part == marker) else {
        return -3;
    };
    let event_start = marker_offset + marker.len();
    let Some(event_length) = request[event_start..].iter().position(|byte| *byte == b'"') else {
        return -4;
    };
    let event_id = match std::str::from_utf8(&request[event_start..event_start + event_length]) {
        Ok(event_id) => event_id,
        Err(_) => return -5,
    };
    let bytes = format!(r#"{{"eventId":"{event_id}","action":"modify","patch":[],"annotations":[{{"native":true}}],"output":{{"bytes":[9,8,7]}}}}"#).into_bytes().into_boxed_slice();
    let length = bytes.len();
    let pointer = Box::into_raw(bytes).cast::<u8>();
    unsafe {
        *output = OutputBuffer {
            pointer,
            length,
            release_context: ptr::null_mut(),
            release: Some(release_output),
        };
    }
    0
}

unsafe extern "C" fn stop(context: *mut c_void) {
    if !context.is_null() {
        unsafe { &*context.cast::<AtomicBool>() }.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    if !context.is_null() {
        drop(unsafe { Box::from_raw(context.cast::<AtomicBool>()) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn capture_extension_init(
    request: *const InitRequest,
    exports: *mut Exports,
) -> i32 {
    if request.is_null() || exports.is_null() {
        return -1;
    }
    let request = unsafe { &*request };
    if request.api_version != 2 || request.manifest.length == 0 || request.configuration.length == 0 {
        return -2;
    }
    unsafe {
        *exports = Exports {
            api_version: 2,
            plugin_context: Box::into_raw(Box::new(AtomicBool::new(false))).cast(),
            invoke: Some(invoke),
            stop: Some(stop),
            destroy: Some(destroy),
        };
    }
    0
}
"###;

/// 编译进程内 Native Mod 夹具；输出完全位于测试临时目录并随目录析构删除。
fn buildNativeExtension(outputPath: &Path) {
    let sourcePath = outputPath.with_extension("rs");
    fs::write(&sourcePath, NATIVE_EXTENSION_SOURCE).expect("写入 Native Mod 夹具");
    let result = Command::new(env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned()))
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "cdylib",
            sourcePath.to_str().expect("夹具源码路径 UTF-8"),
            "-o",
            outputPath.to_str().expect("夹具库路径 UTF-8"),
        ])
        .output()
        .expect("启动 rustc");
    assert!(
        result.status.success(),
        "编译 Native Mod 失败：{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

/// 构造 Native Mod 清单；入口名称由当前平台动态库后缀决定。
fn manifest(entry: String) -> ExtensionManifest {
    ExtensionManifest {
        manifestVersion: 2,
        id: "example.native-mod".to_owned(),
        name: "Native Mod".to_owned(),
        description: "验证开放进程内阶段 ABI".to_owned(),
        version: "1.0.0".to_owned(),
        publisher: "tests".to_owned(),
        engines: EngineRequirements {
            host: ">=1.0.0".to_owned(),
            api: "2.x".to_owned(),
        },
        runtime: ExtensionRuntimeManifest {
            kind: ExtensionRuntimeKind::Native,
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

/// 构造一次真实 Native 阶段调用；固定事件 ID 供无依赖动态库夹具核验输入。
fn invocation() -> RuntimeInvocation {
    RuntimeInvocation {
        pluginId: "example.native-mod".to_owned(),
        moduleId: "transform".to_owned(),
        moduleKind: ModuleKind::StreamTransformer,
        envelope: EventEnvelope {
            apiVersion: "2.0".to_owned(),
            eventId: "event-native".to_owned(),
            stage: Stage::TcpChunk,
            serviceGeneration: 1,
            recordingGeneration: 1,
            pluginInstanceId: "example.native-mod@1.0.0#1".to_owned(),
            connectionId: Some("connection-native".to_owned()),
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

/// 验证进程内 Native Mod 可读取完整阶段事件、返回任意动作并响应热停止。
#[tokio::test]
async fn loadsInvokesAndStopsUnrestrictedNativeExtension() {
    let directory = tempdir().expect("创建 Native Mod 临时目录");
    let libraryName = format!("nativeExtension{}", env::consts::DLL_SUFFIX);
    buildNativeExtension(&directory.path().join(&libraryName));
    let runtime = Arc::new(
        NativeExtensionRuntime::load(
            &manifest(libraryName),
            directory.path(),
            &json!({ "authorControlsEverything": true }),
        )
        .expect("加载 Native Mod"),
    );

    let action = runtime.invoke(invocation()).await.expect("调用 Native Mod");
    assert_eq!(action.output, Some(json!({ "bytes": [9, 8, 7] })));
    assert_eq!(action.annotations, vec![json!({ "native": true })]);

    runtime.stop();
    assert!(runtime.invoke(invocation()).await.is_err());
}

/// 验证宿主启动会发现完整 manifest、跳过 legacy 误报，并按持久配置直接发布 Native Mod。
#[tokio::test]
async fn discoversAndRestoresEnabledNativeModThroughProductionManager() {
    let rootDirectory = tempdir().expect("创建插件根目录");
    let pluginDirectory = rootDirectory.path().join("example.native-mod");
    fs::create_dir_all(&pluginDirectory).expect("创建 Native Mod 目录");
    let libraryName = format!("nativeExtension{}", env::consts::DLL_SUFFIX);
    buildNativeExtension(&pluginDirectory.join(&libraryName));
    fs::write(
        pluginDirectory.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest(libraryName)).expect("序列化完整 manifest"),
    )
    .expect("写入完整 manifest");
    let configurationStore =
        ExtensionConfigurationStore::open(rootDirectory.path()).expect("创建扩展配置存储");
    configurationStore
        .updatePlugin(
            "example.native-mod",
            PluginUserConfiguration {
                enabled: true,
                configuration: json!({ "mode": "unrestricted" }),
                ..PluginUserConfiguration::default()
            },
        )
        .expect("持久化启用配置");
    drop(configurationStore);

    let host = PluginHost::new(rootDirectory.path()).expect("启动完整插件宿主");
    assert!(
        host.snapshots().is_empty(),
        "完整 manifest 不应进入 legacy 列表"
    );
    let packages = host.extensionManager().snapshots();
    assert_eq!(packages.len(), 1);
    assert!(packages[0].enabled && packages[0].running);
    assert_eq!(packages[0].errorCode, None);

    let kernel = host.extensionKernel();
    kernel.setServiceGeneration(1);
    kernel.setRecordingGeneration(1);
    let connection = host.openConnection(ConnectionMetadata {
        transport: TransportKind::Tcp,
        clientAddress: "127.0.0.1:32000".to_owned(),
        targetHost: "example.test".to_owned(),
        targetPort: 443,
    });
    let result = host
        .processDataPlaneBytes(&connection, StreamDirection::ClientToServer, vec![1, 2, 3])
        .await;
    assert_eq!(
        result,
        DataPlaneActionResult::Forward {
            bytes: vec![9, 8, 7]
        }
    );
    host.closeConnection(connection);
}
