#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use http::{HeaderMap, HeaderValue};
use location_core::{LocationPattern, ResolvedLocation};

use http_proxy_core::tools::{HeaderMutation, NoCachingConfiguration, NoCachingTool};

/// 构造固定 HTTP 位置，使头部改写测试聚焦于工具配置和匹配结果。
fn location(host: &str) -> ResolvedLocation {
    ResolvedLocation {
        protocol: "http".to_owned(),
        host: host.to_owned(),
        port: 80,
        path: "/resource".to_owned(),
        query: String::new(),
        display: format!("http://{host}/resource"),
    }
}

/// 验证请求与响应头都会被规范化为禁止缓存语义，且不依赖原始字段大小写。
#[test]
fn rewritesRequestAndResponseCacheHeaders() {
    let tool = NoCachingTool::new(NoCachingConfiguration {
        enabled: true,
        ..NoCachingConfiguration::default()
    })
    .expect("无缓存配置必须有效");
    let mut requestHeaders = HeaderMap::new();
    requestHeaders.insert("If-None-Match", HeaderValue::from_static("\"v1\""));
    requestHeaders.append("Cache-Control", HeaderValue::from_static("max-age=3600"));
    requestHeaders.append("Cache-Control", HeaderValue::from_static("public"));
    let requestMutation = tool
        .onRequest(&location("api.example.test"), &mut requestHeaders)
        .expect("请求头处理必须成功");
    assert!(requestMutation.matched && requestMutation.changed);
    assert!(requestHeaders.get("if-none-match").is_none());
    assert_eq!(requestHeaders["cache-control"], "no-cache");
    assert_eq!(requestHeaders["pragma"], "no-cache");

    let mut responseHeaders = HeaderMap::new();
    responseHeaders.append("ETag", HeaderValue::from_static("\"v1\""));
    responseHeaders.append("Set-Cookie", HeaderValue::from_static("kept=1"));
    let responseMutation = tool
        .onResponse(&location("api.example.test"), &mut responseHeaders)
        .expect("响应头处理必须成功");
    assert!(responseMutation.matched && responseMutation.changed);
    assert!(responseHeaders.get("etag").is_none());
    assert_eq!(
        responseHeaders["cache-control"],
        "no-cache, no-store, must-revalidate"
    );
    assert_eq!(
        responseHeaders.get("set-cookie"),
        Some(&HeaderValue::from_static("kept=1"))
    );
}

/// 验证 Location 未命中时保持头部原样，防止全局工具影响不在作用域内的请求。
#[test]
fn leavesUnmatchedLocationUnchanged() {
    let tool = NoCachingTool::new(NoCachingConfiguration {
        enabled: true,
        locations: vec![LocationPattern {
            host: "cache.example.test".to_owned(),
            ..LocationPattern::default()
        }],
        ..NoCachingConfiguration::default()
    })
    .expect("位置规则必须有效");
    let mut headers = HeaderMap::new();
    headers.insert("ETag", HeaderValue::from_static("\"v1\""));
    assert_eq!(
        tool.onResponse(&location("api.example.test"), &mut headers)
            .expect("未命中位置必须可判定"),
        HeaderMutation::default()
    );
    assert_eq!(headers["etag"], "\"v1\"");
}
