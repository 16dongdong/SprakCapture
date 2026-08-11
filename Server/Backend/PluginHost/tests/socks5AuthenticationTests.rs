#![allow(non_snake_case)]

use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use plugin_host::{
    ActionKind, EngineRequirements, ExtensionAction, ExtensionLimits, ExtensionManifest,
    ExtensionMatch, ExtensionModule, ExtensionRuntime, ExtensionRuntimeKind,
    ExtensionRuntimeManifest, FailurePolicy, ModuleKind, PluginExecutionOptions, PluginHost,
    RuntimeInvocation, Socks5AuthenticationDecision, Socks5AuthenticationRequest, Stage,
    StageSubscription,
};
use serde_json::json;

/// 返回动态认证动作的测试运行时；它同时验证口令只出现在当前调用正文且上下文包含客户端地址。
struct AuthenticationRuntime;

impl ExtensionRuntime for AuthenticationRuntime {
    /// 根据测试凭据返回主体 ID；动作 eventId 必须使用宿主当前值，验证真实调度契约而非固定夹具。
    fn invoke<'a>(
        &'a self,
        invocation: RuntimeInvocation,
    ) -> Pin<Box<dyn Future<Output = Result<ExtensionAction, String>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(invocation.envelope.stage, Stage::Socks5Authentication);
            assert_eq!(
                invocation.envelope.context.address.as_deref(),
                Some("127.0.0.1:50000")
            );
            assert_eq!(invocation.envelope.payload["username"], "alice");
            assert_eq!(invocation.envelope.payload["password"], "plugin-secret");
            Ok(ExtensionAction {
                eventId: invocation.envelope.eventId,
                action: ActionKind::Respond,
                patch: Vec::new(),
                annotations: Vec::new(),
                output: Some(json!({ "principalId": "plugin-principal" })),
            })
        })
    }

    /// 测试运行时没有外部资源，停止通知无需执行清理。
    fn stop(&self) {}
}

/// 构造只订阅 SOCKS5 认证阶段的最小完整清单，匹配条件为空表示接管全部 SOCKS5 客户端。
fn authenticationManifest() -> ExtensionManifest {
    ExtensionManifest {
        manifestVersion: 2,
        id: "test.socks5-auth".to_owned(),
        name: "SOCKS5 认证测试".to_owned(),
        description: "接管 RFC1929 认证".to_owned(),
        version: "1.0.0".to_owned(),
        publisher: "tests".to_owned(),
        engines: EngineRequirements {
            host: ">=1.0.0".to_owned(),
            api: "2.x".to_owned(),
        },
        runtime: ExtensionRuntimeManifest {
            kind: ExtensionRuntimeKind::Wasm,
            entry: "plugin.wasm".to_owned(),
            protocolVersion: Some("2.0".to_owned()),
            arguments: Vec::new(),
        },
        modules: vec![ExtensionModule {
            id: "authentication".to_owned(),
            kind: ModuleKind::TrafficHandler,
            subscriptions: vec![StageSubscription {
                stage: Stage::Socks5Authentication,
                order: 0,
                matchRule: ExtensionMatch::default(),
            }],
            contributes: Vec::new(),
        }],
        capabilities: vec!["authentication.provide".to_owned()],
        dependencies: BTreeMap::new(),
        limits: ExtensionLimits::default(),
        failurePolicy: FailurePolicy::FailClosed,
        configurationSchema: None,
        contributes: None,
        extensions: BTreeMap::new(),
    }
}

/// 验证 PluginHost 把 SOCKS5 凭据交给订阅插件，并返回插件提供的主体而不是原始用户名。
#[tokio::test]
async fn pluginControlsSocks5AuthenticationIdentity() {
    let host = PluginHost::disabled();
    host.extensionKernel()
        .register(
            authenticationManifest(),
            PluginExecutionOptions::default(),
            Arc::new(AuthenticationRuntime),
        )
        .expect("认证插件必须注册成功");

    let decision = host
        .authenticateSocks5(Socks5AuthenticationRequest {
            connectionId: "session-1".to_owned(),
            clientAddress: "127.0.0.1:50000".to_owned(),
            username: "alice".to_owned(),
            password: "plugin-secret".to_owned(),
        })
        .await;
    assert_eq!(
        decision,
        Socks5AuthenticationDecision::Accepted {
            principalId: "plugin-principal".to_owned()
        }
    );
}
