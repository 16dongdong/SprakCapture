#![allow(non_snake_case)]

use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use plugin_host::{
    ActionKind, DispatchFailure, EngineRequirements, EventEnvelope, ExtensionAction,
    ExtensionKernel, ExtensionLimits, ExtensionManifest, ExtensionMatch, ExtensionModule,
    ExtensionRuntime, ExtensionRuntimeKind, ExtensionRuntimeManifest, FailurePolicy,
    InterceptionMode, ModuleKind, PluginExecutionOptions, RuntimeInvocation, Stage, StageContext,
    StageSubscription,
};
use serde_json::json;

/// 提供按调用顺序返回结果的确定性运行时；测试通过它验证内核排序、修改链和失败语义。
struct ScriptedRuntime {
    actions: Mutex<VecDeque<Result<ExtensionAction, String>>>,
    invocations: Mutex<Vec<RuntimeInvocation>>,
    stopped: AtomicBool,
}

impl ScriptedRuntime {
    /// 创建包含固定动作序列的模拟运行时；动作耗尽后返回精确失败而不是伪造 Continue。
    fn new(actions: Vec<Result<ExtensionAction, String>>) -> Self {
        Self {
            actions: Mutex::new(actions.into()),
            invocations: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
        }
    }
}

impl ExtensionRuntime for ScriptedRuntime {
    /// 记录完整调用并异步返回下一动作；测试运行时不执行第三方代码和网络 I/O。
    fn invoke<'a>(
        &'a self,
        invocation: RuntimeInvocation,
    ) -> Pin<Box<dyn Future<Output = Result<ExtensionAction, String>> + Send + 'a>> {
        self.invocations.lock().push(invocation);
        let result = self
            .actions
            .lock()
            .pop_front()
            .unwrap_or_else(|| Err("动作序列已耗尽".to_owned()));
        Box::pin(async move { result })
    }

    /// 标记运行时已停止；热替换和卸载测试据此确认旧实例被回收。
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
}

/// 构造只订阅 TCP 数据阶段的完整清单；参数用于稳定控制插件和模块执行顺序。
fn manifest(pluginId: &str, moduleId: &str, order: i32) -> ExtensionManifest {
    ExtensionManifest {
        manifestVersion: 2,
        id: pluginId.to_owned(),
        name: pluginId.to_owned(),
        description: "内核测试插件".to_owned(),
        version: "1.0.0".to_owned(),
        publisher: "tests".to_owned(),
        engines: EngineRequirements {
            host: ">=1.0.0".to_owned(),
            api: "2.x".to_owned(),
        },
        runtime: ExtensionRuntimeManifest {
            kind: ExtensionRuntimeKind::Wasm,
            entry: "dist/plugin.wasm".to_owned(),
            protocolVersion: Some("2.0".to_owned()),
            arguments: Vec::new(),
        },
        modules: vec![ExtensionModule {
            id: moduleId.to_owned(),
            kind: ModuleKind::StreamTransformer,
            subscriptions: vec![StageSubscription {
                stage: Stage::TcpChunk,
                order,
                matchRule: ExtensionMatch {
                    hosts: vec!["*.example.com".to_owned()],
                    ports: vec![443],
                    transports: vec!["tcp".to_owned()],
                    ..ExtensionMatch::default()
                },
            }],
            contributes: Vec::new(),
        }],
        capabilities: vec![
            "traffic.observe".to_owned(),
            "traffic.modify".to_owned(),
            "capture.annotate".to_owned(),
        ],
        dependencies: BTreeMap::new(),
        limits: ExtensionLimits::default(),
        failurePolicy: FailurePolicy::FailClosed,
        configurationSchema: None,
        contributes: None,
        extensions: BTreeMap::new(),
    }
}

/// 构造测试所需的执行覆盖；开放模式不包含能力授权字段。
fn options(failurePolicy: FailurePolicy) -> PluginExecutionOptions {
    PluginExecutionOptions {
        moduleOrder: Vec::new(),
        subscriptionOverrides: BTreeMap::new(),
        failurePolicy,
        limits: None,
    }
}

