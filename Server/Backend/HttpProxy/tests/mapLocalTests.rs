#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use http::{HeaderMap, Method, StatusCode, Uri, Version};
use location_core::{LocationPattern, ResolvedLocation};

use http_proxy_core::{PipelineContext, PipelineDirective, PipelineTool, RequestDraft};

use http_proxy_core::tools::{MapLocalConfiguration, MapLocalRule, MapLocalTool};

/// 构造 Map Local 适配层请求上下文，使测试覆盖短路响应、标志和 appliedTools 同步路径。
fn context() -> PipelineContext {
    let location = ResolvedLocation {
        protocol: "http".to_owned(),
        host: "source.test".to_owned(),
        port: 80,
        path: "/fixture.json".to_owned(),
        query: String::new(),
        display: "http://source.test/fixture.json".to_owned(),
    };
    PipelineContext::new(
        "127.0.0.1:9000".to_owned(),
        location,
        RequestDraft {
            method: Method::GET,
            uri: "http://source.test/fixture.json"
                .parse::<Uri>()
                .expect("测试 URI 必须有效"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: None,
        },
    )
}

/// Map Local 命中必须返回流水线短路响应并保留规则级映射痕迹，后续 ToolPipeline 负责进入响应钩子。
#[tokio::test]
async fn pipelineAdapterCreatesShortCircuitResponse() {
    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let file = directory.path().join("fixture.json");
    tokio::fs::write(&file, br#"{"local":true}"#)
        .await
        .expect("本地夹具必须写入");
    let tool = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![MapLocalRule {
                id: "local-rule".to_owned(),
                enabled: true,
                location: LocationPattern {
                    protocol: "http".to_owned(),
                    host: "source.test".to_owned(),
                    port: "80".to_owned(),
                    path: "/fixture.json".to_owned(),
                    query: None,
                },
                localPath: file.to_string_lossy().into_owned(),
                isDirectory: false,
                statusCode: 200,
                responseHeaders: Vec::new(),
                contentTypeOverride: String::new(),
            }],
        },
        directory.path(),
    )
    .expect("本地规则必须有效");
    let mut context = context();

    let directive = tool
        .onRequest(&mut context)
        .await
        .expect("流水线适配不得失败");

    let PipelineDirective::ShortCircuit(response) = directive else {
        panic!("Map Local 必须短路出站");
    };
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, br#"{"local":true}"#.as_slice());
    assert!(context.flags.mappedLocal);
    assert_eq!(context.appliedTools, vec!["mapLocal:local-rule"]);
}
