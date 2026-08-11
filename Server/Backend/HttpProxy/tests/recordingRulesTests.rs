use std::sync::Arc;

use capture_core::{
    RecordingRule, RecordingRuleAction, RecordingRuleConfiguration, RecordingRuleKind,
    RecordingRuleRuntime, RecordingRuleSet,
};
use http::{HeaderMap, Method, Uri, Version};
use http_proxy_core::{
    PipelineContext, PipelineRequestOutcome, RecordingRulesTool, RequestDraft, ToolPipeline,
};
use location_core::ResolvedLocation;

/// 创建只拒绝指定域名的热更新运行时；该配置同时验证可视化编辑器的 wire 形状。
fn reject_runtime() -> RecordingRuleRuntime {
    RecordingRuleRuntime::new(RecordingRuleConfiguration {
        enabled: true,
        defaultAction: RecordingRuleAction::Record,
        ruleSets: vec![RecordingRuleSet {
            id: "primary".to_owned(),
            name: "阻断规则".to_owned(),
            enabled: true,
            rules: vec![RecordingRule {
                id: "reject".to_owned(),
                enabled: true,
                kind: RecordingRuleKind::Domain,
                value: "blocked.example".to_owned(),
                action: RecordingRuleAction::Reject,
            }],
        }],
    })
    .expect("规则配置应有效")
}

/// 创建不含正文的 GET 上下文；规则必须在任何上游连接或正文读取前完成决策。
fn request_context(host: &str) -> PipelineContext {
    PipelineContext::new(
        "127.0.0.1:49152".to_owned(),
        ResolvedLocation {
            protocol: "https".to_owned(),
            host: host.to_owned(),
            port: 443,
            path: "/".to_owned(),
            query: String::new(),
            display: format!("https://{host}/"),
        },
        RequestDraft {
            method: Method::GET,
            uri: Uri::from_static("https://blocked.example/"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: None,
        },
    )
}

#[tokio::test]
async fn rejects_matching_http_request_before_forwarding() {
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(RecordingRulesTool::new(reject_runtime())))
        .expect("规则工具应只注册一次");
    let mut context = request_context("blocked.example");

    assert_eq!(
        pipeline
            .runRequest(&mut context)
            .await
            .expect("规则执行应成功"),
        PipelineRequestOutcome::Blocked
    );
    assert!(context.blocked);
    assert_eq!(context.appliedTools, vec!["recordingRules"]);
    assert_eq!(context.response.expect("应生成阻断响应").status, 403);
}

#[tokio::test]
async fn forwards_unmatched_http_request() {
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(RecordingRulesTool::new(reject_runtime())))
        .expect("规则工具应只注册一次");
    let mut context = request_context("allowed.example");

    assert_eq!(
        pipeline
            .runRequest(&mut context)
            .await
            .expect("规则执行应成功"),
        PipelineRequestOutcome::Forward
    );
    assert!(!context.blocked);
}
