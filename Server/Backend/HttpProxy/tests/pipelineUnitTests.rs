#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri, Version};
use location_core::ResolvedLocation;
use parking_lot::Mutex;

use http_proxy_core::{
    PipelineContext, PipelineDirective, PipelineError, PipelineRequestOutcome, PipelineTool,
    RequestDraft, ResponseDraft, SyntheticResponse, ToolId, ToolPhase, ToolPipeline,
    ToolRegistration,
};

/// 使用共享事件表构造顺序可观测的测试工具；生产工具不依赖此辅助类型。
struct TestTool {
    registration: ToolRegistration,
    requestDirective: PipelineDirective,
    responseDirective: PipelineDirective,
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl PipelineTool for TestTool {
    /// 返回测试工具的固定运行快照，模拟真实工具从配置锁读取 enabled 的行为。
    fn registration(&self) -> ToolRegistration {
        self.registration.clone()
    }

    /// 记录请求阶段调用顺序，并返回预设结果以覆盖普通、短路和阻断分支。
    async fn onRequest(
        &self,
        _context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        self.events
            .lock()
            .push(format!("request:{}", self.registration.id.asStr()));
        Ok(self.requestDirective.clone())
    }

    /// 记录响应阶段调用顺序，证明合成响应与真实响应共用固定响应工具槽。
    async fn onResponse(
        &self,
        _context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        self.events
            .lock()
            .push(format!("response:{}", self.registration.id.asStr()));
        Ok(self.responseDirective.clone())
    }
}

/// 创建最小请求上下文，避免顺序测试与网络、录制或正文存储耦合。
fn testContext() -> PipelineContext {
    PipelineContext::new(
        "127.0.0.1:12345".to_owned(),
        ResolvedLocation {
            protocol: "http".to_owned(),
            host: "example.test".to_owned(),
            port: 80,
            path: "/".to_owned(),
            query: String::new(),
            display: "http://example.test/".to_owned(),
        },
        RequestDraft {
            method: Method::GET,
            uri: "http://example.test/"
                .parse::<Uri>()
                .expect("测试 URI 必须有效"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: None,
        },
    )
}

/// 固定顺序必须覆盖注册顺序，防止控制面热更新或加载次序改变线上工具语义。
#[tokio::test]
async fn runsHooksInDocumentedOrder() {
    let pipeline = ToolPipeline::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    for (id, phases) in [
        (
            ToolId::Throttling,
            vec![ToolPhase::Request, ToolPhase::Response],
        ),
        (
            ToolId::Rewrite,
            vec![ToolPhase::Request, ToolPhase::Response],
        ),
        (ToolId::MapLocal, vec![ToolPhase::Request]),
    ] {
        pipeline
            .register(Arc::new(TestTool {
                registration: ToolRegistration::new(id, phases, true),
                requestDirective: PipelineDirective::Applied,
                responseDirective: PipelineDirective::Applied,
                events: events.clone(),
            }))
            .expect("测试工具必须注册");
    }
    let mut context = testContext();

    assert_eq!(
        pipeline
            .runRequest(&mut context)
            .await
            .expect("请求钩子必须成功"),
        PipelineRequestOutcome::Forward
    );
    context.response = Some(ResponseDraft {
        status: StatusCode::OK,
        version: Version::HTTP_11,
        headers: HeaderMap::new(),
        body: Some(Bytes::new()),
    });
    pipeline
        .runResponse(&mut context)
        .await
        .expect("响应钩子必须成功");

    assert_eq!(
        *events.lock(),
        vec![
            "request:mapLocal",
            "request:rewrite",
            "request:throttling",
            "response:rewrite",
            "response:throttling",
        ]
    );
    assert_eq!(
        context.appliedTools,
        vec!["mapLocal", "rewrite", "throttling"]
    );
}

/// 短路后不得触发后续请求工具，但必须让响应工具改写合成响应并留下完整痕迹。
#[tokio::test]
async fn shortCircuitStillRunsResponseHooks() {
    let pipeline = ToolPipeline::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    pipeline
        .register(Arc::new(TestTool {
            registration: ToolRegistration::new(ToolId::MapLocal, vec![ToolPhase::Request], true),
            requestDirective: PipelineDirective::ShortCircuit(SyntheticResponse::new(
                StatusCode::OK,
                Bytes::from_static(b"local"),
            )),
            responseDirective: PipelineDirective::Continue,
            events: events.clone(),
        }))
        .expect("Map Local 必须注册");
    pipeline
        .register(Arc::new(TestTool {
            registration: ToolRegistration::new(
                ToolId::Rewrite,
                vec![ToolPhase::Request, ToolPhase::Response],
                true,
            ),
            requestDirective: PipelineDirective::Applied,
            responseDirective: PipelineDirective::Applied,
            events: events.clone(),
        }))
        .expect("Rewrite 必须注册");
    let mut context = testContext();

    assert_eq!(
        pipeline
            .runRequest(&mut context)
            .await
            .expect("短路必须成功"),
        PipelineRequestOutcome::Synthetic
    );
    assert!(context.shortCircuit);
    assert_eq!(
        context
            .response
            .as_ref()
            .expect("短路必须设置响应")
            .body
            .as_deref(),
        Some(b"local".as_slice())
    );
    pipeline
        .runResponse(&mut context)
        .await
        .expect("合成响应必须进入响应钩子");

    assert_eq!(*events.lock(), vec!["request:mapLocal", "response:rewrite"]);
    assert_eq!(context.appliedTools, vec!["mapLocal", "rewrite"]);
}

/// disabled 工具必须完全跳过，避免关闭工具后仍改写线上消息或留下误导性痕迹。
#[tokio::test]
async fn skipsDisabledTool() {
    let pipeline = ToolPipeline::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    pipeline
        .register(Arc::new(TestTool {
            registration: ToolRegistration::new(ToolId::NoCaching, vec![ToolPhase::Request], false),
            requestDirective: PipelineDirective::Applied,
            responseDirective: PipelineDirective::Continue,
            events: events.clone(),
        }))
        .expect("禁用工具也必须可注册");
    let mut context = testContext();

    assert_eq!(
        pipeline
            .runRequest(&mut context)
            .await
            .expect("空阶段必须成功"),
        PipelineRequestOutcome::Forward
    );
    assert!(events.lock().is_empty());
    assert!(context.appliedTools.is_empty());
}
