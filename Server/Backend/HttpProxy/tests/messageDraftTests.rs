#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use capture_core::HeaderField;
use http::{HeaderMap, Method, StatusCode, Uri, Version};
use location_core::ResolvedLocation;

use http_proxy_core::{PipelineContext, RequestDraft, ResponseDraft};

use http_proxy_core::tools::{
    EditableHttpMessage, MessageDraftError, applyRequestDraft, applyResponseDraft, editableRequest,
};

/// 构造带完整正文草稿的请求上下文，覆盖断点 continue 对 URL、头部和正文的一致性回写。
fn requestContext() -> PipelineContext {
    PipelineContext::new(
        "127.0.0.1:8080".to_owned(),
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
    )
}

/// 验证请求草稿编辑会同步 URL、Location、Host 和正文 framing，避免后续上游看到相互矛盾的消息。
#[test]
fn requestDraftUpdatesTargetAndBodyFraming() {
    let mut context = requestContext();
    applyRequestDraft(
        &mut context,
        EditableHttpMessage {
            method: Some("POST".to_owned()),
            url: Some("http://target.test:8080/new?mode=edited".to_owned()),
            statusCode: None,
            reason: None,
            headers: vec![HeaderField {
                name: "Content-Encoding".to_owned(),
                value: "gzip".to_owned(),
            }],
            bodyBase64: STANDARD.encode(b"new-body"),
        },
    )
    .expect("有效请求草稿必须可回写");
    assert_eq!(context.request.method, Method::POST);
    assert_eq!(context.location.host, "target.test");
    assert_eq!(context.location.port, 8080);
    assert_eq!(context.location.path, "/new");
    assert_eq!(context.request.headers["host"], "target.test:8080");
    assert_eq!(context.request.headers["content-encoding"], "identity");
    assert_eq!(context.request.headers["content-length"], "8");
    assert_eq!(context.request.body, Some(Bytes::from_static(b"new-body")));
}

/// 验证响应草稿可以更新状态和正文，而非空自定义 reason 会在写回前被确定性拒绝。
#[test]
fn responseDraftRejectsUnsupportedReason() {
    let mut response = ResponseDraft {
        status: StatusCode::OK,
        version: Version::HTTP_11,
        headers: HeaderMap::new(),
        body: Some(Bytes::from_static(b"old")),
    };
    let invalid = EditableHttpMessage {
        method: None,
        url: None,
        statusCode: Some(201),
        reason: Some("Created by draft".to_owned()),
        headers: Vec::new(),
        bodyBase64: STANDARD.encode(b"new"),
    };
    assert_eq!(
        applyResponseDraft(&mut response, invalid),
        Err(MessageDraftError::UnsupportedReason)
    );
    assert_eq!(response.status, StatusCode::OK);
}

/// 验证导出的草稿正文使用标准 base64，并保持前端可直接提交的固定字段形状。
#[test]
fn editableRequestEncodesBody() {
    let context = requestContext();
    let draft = editableRequest(&context);
    assert_eq!(draft.bodyBase64, STANDARD.encode(b"old"));
    assert_eq!(draft.method.as_deref(), Some("GET"));
}
