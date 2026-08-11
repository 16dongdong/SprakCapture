#![allow(non_snake_case)]

use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, Version};
use http_proxy_core::{
    PipelineContext, PipelineRequestOutcome, RequestDraft, ResponseDraft, ToolPipeline,
    tools::{
        BlockCookiesConfiguration, BlockCookiesTool, BlockListConfiguration, BlockListTool,
        BlockMode, NoCachingConfiguration, NoCachingTool,
    },
};
use location_core::{LocationPattern, ResolvedLocation};

/// 构造流水线可直接消费的 HTTP 请求上下文，避免工具集成测试耦合真实监听器和上游网络。
fn context(host: &str) -> PipelineContext {
    PipelineContext::new(
        "127.0.0.1:38421".to_owned(),
        ResolvedLocation {
            protocol: "http".to_owned(),
            host: host.to_owned(),
            port: 80,
            path: "/resource".to_owned(),
            query: String::new(),
            display: format!("http://{host}/resource"),
        },
        RequestDraft {
            method: Method::GET,
            uri: format!("http://{host}/resource")
                .parse::<Uri>()
                .expect("集成测试 URI 必须有效"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: None,
        },
    )
}

/// 验证 Block List 在流水线中阻止出站、生成指定响应并留下 blocked 和 appliedTools 可观察状态。
#[tokio::test]
async fn blockListShortCircuitsWithConfiguredResponse() {
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(
            BlockListTool::new(BlockListConfiguration {
                mode: BlockMode::BlockList,
                locations: vec![LocationPattern {
                    host: "blocked.example.test".to_owned(),
                    ..LocationPattern::default()
                }],
                statusCode: 451,
                responseBody: "blocked by rule".to_owned(),
                closeConnection: true,
            })
            .expect("屏蔽列表配置必须有效"),
        ))
        .expect("屏蔽工具必须可注册");
    let mut context = context("blocked.example.test");

    assert_eq!(
        pipeline
            .runRequest(&mut context)
            .await
            .expect("屏蔽请求必须可执行"),
        PipelineRequestOutcome::Blocked
    );
    assert!(context.blocked && context.shortCircuit);
    assert_eq!(context.appliedTools, vec!["blockList"]);
    let response = context.response.expect("阻断必须产生合成响应");
    assert_eq!(response.status, StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS);
    assert_eq!(response.body, Some(Bytes::from_static(b"blocked by rule")));
    assert_eq!(response.headers["connection"], "close");
}

/// 验证 No Caching 与 Block Cookies 按固定顺序共同改写请求和响应头，且仅写入一次工具痕迹。
#[tokio::test]
async fn cacheAndCookieToolsTransformBothMessageDirections() {
    let pipeline = ToolPipeline::new();
    let scopedLocation = vec![LocationPattern {
        host: "api.example.test".to_owned(),
        ..LocationPattern::default()
    }];
    pipeline
        .register(Arc::new(
            NoCachingTool::new(NoCachingConfiguration {
                enabled: true,
                locations: scopedLocation.clone(),
                ..NoCachingConfiguration::default()
            })
            .expect("无缓存配置必须有效"),
        ))
        .expect("无缓存工具必须可注册");
    pipeline
        .register(Arc::new(
            BlockCookiesTool::new(BlockCookiesConfiguration {
                enabled: true,
                locations: scopedLocation,
                ..BlockCookiesConfiguration::default()
            })
            .expect("阻止 Cookie 配置必须有效"),
        ))
        .expect("Cookie 工具必须可注册");
    let mut context = context("api.example.test");
    context
        .request
        .headers
        .insert("If-None-Match", HeaderValue::from_static("\"r1\""));
    context
        .request
        .headers
        .insert("Cookie", HeaderValue::from_static("session=1"));

    assert_eq!(
        pipeline
            .runRequest(&mut context)
            .await
            .expect("请求工具必须可执行"),
        PipelineRequestOutcome::Forward
    );
    assert!(context.request.headers.get("if-none-match").is_none());
    assert!(context.request.headers.get("cookie").is_none());
    assert_eq!(context.request.headers["cache-control"], "no-cache");
    assert_eq!(context.appliedTools, vec!["noCaching", "blockCookies"]);

    let mut responseHeaders = HeaderMap::new();
    responseHeaders.insert("ETag", HeaderValue::from_static("\"r1\""));
    responseHeaders.append("Set-Cookie", HeaderValue::from_static("session=1"));
    responseHeaders.append("Set-Cookie", HeaderValue::from_static("theme=dark"));
    context.response = Some(ResponseDraft {
        status: StatusCode::OK,
        version: Version::HTTP_11,
        headers: responseHeaders,
        body: Some(Bytes::new()),
    });

    pipeline
        .runResponse(&mut context)
        .await
        .expect("响应工具必须可执行");
    let response = context.response.expect("上游响应必须保留");
    assert!(response.headers.get("etag").is_none());
    assert!(response.headers.get("set-cookie").is_none());
    assert_eq!(
        response.headers["cache-control"],
        "no-cache, no-store, must-revalidate"
    );
    assert_eq!(context.appliedTools, vec!["noCaching", "blockCookies"]);
}