/// 构造处于当前代际的 TCP 阶段事件；每个测试使用独立事件 ID 防止动作误关联。
fn envelope(eventId: &str, payload: serde_json::Value) -> EventEnvelope {
    EventEnvelope {
        apiVersion: "2.0".to_owned(),
        eventId: eventId.to_owned(),
        stage: Stage::TcpChunk,
        serviceGeneration: 7,
        recordingGeneration: 3,
        pluginInstanceId: String::new(),
        connectionId: Some("connection-1".to_owned()),
        transactionId: None,
        deadlineUnixMs: u64::MAX,
        context: StageContext {
            transport: Some("tcp".to_owned()),
            host: Some("api.example.com".to_owned()),
            port: Some(443),
            interceptionMode: InterceptionMode::Intercept,
            ..StageContext::default()
        },
        payload,
    }
}

#[test]
fn parsesCompleteManifestAndAllowsTrustedRuntimeAndCrossStageModules() {
    let valid = r#"{
      "manifestVersion": 2,
      "id": "example.protocol",
      "name": "协议扩展",
      "description": "解析私有协议",
      "version": "2.1.0",
      "publisher": "example",
      "engines": { "host": ">=2.0.0 <3.0.0", "api": "2.x" },
      "runtime": { "kind": "wasm", "entry": "dist/plugin.wasm", "protocolVersion": "2.0" },
      "modules": [{
        "id": "decoder",
        "kind": "protocolDecoder",
        "subscriptions": [{ "stage": "tcpChunk", "order": 10 }]
      }],
      "capabilities": ["author.custom.behavior", "author.custom.behavior"],
      "limits": {
        "timeoutMs": 0,
        "maxPendingEvents": 0,
        "maxOutputBytes": 0,
        "maxStorageBytes": 0
      }
    }"#;
    let parsed = ExtensionManifest::parse(valid.as_bytes());
    assert!(parsed.is_ok(), "完整清单解析失败：{parsed:?}");

    let incompatibleProtocol = valid.replace(
        "\"protocolVersion\": \"2.0\"",
        "\"protocolVersion\": \"999\"",
    );
    assert!(ExtensionManifest::parse(incompatibleProtocol.as_bytes()).is_err());

    let unsafeEntry = valid.replace("dist/plugin.wasm", "../plugin.wasm");
    assert!(ExtensionManifest::parse(unsafeEntry.as_bytes()).is_err());

    let legacyRuntime = valid.replace("\"wasm\"", "\"legacyNative\"");
    assert!(ExtensionManifest::parse(legacyRuntime.as_bytes()).is_ok());

    let nativeRuntime = valid.replace("\"wasm\"", "\"native\"");
    assert!(ExtensionManifest::parse(nativeRuntime.as_bytes()).is_ok());

    let invalidModuleStage = valid
        .replace("\"protocolDecoder\"", "\"streamTransformer\"")
        .replace("\"tcpChunk\"", "\"serviceStarted\"");
    assert!(ExtensionManifest::parse(invalidModuleStage.as_bytes()).is_ok());
}

#[tokio::test]
async fn declaredCapabilitiesAreInformationalAndDoNotBlockTrustedModActions() {
    let kernel = ExtensionKernel::default();
    kernel.setServiceGeneration(7);
    let runtime = Arc::new(ScriptedRuntime::new(vec![Ok(ExtensionAction {
        eventId: "event-open-access".to_owned(),
        action: ActionKind::Modify,
        patch: Vec::new(),
        annotations: Vec::new(),
        output: Some(json!({ "bytes": [7, 8, 9] })),
    })]));
    let mut openManifest = manifest("example.open-access", "transform", 0);
    openManifest.capabilities.clear();
    kernel
        .register(openManifest, options(FailurePolicy::FailClosed), runtime)
        .expect("开放模式注册插件");

    let result = kernel
        .dispatch(envelope("event-open-access", json!({ "bytes": [1, 2, 3] })))
        .await
        .expect("开放模式允许插件修改事件");
    assert_eq!(result.finalPayload, json!({ "bytes": [7, 8, 9] }));
}

