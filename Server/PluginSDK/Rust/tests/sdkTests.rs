#![allow(non_snake_case)]

use std::{
    ffi::c_void,
    ptr, slice,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::json;
use sprak_plugin_sdk::{
    Action, ActionKind, BinaryEvent, Invocation, LengthPrefixedFrames, Plugin, joinPackets,
    nativeAbi::{
        NativeByteSlice, NativeExtensionBuffer, NativeExtensionExports, NativeExtensionInitRequest,
        initializePlugin,
    },
    splitPackets,
};

/// 构造与宿主序列化格式一致的 TCP chunk，供动作与二进制帮助器契约测试复用。
fn invocation(bytes: &[u8]) -> Invocation {
    serde_json::from_value(json!({
        "pluginId": "example.rust-native",
        "moduleId": "rewrite",
        "moduleKind": "streamTransformer",
        "envelope": {
            "apiVersion": "2.0.0",
            "eventId": "event-rust-1",
            "stage": "tcpChunk",
            "serviceGeneration": 1,
            "recordingGeneration": 1,
            "pluginInstanceId": "example.rust-native@1.0.0#1",
            "connectionId": "connection-1",
            "transactionId": null,
            "deadlineUnixMs": 4102444800000_u64,
            "context": { "transport": "tcp", "interceptionMode": "intercept" },
            "payload": { "bytes": bytes, "endOfStream": false }
        }
    }))
    .expect("解析测试调用")
}

/// 验证作者只用普通闭包即可读取并替换 TCP 二进制输出。
#[test]
fn ordinaryClosureBuildsBinaryModification() {
    let plugin = Plugin::new(|request| {
        let binary =
            BinaryEvent::fromInvocation(request).map_err(sprak_plugin_sdk::PluginError::new)?;
        binary
            .modify(request, |mut bytes| {
                bytes.make_ascii_uppercase();
                bytes
            })
            .map_err(sprak_plugin_sdk::PluginError::new)
    });
    let _ = plugin;
    let request = invocation(b"hello");
    let action = BinaryEvent::fromInvocation(&request)
        .expect("读取二进制负载")
        .modify(&request, |mut bytes| {
            bytes.make_ascii_uppercase();
            bytes
        })
        .expect("构造二进制修改动作");
    assert_eq!(
        action,
        Action::modifyBytes(&request, b"HELLO".to_vec()).expect("构造预期动作")
    );
}

/// 验证全部稳定宿主动作都绑定当前事件，并保持输出结构与输入校验一致。
#[test]
fn actionConstructorsMatchHostContract() {
    let request = invocation(b"hello");
    assert_eq!(Action::continueEvent(&request).action, ActionKind::Continue);
    assert_eq!(Action::hold(&request).action, ActionKind::Hold);
    assert_eq!(Action::dropEvent(&request).action, ActionKind::Drop);

    let modified = Action::modifyBytes(&request, b"world".to_vec()).expect("构造二进制修改动作");
    assert_eq!(modified.action, ActionKind::Modify);
    assert_eq!(modified.patch[0]["path"], "");
    assert_eq!(
        modified.patch[0]["value"]["bytes"],
        json!([119, 111, 114, 108, 100])
    );
    assert_eq!(modified.patch[0]["value"]["endOfStream"], false);

    let rejected = Action::reject(&request, "不允许").expect("构造拒绝动作");
    assert_eq!(rejected.output, Some(json!({ "reason": "不允许" })));
    let closed = Action::close(&request, "连接结束").expect("构造关闭动作");
    assert_eq!(closed.output, Some(json!({ "reason": "连接结束" })));
    assert!(Action::reject(&request, "  ").is_err());

    let annotated = Action::annotate(&request, vec![json!({ "tag": "检查" })]);
    assert_eq!(annotated.annotations, vec![json!({ "tag": "检查" })]);
    let redirected = Action::redirect(&request, "upstream.local", 8443).expect("构造重定向动作");
    assert_eq!(
        redirected.output,
        Some(json!({ "host": "upstream.local", "port": 8443 }))
    );
    assert!(Action::redirect(&request, "", 0).is_err());
    let responded = Action::respond(&request, json!({ "statusCode": 204 }));
    assert_eq!(responded.output, Some(json!({ "statusCode": 204 })));
    let payload = Action::modifyPayload(&request, json!({ "value": 7 }));
    assert_eq!(
        payload.patch[0],
        json!({ "op": "replace", "path": "", "value": { "value": 7 } })
    );
}

/// 验证分包、合包和跨 chunk 长度前缀解析均保持字节顺序且能重封包。
#[test]
fn packetHelpersPreserveCompletePayload() {
    let packets = splitPackets(b"abcdefgh", 3).expect("切分负载");
    assert_eq!(packets, [b"abc".to_vec(), b"def".to_vec(), b"gh".to_vec()]);
    assert_eq!(joinPackets(&packets).expect("合并负载"), b"abcdefgh");

    let mut framer = LengthPrefixedFrames::new(32).expect("创建分帧器");
    let encoded = framer.encode(b"hello").expect("重封包");
    assert!(framer.push(&encoded[..2]).expect("输入半前缀").is_empty());
    assert_eq!(framer.push(&encoded[2..]).expect("输入剩余帧"), [b"hello"]);
    assert_eq!(framer.bufferedBytes(), 0);
}

/// 验证生命周期闭包可以安全共享作者状态；SDK 的 ABI 层负责至多一次执行。
#[test]
fn lifecycleClosuresAcceptSharedState() {
    let stopped = Arc::new(AtomicUsize::new(0));
    let destroyed = Arc::new(AtomicUsize::new(0));
    let stopCounter = Arc::clone(&stopped);
    let destroyCounter = Arc::clone(&destroyed);
    let plugin = Plugin::builder(|request| Ok(Action::continueEvent(request)))
        .onStop(move || {
            stopCounter.fetch_add(1, Ordering::AcqRel);
        })
        .onDestroy(move || {
            destroyCounter.fetch_add(1, Ordering::AcqRel);
        })
        .build();
    drop(plugin);
    assert_eq!(stopped.load(Ordering::Acquire), 0);
    assert_eq!(destroyed.load(Ordering::Acquire), 0);
}

/// 验证真实 ABI v2 初始化、invoke、输出释放、重复 stop 与 destroy 的完整所有权顺序。
#[test]
fn nativeAbiOwnsOutputAndLifecycleExactlyOnce() {
    let stopped = Arc::new(AtomicUsize::new(0));
    let destroyed = Arc::new(AtomicUsize::new(0));
    let stopCounter = Arc::clone(&stopped);
    let destroyCounter = Arc::clone(&destroyed);
    let manifest = br#"{"id":"example.rust-native"}"#;
    let configuration = br#"{"enabled":true}"#;
    let request = NativeExtensionInitRequest {
        apiVersion: 2,
        manifest: nativeSlice(manifest),
        configuration: nativeSlice(configuration),
    };
    let mut exports = NativeExtensionExports {
        apiVersion: 0,
        pluginContext: ptr::null_mut(),
        invoke: None,
        stop: None,
        destroy: None,
    };
    let status = unsafe {
        initializePlugin(
            move |context| {
                assert_eq!(context.configuration["enabled"], true);
                Ok(
                    Plugin::builder(|request| Ok(Action::continueEvent(request)))
                        .onStop(move || {
                            stopCounter.fetch_add(1, Ordering::AcqRel);
                        })
                        .onDestroy(move || {
                            destroyCounter.fetch_add(1, Ordering::AcqRel);
                        })
                        .build(),
                )
            },
            &request,
            &mut exports,
        )
    };
    assert_eq!(status, 0);
    assert_eq!(exports.apiVersion, 2);

    let invocationBytes = serde_json::to_vec(&invocation(b"hello")).expect("序列化 ABI 调用");
    let mut output = NativeExtensionBuffer {
        pointer: ptr::null(),
        length: 0,
        releaseContext: ptr::null_mut::<c_void>(),
        release: None,
    };
    let invoke = exports.invoke.expect("导出 invoke");
    assert_eq!(
        unsafe {
            invoke(
                exports.pluginContext,
                nativeSlice(&invocationBytes),
                &mut output,
            )
        },
        0
    );
    let actionBytes = unsafe { slice::from_raw_parts(output.pointer, output.length) }.to_vec();
    let action: Action = serde_json::from_slice(&actionBytes).expect("解析 ABI 动作");
    assert_eq!(action, Action::continueEvent(&invocation(b"hello")));
    unsafe {
        output.release.expect("导出释放回调")(
            output.releaseContext,
            output.pointer,
            output.length,
        );
        exports.stop.expect("导出 stop")(exports.pluginContext);
        exports.stop.expect("导出 stop")(exports.pluginContext);
        exports.destroy.expect("导出 destroy")(exports.pluginContext);
    }
    assert_eq!(stopped.load(Ordering::Acquire), 1);
    assert_eq!(destroyed.load(Ordering::Acquire), 1);
}

/// 验证作者停止与销毁回调 panic 被限制在 ABI 边界内，并且每个生命周期仍只进入一次。
#[test]
fn nativeAbiContainsLifecyclePanics() {
    let stopped = Arc::new(AtomicUsize::new(0));
    let destroyed = Arc::new(AtomicUsize::new(0));
    let stopCounter = Arc::clone(&stopped);
    let destroyCounter = Arc::clone(&destroyed);
    let request = NativeExtensionInitRequest {
        apiVersion: 2,
        manifest: nativeSlice(br#"{"id":"panic-fixture"}"#),
        configuration: nativeSlice(b"{}"),
    };
    let mut exports = NativeExtensionExports {
        apiVersion: 0,
        pluginContext: ptr::null_mut(),
        invoke: None,
        stop: None,
        destroy: None,
    };
    let status = unsafe {
        initializePlugin(
            move |_| {
                Ok(
                    Plugin::builder(|request| Ok(Action::continueEvent(request)))
                        .onStop(move || {
                            stopCounter.fetch_add(1, Ordering::AcqRel);
                            panic!("停止夹具 panic");
                        })
                        .onDestroy(move || {
                            destroyCounter.fetch_add(1, Ordering::AcqRel);
                            panic!("销毁夹具 panic");
                        })
                        .build(),
                )
            },
            &request,
            &mut exports,
        )
    };
    assert_eq!(status, 0);
    unsafe {
        exports.stop.expect("导出 stop")(exports.pluginContext);
        exports.stop.expect("导出 stop")(exports.pluginContext);
        exports.destroy.expect("导出 destroy")(exports.pluginContext);
    }
    assert_eq!(stopped.load(Ordering::Acquire), 1);
    assert_eq!(destroyed.load(Ordering::Acquire), 1);
}

/// 把测试字节映射为仅在当前 ABI 调用有效的借用切片。
fn nativeSlice(bytes: &[u8]) -> NativeByteSlice {
    NativeByteSlice {
        pointer: bytes.as_ptr(),
        length: bytes.len(),
    }
}
