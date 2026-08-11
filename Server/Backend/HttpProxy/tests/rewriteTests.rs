#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, Version, header::CONTENT_TYPE};
use location_core::ResolvedLocation;

use http_proxy_core::{
    PipelineContext, PipelineDirective, PipelineTool, RequestDraft, ResponseDraft,
};

use http_proxy_core::tools::{
    HeaderAction, RewriteConfiguration, RewriteError, RewriteRule, RewriteRuleType, RewriteSet,
    RewriteTool,
};

/// 构造带可编辑请求和响应草稿的上下文，使规则测试覆盖 URL、头部、正文及标志位的完整数据路径。
fn context() -> PipelineContext {
    PipelineContext::new(
        "127.0.0.1:9000".to_owned(),
        ResolvedLocation {
            protocol: "http".to_owned(),
            host: "source.test".to_owned(),
            port: 80,
            path: "/old".to_owned(),
            query: "mode=old".to_owned(),
            display: "http://source.test/old?mode=old".to_owned(),
        },
        RequestDraft {
            method: Method::GET,
            uri: "http://source.test/old?mode=old"
                .parse::<Uri>()
                .expect("测试 URI 必须有效"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: Some(Bytes::from_static(b"old request")),
        },
    )
}

/// 创建一个最小有效规则，调用方仅覆盖与测试场景相关的字段以避免掩盖协议默认值。
fn rule(ruleType: RewriteRuleType) -> RewriteRule {
    RewriteRule {
        id: format!("{ruleType:?}"),
        enabled: true,
        r#type: ruleType,
        matchRegex: "old".to_owned(),
        replace: "new".to_owned(),
        headerName: None,
        matchValueRegex: None,
        headerAction: None,
        caseSensitive: true,
        matchAllOccurrences: true,
    }
}

/// 验证请求 URL 与头部规则会同步刷新 Host、Location 和 rewritten 标志，后续出站层读取的是同一目标。
#[tokio::test]
async fn requestRulesRefreshTargetAndHeaders() {
    let mut hostRule = rule(RewriteRuleType::UrlHost);
    hostRule.matchRegex = "source\\.test".to_owned();
    hostRule.replace = "target.test".to_owned();
    let mut headerRule = rule(RewriteRuleType::RequestHeader);
    headerRule.id = "header".to_owned();
    headerRule.headerName = Some("X-Rewritten".to_owned());
    headerRule.headerAction = Some(HeaderAction::Add);
    headerRule.replace = "yes".to_owned();
    let tool = RewriteTool::new(RewriteConfiguration {
        enabled: true,
        sets: vec![RewriteSet {
            id: "set".to_owned(),
            name: "请求".to_owned(),
            enabled: true,
            locations: Vec::new(),
            rules: vec![hostRule, headerRule],
        }],
    })
    .expect("规则必须有效");
    let mut context = context();

    let directive = tool
        .onRequest(&mut context)
        .await
        .expect("请求改写必须成功");

    assert!(matches!(directive, PipelineDirective::Applied));
    assert_eq!(context.request.uri, "http://target.test/old?mode=old");
    assert_eq!(context.request.headers["host"], "target.test");
    assert_eq!(context.request.headers["x-rewritten"], "yes");
    assert_eq!(context.location.host, "target.test");
    assert!(context.flags.rewritten);
}

/// 验证响应正文改写会重建 Content-Length 并解除旧压缩编码声明，客户端读取到的是新的 identity 字节序列。
#[tokio::test]
async fn responseBodyRuleRefreshesFraming() {
    let tool = RewriteTool::new(RewriteConfiguration {
        enabled: true,
        sets: vec![RewriteSet {
            id: "set".to_owned(),
            name: "响应".to_owned(),
            enabled: true,
            locations: Vec::new(),
            rules: vec![rule(RewriteRuleType::ResponseBody)],
        }],
    })
    .expect("规则必须有效");
    let mut context = context();
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    headers.insert("content-encoding", HeaderValue::from_static("gzip"));
    context.response = Some(ResponseDraft {
        status: StatusCode::OK,
        version: Version::HTTP_11,
        headers,
        body: Some(Bytes::from_static(b"old old")),
    });

    let directive = tool
        .onResponse(&mut context)
        .await
        .expect("响应改写必须成功");

    assert!(matches!(directive, PipelineDirective::Applied));
    let response = context.response.expect("响应草稿必须保留");
    assert_eq!(response.body, Some(Bytes::from_static(b"new new")));
    assert_eq!(response.headers["content-encoding"], "identity");
    assert_eq!(response.headers["content-length"], "7");
}

/// 验证非法正则在配置写入前失败，旧工具实例不会进入包含未知匹配语义的运行状态。
#[test]
fn invalidRegexIsRejectedBeforeActivation() {
    let mut invalid = rule(RewriteRuleType::UrlPath);
    invalid.matchRegex = "(".to_owned();
    let result = RewriteTool::new(RewriteConfiguration {
        enabled: true,
        sets: vec![RewriteSet {
            id: "set".to_owned(),
            name: "无效".to_owned(),
            enabled: true,
            locations: Vec::new(),
            rules: vec![invalid],
        }],
    });
    assert!(matches!(result, Err(RewriteError::InvalidRegex)));
}

/// 验证 Location 不命中的集合不会改写消息，保证单个规则集不会越出其声明作用域。
#[tokio::test]
async fn locationScopeLeavesUnmatchedRequestUntouched() {
    let tool = RewriteTool::new(RewriteConfiguration {
        enabled: true,
        sets: vec![RewriteSet {
            id: "set".to_owned(),
            name: "受限".to_owned(),
            enabled: true,
            locations: vec![location_core::LocationPattern {
                host: "other.test".to_owned(),
                ..location_core::LocationPattern::default()
            }],
            rules: vec![rule(RewriteRuleType::UrlPath)],
        }],
    })
    .expect("规则必须有效");
    let mut context = context();

    let directive = tool
        .onRequest(&mut context)
        .await
        .expect("未命中规则必须通过");

    assert!(matches!(directive, PipelineDirective::Continue));
    assert_eq!(context.request.uri, "http://source.test/old?mode=old");
}