#[tokio::test]
async fn declaredLimitsAndEventDeadlineDoNotRestrictTrustedMod() {
    let kernel = ExtensionKernel::default();
    kernel.setServiceGeneration(7);
    let runtime = Arc::new(ScriptedRuntime::new(vec![Ok(ExtensionAction {
        eventId: "event-unrestricted".to_owned(),
        action: ActionKind::Modify,
        patch: Vec::new(),
        annotations: Vec::new(),
        output: Some(json!({ "bytes": [1, 2, 3, 4] })),
    })]));
    let mut unrestrictedManifest = manifest("example.unrestricted", "transform", 0);
    unrestrictedManifest.limits = ExtensionLimits {
        timeoutMs: 0,
        maxPendingEvents: 0,
        maxOutputBytes: 1,
        maxStorageBytes: 0,
    };
    kernel
        .register(
            unrestrictedManifest,
            options(FailurePolicy::FailClosed),
            runtime,
        )
        .expect("声明参数不限制可信 Mod");
    let mut unrestrictedEvent = envelope("event-unrestricted", json!({ "bytes": [9] }));
    unrestrictedEvent.deadlineUnixMs = 0;

    let result = kernel
        .dispatch(unrestrictedEvent)
        .await
        .expect("过期说明字段和输出建议值不会阻止调用");
    assert_eq!(result.finalPayload, json!({ "bytes": [1, 2, 3, 4] }));
    assert_eq!(kernel.snapshots()[0].inFlightInvocations, 0);
}

#[test]
fn rejectsUnknownModuleOrderButTreatsLimitsAsAuthorMetadata() {
    let kernel = ExtensionKernel::default();
    let runtime = Arc::new(ScriptedRuntime::new(Vec::new()));
    let mut invalidOrder = options(FailurePolicy::FailClosed);
    invalidOrder.moduleOrder.push("missing".to_owned());
    assert_eq!(
        kernel.register(
            manifest("example.invalid-order", "transform", 0),
            invalidOrder,
            runtime.clone(),
        ),
        Err(DispatchFailure::ConfigurationInvalid)
    );

    let mut invalidBudget = options(FailurePolicy::FailClosed);
    invalidBudget.limits = Some(ExtensionLimits {
        timeoutMs: 0,
        ..ExtensionLimits::default()
    });
    kernel
        .register(
            manifest("example.unrestricted-budget", "transform", 0),
            invalidBudget,
            runtime,
        )
        .expect("开放可信模式不把调度说明当作宿主门禁");
}

#[tokio::test]
async fn appliesOrderedModifyChainAndProducesAuditableTrace() {
    let kernel = ExtensionKernel::default();
    kernel.setServiceGeneration(7);
    kernel.setRecordingGeneration(3);
    let secondRuntime = Arc::new(ScriptedRuntime::new(vec![Ok(ExtensionAction {
        eventId: "event-1".to_owned(),
        action: ActionKind::Modify,
        patch: vec![json!({ "op": "add", "path": "/order/-", "value": "second" })],
        annotations: vec![json!({ "label": "second" })],
        output: None,
    })]));
    let firstRuntime = Arc::new(ScriptedRuntime::new(vec![Ok(ExtensionAction {
        eventId: "event-1".to_owned(),
        action: ActionKind::Modify,
        patch: vec![json!({ "op": "add", "path": "/order/-", "value": "first" })],
        annotations: vec![json!({ "label": "first" })],
        output: None,
    })]));
    kernel
        .register(
            manifest("example.second", "transform", 200),
            options(FailurePolicy::FailClosed),
            secondRuntime,
        )
        .expect("注册第二插件");
    kernel
        .register(
            manifest("example.first", "transform", 100),
            options(FailurePolicy::FailClosed),
            firstRuntime,
        )
        .expect("注册第一插件");

    let result = kernel
        .dispatch(envelope("event-1", json!({ "order": [] })))
        .await
        .expect("分发事件");

    assert_eq!(result.finalPayload, json!({ "order": ["first", "second"] }));
    assert_eq!(result.appliedActions.len(), 2);
    assert_eq!(result.annotations.len(), 2);
    assert_eq!(
        result
            .traces
            .iter()
            .map(|trace| trace.pluginId.as_str())
            .collect::<Vec<_>>(),
        vec!["example.first", "example.second"]
    );
    assert_eq!(kernel.invocationTraces(16), result.traces);
    assert_eq!(kernel.snapshots().len(), 2);
    kernel.clearInvocationTraces();
    assert!(kernel.invocationTraces(16).is_empty());
}

