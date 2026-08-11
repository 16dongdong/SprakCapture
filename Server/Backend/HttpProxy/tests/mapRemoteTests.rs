#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use http::{HeaderMap, Method, Uri, Version};
use location_core::{LocationPattern, ResolvedLocation};

use http_proxy_core::{PipelineContext, PipelineDirective, PipelineTool, RequestDraft};

use http_proxy_core::tools::{
    MapRemoteConfiguration, MapRemoteRule, MapRemoteTarget, MapRemoteTool,
};

/// 构造流水线输入，覆盖原始 Location、可变 URI 与 Host 头三者必须同步的 Map Remote 边界。
fn context() -> PipelineContext {
    let location = ResolvedLocation {
        protocol: "http".to_owned(),
        host: "source.test".to_owned(),
        port: 80,
        path: "/v1/users".to_owned(),
        query: String::new(),
        display: "http://source.test/v1/users".to_owned(),
    };
    PipelineContext::new(
        "127.0.0.1:9000".to_owned(),
        location,
        RequestDraft {
            method: Method::GET,
            uri: "http://source.test/v1/users"
                .parse::<Uri>()
                .expect("测试 URI 必须有效"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: None,
        },
    )
}

/// Map Remote 命中必须只改写可变出站目标，原始 URL 保持不变并在 appliedTools 留下规则级映射信息。
#[tokio::test]
async fn pipelineAdapterRewritesRequestAndKeepsOriginalLocation() {
    let tool = MapRemoteTool::new(MapRemoteConfiguration {
        enabled: true,
        rules: vec![MapRemoteRule {
            id: "remote-rule".to_owned(),
            enabled: true,
            r#from: LocationPattern {
                protocol: "http".to_owned(),
                host: "source.test".to_owned(),
                port: "80".to_owned(),
                path: "/v1/*".to_owned(),
                query: None,
            },
            to: MapRemoteTarget {
                protocol: "http".to_owned(),
                host: "127.0.0.1".to_owned(),
                port: "8088".to_owned(),
                path: "/v2/*".to_owned(),
            },
        }],
    })
    .expect("远端规则必须有效");
    let mut context = context();

    let directive = tool
        .onRequest(&mut context)
        .await
        .expect("流水线适配不得失败");

    assert!(matches!(directive, PipelineDirective::Applied));
    assert_eq!(context.originalLocation.host, "source.test");
    assert_eq!(context.location.host, "127.0.0.1");
    assert_eq!(context.location.path, "/v2/users");
    assert_eq!(context.request.uri, "http://127.0.0.1:8088/v2/users");
    assert_eq!(context.request.headers["host"], "127.0.0.1:8088");
    assert!(context.flags.mappedRemote);
    assert_eq!(context.appliedTools, vec!["mapRemote:remote-rule"]);
}
