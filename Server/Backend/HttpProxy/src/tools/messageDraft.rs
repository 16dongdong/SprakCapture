use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use capture_core::HeaderField;
use http::{
    HeaderMap, HeaderValue, Method, StatusCode, Uri,
    header::{CONTENT_ENCODING, CONTENT_LENGTH, HOST, HeaderName, TRANSFER_ENCODING},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{PipelineContext, ResponseDraft, target::parsePipelineTarget};

pub(crate) const maximumEditableBodyBytes: usize = 8 * 1024 * 1024;
pub(crate) const maximumEditableBodyCharacters: usize = maximumEditableBodyBytes.div_ceil(3) * 4;
const maximumEditableHeaders: usize = 256;
const maximumEditableMethodCharacters: usize = 32;
const maximumEditableUrlCharacters: usize = 8_192;
const contentMd5Name: HeaderName = HeaderName::from_static("content-md5");

/// 描述断点继续操作可回写的标准 HTTP 消息草稿；正文采用 base64，避免 JSON 控制面混淆二进制字节。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditableHttpMessage {
    pub method: Option<String>,
    pub url: Option<String>,
    pub statusCode: Option<u16>,
    pub reason: Option<String>,
    pub headers: Vec<HeaderField>,
    pub bodyBase64: String,
}

/// 描述可编辑草稿在控制边界和流水线回写阶段的稳定失败类型；不包含原始正文、头字段值或 URI。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MessageDraftError {
    #[error("error.breakpoints.tooManyHeaders")]
    TooManyHeaders,
    #[error("error.breakpoints.invalidHeaderName")]
    InvalidHeaderName,
    #[error("error.breakpoints.invalidHeaderValue")]
    InvalidHeaderValue,
    #[error("error.breakpoints.invalidBodyBase64")]
    InvalidBodyBase64,
    #[error("error.breakpoints.bodyTooLarge")]
    BodyTooLarge,
    #[error("error.breakpoints.invalidRequestMethod")]
    InvalidRequestMethod,
    #[error("error.breakpoints.invalidRequestUrl")]
    InvalidRequestUrl,
    #[error("error.breakpoints.invalidResponseStatus")]
    InvalidResponseStatus,
    #[error("error.breakpoints.unsupportedReason")]
    UnsupportedReason,
    #[error("error.breakpoints.missingResponse")]
    MissingResponse,
}

impl MessageDraftError {
    /// 返回 API、MCP 与流水线错误映射使用的稳定机器码，避免将草稿内容写入日志或响应。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooManyHeaders => "breakpointTooManyHeaders",
            Self::InvalidHeaderName => "breakpointInvalidHeaderName",
            Self::InvalidHeaderValue => "breakpointInvalidHeaderValue",
            Self::InvalidBodyBase64 => "breakpointInvalidBodyBase64",
            Self::BodyTooLarge => "breakpointBodyTooLarge",
            Self::InvalidRequestMethod => "breakpointInvalidRequestMethod",
            Self::InvalidRequestUrl => "breakpointInvalidRequestUrl",
            Self::InvalidResponseStatus => "breakpointInvalidResponseStatus",
            Self::UnsupportedReason => "breakpointUnsupportedReason",
            Self::MissingResponse => "breakpointMissingResponse",
        }
    }

    /// 返回由前端语言包渲染的稳定消息键；模块本身不拼接用户可见错误文本。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::TooManyHeaders => "error.breakpoints.tooManyHeaders",
            Self::InvalidHeaderName => "error.breakpoints.invalidHeaderName",
            Self::InvalidHeaderValue => "error.breakpoints.invalidHeaderValue",
            Self::InvalidBodyBase64 => "error.breakpoints.invalidBodyBase64",
            Self::BodyTooLarge => "error.breakpoints.bodyTooLarge",
            Self::InvalidRequestMethod => "error.breakpoints.invalidRequestMethod",
            Self::InvalidRequestUrl => "error.breakpoints.invalidRequestUrl",
            Self::InvalidResponseStatus => "error.breakpoints.invalidResponseStatus",
            Self::UnsupportedReason => "error.breakpoints.unsupportedReason",
            Self::MissingResponse => "error.breakpoints.missingResponse",
        }
    }
}

