#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, Version, header::CONTENT_TYPE};
use location_core::{LocationPattern, ResolvedLocation};
use tokio::time::timeout;

use http_proxy_core::{
    PipelineContext, PipelineDirective, PipelineTool, RequestDraft, ResponseDraft,
};

use http_proxy_core::tools::{
    BreakpointPhase, BreakpointRule, BreakpointTimeoutAction, BreakpointsConfiguration,
    BreakpointsTool, EditableHttpMessage,
};

/// 构造带真实事务 ID 和已物化正文的请求上下文，使暂停队列能通过公开控制 API 被准确定位。
fn requestContext() -> PipelineContext {
    let mut context = PipelineContext::new(
        "127.0.0.1:9000".to_owned(),
        ResolvedLocation {
            protocol: "http".to_owned(),
            host: "source.test".to_owned(),
            port: 80,
            path: "/old".to_owned(),
            query: String::new(),
            display: "http://source.test/old".to_owned(),
        },
        RequestDraft {
            method: Method::GET,
            uri: "http://source.test/old"
                .parse::<Uri>()
                .expect("测试 URI 必须有效"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: Some(Bytes::from_static(b"old")),
        },
    );
    context.bindTransaction("transaction-1".to_owned(), "session-1".to_owned());
    context
}

/// 创建覆盖指定阶段的单条全局规则，测试只关注暂停和恢复语义而不重复 Location 解析细节。
fn tool(onRequest: bool, onResponse: bool) -> BreakpointsTool {
    BreakpointsTool::new(BreakpointsConfiguration {
        enabled: true,
        rules: vec![BreakpointRule {
            id: "rule-1".to_owned(),
            enabled: true,
            location: LocationPattern::default(),
            onRequest,
            onResponse,
        }],
        suspendTimeoutSeconds: 1,
        maxSuspended: 4,
        onTimeout: BreakpointTimeoutAction::Continue,
    })
    .expect("断点规则必须有效")
}

/// 等待异步流水线注册暂停快照，超时即表示工具没有按规则暴露可继续的控制面状态。
async fn waitForSuspended(tool: &BreakpointsTool) {
    timeout(Duration::from_millis(500), async {
        while tool.suspendedBreakpoints().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("断点快照必须在超时前进入队列");
}

/// 验证请求暂停可经 continue 回写 URL、Host 和正文，且队列项会在发送继续信号时立即清理。
#[tokio::test]
async fn requestBreakpointContinuesWithEditedDraft() {
    let tool = tool(true, false);
    let mut changes = tool.subscribeSuspendedChanges();
    let workerTool = tool.clone();
    let task = tokio::spawn(async move {
        let mut context = requestContext();
        let directive = workerTool
            .onRequest(&mut context)
            .await
            .expect("请求断点必须完成");
        (directive, context)
    });
    waitForSuspended(&tool).await;
    timeout(Duration::from_millis(500), changes.changed())
        .await
        .expect("挂起必须发布队列变更")
        .expect("变更订阅器必须保持打开");
    assert_eq!(*changes.borrow_and_update(), tool.suspendedRevision());
    let snapshot = tool.suspendedBreakpoints().pop().expect("必须存在暂停快照");
    tool.continueBreakpoint(
        &snapshot.transactionId,
        EditableHttpMessage {
            method: Some("POST".to_owned()),
            url: Some("http://target.test/new".to_owned()),
            statusCode: None,
            reason: None,
            headers: Vec::new(),
            bodyBase64: STANDARD.encode(b"edited"),
        },
    )
    .expect("有效草稿必须继续请求");
    timeout(Duration::from_millis(500), changes.changed())
        .await
        .expect("继续必须发布队列变更")
        .expect("变更订阅器必须保持打开");
    let (directive, context) = task.await.expect("请求任务不得 panic");

    assert!(matches!(directive, PipelineDirective::Applied));
    assert!(tool.suspendedBreakpoints().is_empty());
    assert_eq!(context.request.method, Method::POST);
    assert_eq!(context.request.uri, "http://target.test/new");
    assert_eq!(context.request.headers["host"], "target.test");
    assert_eq!(context.request.body, Some(Bytes::from_static(b"edited")));
    assert!(context.flags.breakpointHit);
    assert!(!context.suspended);
}

/// 验证超时继续会使用进入队列时的草稿并移除暂停项，避免无人处理时永久占用代理任务。
#[tokio::test]
async fn timeoutContinuesAndRemovesSuspendedSlot() {
    let tool = tool(true, false);
    let mut context = requestContext();
    let directive = tool
        .onRequest(&mut context)
        .await
        .expect("超时继续必须完成");

    assert!(matches!(directive, PipelineDirective::Applied));
    assert!(tool.suspendedBreakpoints().is_empty());
    assert_eq!(context.request.uri, "http://source.test/old");
}

/// 验证响应暂停执行 abort 后会替换为 502 响应草稿，同时仍满足响应阶段不得短路的流水线契约。
#[tokio::test]
async fn responseBreakpointAbortsWithBadGateway() {
    let tool = tool(false, true);
    let workerTool = tool.clone();
    let task = tokio::spawn(async move {
        let mut context = requestContext();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        context.response = Some(ResponseDraft {
            status: StatusCode::OK,
            version: Version::HTTP_11,
            headers,
            body: Some(Bytes::from_static(b"body")),
        });
        let directive = workerTool
            .onResponse(&mut context)
            .await
            .expect("响应断点必须完成");
        (directive, context)
    });
    waitForSuspended(&tool).await;
    let snapshot = tool
        .suspendedBreakpoints()
        .pop()
        .expect("必须存在响应暂停快照");
    assert_eq!(snapshot.phase, BreakpointPhase::Response);
    tool.abortBreakpoint(&snapshot.transactionId)
        .expect("中止必须发送给等待任务");
    let (directive, context) = task.await.expect("响应任务不得 panic");

    assert!(matches!(directive, PipelineDirective::Applied));
    assert_eq!(
        context.response.expect("必须保留响应草稿").status,
        StatusCode::BAD_GATEWAY
    );
}

/// 验证响应暂停可经 continue 修改状态、头部和正文，覆盖请求阶段已有测试未触及的响应分辨通道。
#[tokio::test]
async fn responseBreakpointContinuesWithEditedDraft() {
    let tool = tool(false, true);
    let workerTool = tool.clone();
    let task = tokio::spawn(async move {
        let mut context = requestContext();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        context.response = Some(ResponseDraft {
            status: StatusCode::OK,
            version: Version::HTTP_11,
            headers,
            body: Some(Bytes::from_static(b"before")),
        });
        let directive = workerTool
            .onResponse(&mut context)
            .await
            .expect("响应断点必须完成");
        (directive, context)
    });
    waitForSuspended(&tool).await;
    let snapshot = tool
        .suspendedBreakpoints()
        .pop()
        .expect("必须存在响应暂停快照");
    tool.continueBreakpoint(
        &snapshot.transactionId,
        EditableHttpMessage {
            method: None,
            url: None,
            statusCode: Some(StatusCode::CREATED.as_u16()),
            reason: None,
            headers: vec![capture_core::HeaderField {
                name: CONTENT_TYPE.as_str().to_owned(),
                value: "application/json".to_owned(),
            }],
            bodyBase64: STANDARD.encode(br#"{"continued":true}"#),
        },
    )
    .expect("有效响应草稿必须继续响应");
    let (directive, context) = task.await.expect("响应任务不得 panic");
    let response = context.response.expect("继续后必须保留响应草稿");

    assert!(matches!(directive, PipelineDirective::Applied));
    assert_eq!(response.status, StatusCode::CREATED);
    assert_eq!(response.headers[CONTENT_TYPE], "application/json");
    assert_eq!(
        response.body,
        Some(Bytes::from_static(br#"{"continued":true}"#))
    );
    assert!(tool.suspendedBreakpoints().is_empty());
}

/// 验证承载暂停响应的代理任务被取消后立即释放队列项，避免控制端继续看到已关闭接收端的过期草稿。
#[tokio::test]
async fn cancelledResponseRemovesSuspendedSlot() {
    let tool = tool(false, true);
    let mut changes = tool.subscribeSuspendedChanges();
    let workerTool = tool.clone();
    let task = tokio::spawn(async move {
        let mut context = requestContext();
        context.response = Some(ResponseDraft {
            status: StatusCode::OK,
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: Some(Bytes::from_static(b"body")),
        });
        workerTool.onResponse(&mut context).await
    });
    waitForSuspended(&tool).await;
    timeout(Duration::from_millis(500), changes.changed())
        .await
        .expect("挂起必须发布队列变更")
        .expect("变更订阅器必须保持打开");
    changes.borrow_and_update();

    task.abort();
    let _ = task.await;
    timeout(Duration::from_millis(500), changes.changed())
        .await
        .expect("任务取消必须发布队列变更")
        .expect("变更订阅器必须保持打开");

    assert!(tool.suspendedBreakpoints().is_empty());
}

/// 验证暂停槽位达到上限时新命中直接通过且不设置命中标志，防止单个控制面故障耗尽代理任务。
#[tokio::test]
async fn fullQueueLetsNewRequestContinue() {
    let tool = BreakpointsTool::new(BreakpointsConfiguration {
        maxSuspended: 1,
        ..tool(true, false).configuration()
    })
    .expect("队列上限配置必须有效");
    let firstTool = tool.clone();
    let first = tokio::spawn(async move {
        let mut context = requestContext();
        firstTool.onRequest(&mut context).await
    });
    waitForSuspended(&tool).await;
    let mut secondContext = requestContext();
    secondContext.bindTransaction("transaction-2".to_owned(), "session-1".to_owned());
    let second = tool
        .onRequest(&mut secondContext)
        .await
        .expect("满队列请求必须正常通过");
    assert!(matches!(second, PipelineDirective::Continue));
    assert!(!secondContext.flags.breakpointHit);
    let snapshot = tool.suspendedBreakpoints().pop().expect("首个请求仍应等待");
    tool.abortBreakpoint(&snapshot.transactionId)
        .expect("清理首个等待请求必须成功");
    let _ = first.await.expect("首个请求任务不得 panic");
}