#[tokio::test]
async fn rejectsMutationInObserveOnlyStageWithoutApplyingBytes() {
    let kernel = ExtensionKernel::default();
    kernel.setServiceGeneration(7);
    let runtime = Arc::new(ScriptedRuntime::new(vec![Ok(ExtensionAction {
        eventId: "event-observe".to_owned(),
        action: ActionKind::Modify,
        patch: Vec::new(),
        annotations: Vec::new(),
        output: Some(json!({ "bytes": [9] })),
    })]));
    kernel
        .register(
            manifest("example.observe", "transform", 0),
            options(FailurePolicy::FailClosed),
            runtime,
        )
        .expect("注册插件");
    let mut event = envelope("event-observe", json!({ "bytes": [1] }));
    event.context.interceptionMode = InterceptionMode::ObserveOnly;

    assert_eq!(
        kernel.dispatch(event).await,
        Err(DispatchFailure::InvalidAction)
    );
}

#[tokio::test]
async fn ignoresFailedInstanceOnlyWhenUserSelectedFailOpen() {
    let kernel = ExtensionKernel::default();
    kernel.setServiceGeneration(7);
    let runtime = Arc::new(ScriptedRuntime::new(vec![Err("运行时退出".to_owned())]));
    kernel
        .register(
            manifest("example.failure", "transform", 0),
            options(FailurePolicy::FailOpen),
            runtime,
        )
        .expect("注册插件");

    let result = kernel
        .dispatch(envelope("event-failure", json!({ "bytes": [1, 2, 3] })))
        .await
        .expect("开放失败策略继续转发");
    assert_eq!(result.finalPayload, json!({ "bytes": [1, 2, 3] }));
    assert_eq!(
        result.traces[0].errorCode.as_deref(),
        Some("extensionRuntimeFailed")
    );
}

#[tokio::test]
async fn rejectsLateResultAfterServiceGenerationChanges() {
    let kernel = ExtensionKernel::default();
    kernel.setServiceGeneration(8);
    assert_eq!(
        kernel
            .dispatch(envelope("event-old", json!({ "bytes": [] })))
            .await,
        Err(DispatchFailure::GenerationExpired)
    );
}

#[test]
fn hotReplacementStopsOldRuntimeAndPublishesNewPlan() {
    let kernel = ExtensionKernel::default();
    let oldRuntime = Arc::new(ScriptedRuntime::new(Vec::new()));
    let newRuntime = Arc::new(ScriptedRuntime::new(Vec::new()));
    kernel
        .register(
            manifest("example.replace", "old", 0),
            options(FailurePolicy::FailClosed),
            oldRuntime.clone(),
        )
        .expect("注册旧实例");
    kernel
        .register(
            manifest("example.replace", "new", 0),
            options(FailurePolicy::FailClosed),
            newRuntime,
        )
        .expect("热替换实例");
    assert!(oldRuntime.stopped.load(Ordering::Acquire));
}