/// 根据请求草稿生成可跨控制 API 传输的编辑副本；`body=None` 表示调用方未物化正文，此时导出为空正文。
pub fn editableRequest(context: &PipelineContext) -> EditableHttpMessage {
    EditableHttpMessage {
        method: Some(context.request.method.as_str().to_owned()),
        url: Some(context.request.uri.to_string()),
        statusCode: None,
        reason: None,
        headers: headerFields(&context.request.headers),
        bodyBase64: encodeBody(context.request.body.as_deref()),
    }
}

/// 根据响应草稿生成可编辑副本；调用方仅在已有响应时调用，因此返回值始终含当前状态码。
pub fn editableResponse(response: &ResponseDraft) -> EditableHttpMessage {
    EditableHttpMessage {
        method: None,
        url: None,
        statusCode: Some(response.status.as_u16()),
        reason: None,
        headers: headerFields(&response.headers),
        bodyBase64: encodeBody(response.body.as_deref()),
    }
}

/// 在继续请求断点前校验并回写草稿；URL 改写同步刷新 Location，正文改动才重建消息分帧字段。
pub fn applyRequestDraft(
    context: &mut PipelineContext,
    draft: EditableHttpMessage,
) -> Result<(), MessageDraftError> {
    validateRequestDraft(&draft)?;
    let headers = headerMap(&draft.headers)?;
    let body = decodeBody(&draft.bodyBase64)?;
    let bodyChanged = body.as_slice() != context.request.body.as_deref().unwrap_or_default();
    if let Some(method) = draft.method {
        context.request.method = method
            .parse::<Method>()
            .map_err(|_| MessageDraftError::InvalidRequestMethod)?;
    }
    if let Some(url) = draft.url {
        context.request.uri = parseAbsoluteUri(&url)?;
    }
    context.request.headers = headers;
    if bodyChanged {
        normalizeModifiedBodyHeaders(&mut context.request.headers, body.len());
    }
    context.request.body = Some(Bytes::from(body));
    refreshRequestLocation(context)
}

/// 在继续响应断点前校验并回写草稿；响应 reason 不可由 Hyper 稳定写入，因此非空自定义值在控制边界拒绝。
pub fn applyResponseDraft(
    response: &mut ResponseDraft,
    draft: EditableHttpMessage,
) -> Result<(), MessageDraftError> {
    validateResponseDraft(&draft)?;
    let headers = headerMap(&draft.headers)?;
    let body = decodeBody(&draft.bodyBase64)?;
    let bodyChanged = body.as_slice() != response.body.as_deref().unwrap_or_default();
    if let Some(statusCode) = draft.statusCode {
        response.status = StatusCode::from_u16(statusCode)
            .map_err(|_| MessageDraftError::InvalidResponseStatus)?;
    }
    response.headers = headers;
    if bodyChanged {
        normalizeModifiedBodyHeaders(&mut response.headers, body.len());
    }
    response.body = Some(Bytes::from(body));
    Ok(())
}

/// 校验待继续的请求草稿，禁止把响应专属字段或非绝对 URL 混入即将出站的请求。
pub(crate) fn validateRequestDraft(draft: &EditableHttpMessage) -> Result<(), MessageDraftError> {
    if draft.statusCode.is_some() || nonEmpty(&draft.reason) {
        return Err(MessageDraftError::InvalidRequestUrl);
    }
    if let Some(method) = &draft.method {
        if method.len() > maximumEditableMethodCharacters {
            return Err(MessageDraftError::InvalidRequestMethod);
        }
        method
            .parse::<Method>()
            .map_err(|_| MessageDraftError::InvalidRequestMethod)?;
    }
    if let Some(url) = &draft.url {
        if url.len() > maximumEditableUrlCharacters {
            return Err(MessageDraftError::InvalidRequestUrl);
        }
        parseAbsoluteUri(url)?;
    }
    validateSharedDraft(draft)
}

/// 校验待继续的响应草稿，禁止把请求专属字段和无法可靠落线的自定义 reason 写入响应路径。
pub(crate) fn validateResponseDraft(draft: &EditableHttpMessage) -> Result<(), MessageDraftError> {
    if draft.method.is_some() || draft.url.is_some() {
        return Err(MessageDraftError::InvalidResponseStatus);
    }
    if let Some(statusCode) = draft.statusCode {
        StatusCode::from_u16(statusCode).map_err(|_| MessageDraftError::InvalidResponseStatus)?;
    }
    if nonEmpty(&draft.reason) {
        return Err(MessageDraftError::UnsupportedReason);
    }
    validateSharedDraft(draft)
}

