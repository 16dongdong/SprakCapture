#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use http::{HeaderMap, HeaderValue};
use location_core::{LocationPattern, ResolvedLocation};

use http_proxy_core::tools::{BlockCookiesConfiguration, BlockCookiesTool, HeaderMutation};

/// 构造固定 HTTP 位置，使 Cookie 剥离测试只验证工具本身的范围选择与字段语义。
fn location(host: &str) -> ResolvedLocation {
    ResolvedLocation {
        protocol: "http".to_owned(),
        host: host.to_owned(),
        port: 80,
        path: "/".to_owned(),
        query: String::new(),
        display: format!("http://{host}/"),
    }
}

/// 验证请求 Cookie 与多个响应 Set-Cookie 会同时被移除，保留无关字段供后续工具继续处理。
#[test]
fn stripsRequestAndAllResponseCookies() {
    let tool = BlockCookiesTool::new(BlockCookiesConfiguration {
        enabled: true,
        ..BlockCookiesConfiguration::default()
    })
    .expect("阻止 Cookie 配置必须有效");
    let mut requestHeaders = HeaderMap::new();
    requestHeaders.insert("Cookie", HeaderValue::from_static("session=1"));
    requestHeaders.insert("Accept", HeaderValue::from_static("application/json"));
    let requestMutation = tool
        .onRequest(&location("api.example.test"), &mut requestHeaders)
        .expect("请求处理必须成功");
    assert!(requestMutation.matched && requestMutation.changed);
    assert!(requestHeaders.get("cookie").is_none());
    assert_eq!(requestHeaders["accept"], "application/json");

    let mut responseHeaders = HeaderMap::new();
    responseHeaders.append("Set-Cookie", HeaderValue::from_static("session=1"));
    responseHeaders.append("Set-Cookie", HeaderValue::from_static("theme=dark"));
    let responseMutation = tool
        .onResponse(&location("api.example.test"), &mut responseHeaders)
        .expect("响应处理必须成功");
    assert!(responseMutation.matched && responseMutation.changed);
    assert!(
        responseHeaders
            .get_all("set-cookie")
            .iter()
            .next()
            .is_none()
    );
}

/// 验证未命中 Location 时 Cookie 保留，确保局部规则不会影响其它主机的会话状态。
#[test]
fn preservesCookiesOutsideLocationScope() {
    let tool = BlockCookiesTool::new(BlockCookiesConfiguration {
        enabled: true,
        locations: vec![LocationPattern {
            host: "private.example.test".to_owned(),
            ..LocationPattern::default()
        }],
        ..BlockCookiesConfiguration::default()
    })
    .expect("位置规则必须有效");
    let mut headers = HeaderMap::new();
    headers.insert("Cookie", HeaderValue::from_static("session=1"));
    assert_eq!(
        tool.onRequest(&location("api.example.test"), &mut headers)
            .expect("未命中位置必须可判定"),
        HeaderMutation::default()
    );
    assert_eq!(headers["cookie"], "session=1");
}