/// 规范化正文改写后的 framing 和完整性字段；内容已成为 identity 字节流时不得保留旧压缩或长度声明。
pub(crate) fn normalizeModifiedBodyHeaders(headers: &mut HeaderMap, bodyLength: usize) {
    if headers.contains_key(CONTENT_ENCODING) {
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("identity"));
    }
    headers.remove(TRANSFER_ENCODING);
    headers.remove(contentMd5Name);
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&bodyLength.to_string())
            .expect("usize 十进制长度必须满足 HTTP HeaderValue 语法"),
    );
}

/// 判断响应或请求正文是否属于默认可安全处理的文本媒体类型；缺失或二进制类型一律不进入正则改写。
pub(crate) fn isTextBody(headers: &HeaderMap) -> bool {
    let Some(contentType) = headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let mediaType = contentType
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    mediaType.starts_with("text/")
        || matches!(
            mediaType.as_str(),
            "application/json"
                | "application/ld+json"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/x-www-form-urlencoded"
                | "application/graphql"
        )
        || mediaType.ends_with("+json")
        || mediaType.ends_with("+xml")
}

/// 将 HeaderMap 转为保序可传输字段列表；重复 Set-Cookie 等字段保留为独立条目。
fn headerFields(headers: &HeaderMap) -> Vec<HeaderField> {
    headers
        .iter()
        .map(|(name, value)| HeaderField {
            name: name.as_str().to_owned(),
            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
        })
        .collect()
}

/// 将控制面字段列表恢复为 HeaderMap；先完成整表校验，避免部分草稿被写入后才发现非法字段。
fn headerMap(fields: &[HeaderField]) -> Result<HeaderMap, MessageDraftError> {
    if fields.len() > maximumEditableHeaders {
        return Err(MessageDraftError::TooManyHeaders);
    }
    let mut headers = HeaderMap::new();
    for field in fields {
        let name = field
            .name
            .parse::<HeaderName>()
            .map_err(|_| MessageDraftError::InvalidHeaderName)?;
        let value = HeaderValue::from_str(&field.value)
            .map_err(|_| MessageDraftError::InvalidHeaderValue)?;
        headers.append(name, value);
    }
    Ok(headers)
}

/// 解码并限制正文草稿长度，控制面输入不会绕过 capture 侧的单正文内存上限。
fn decodeBody(encoded: &str) -> Result<Vec<u8>, MessageDraftError> {
    if encoded.len() > maximumEditableBodyCharacters {
        return Err(MessageDraftError::BodyTooLarge);
    }
    let body = STANDARD
        .decode(encoded)
        .map_err(|_| MessageDraftError::InvalidBodyBase64)?;
    if body.len() > maximumEditableBodyBytes {
        return Err(MessageDraftError::BodyTooLarge);
    }
    Ok(body)
}

/// 编码可选正文；None 与空正文均导出为空 base64，使前端草稿形状保持固定。
fn encodeBody(body: Option<&[u8]>) -> String {
    STANDARD.encode(body.unwrap_or_default())
}

/// 解析仅允许 HTTP/HTTPS absolute-form 的请求 URL，防止断点草稿把数据面带离代理支持的传输边界。
fn parseAbsoluteUri(value: &str) -> Result<Uri, MessageDraftError> {
    let uri = value
        .parse::<Uri>()
        .map_err(|_| MessageDraftError::InvalidRequestUrl)?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.authority().is_none()
        || !uri.path().starts_with('/')
    {
        return Err(MessageDraftError::InvalidRequestUrl);
    }
    Ok(uri)
}

/// 根据最终请求 URI 重建 Location 与 Host，保证断点 URL 编辑和后续出站目标使用同一 authority。
fn refreshRequestLocation(context: &mut PipelineContext) -> Result<(), MessageDraftError> {
    let target =
        parsePipelineTarget(&context.request).map_err(|_| MessageDraftError::InvalidRequestUrl)?;
    context.location = target.location;
    context.request.headers.insert(HOST, target.hostHeader);
    Ok(())
}

/// 判断可选文本是否包含实际 reason 内容；空字符串兼容 UI 的未填写状态，不作为不可落线字段处理。
fn nonEmpty(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|value| !value.is_empty())
}

/// 校验请求和响应草稿共用的头字段与正文编码边界，保证 continue 操作失败时不触及挂起连接。
fn validateSharedDraft(draft: &EditableHttpMessage) -> Result<(), MessageDraftError> {
    let _ = headerMap(&draft.headers)?;
    let _ = decodeBody(&draft.bodyBase64)?;
    Ok(())
}
