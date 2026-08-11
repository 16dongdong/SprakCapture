//! 提供从 RecordingSession 构造 HAR 1.2 文档的导出模型，并支持全量或选中事务正文。
use std::collections::BTreeSet;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    BodyHandleMeta, BodyResponse, CaptureError, HeaderField, MessageSide, RecordingSession,
    TransactionDetailRecord, TransactionProtocol, TransactionStatus, TransactionSummary,
    TransactionTimings,
};

const harVersion: &str = "1.2";
const creatorName: &str = "Sprak Capture";
const defaultMimeType: &str = "application/octet-stream";
const defaultHttpVersion: &str = "HTTP/1.1";
const unknownTiming: f64 = -1.0;

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarExportRequest {
    pub includeBodies: bool,
    pub transactionIds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarArchive {
    pub log: HarLog,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarLog {
    pub version: String,
    pub creator: HarCreator,
    pub entries: Vec<HarEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarCreator {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarEntry {
    #[serde(rename = "startedDateTime")]
    pub startedDateTime: String,
    pub time: f64,
    pub request: HarRequest,
    pub response: HarResponse,
    pub cache: HarCache,
    pub timings: HarTimings,
    #[serde(rename = "_capture")]
    pub capture: HarCaptureExtension,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarRequest {
    pub method: String,
    pub url: String,
    #[serde(rename = "httpVersion")]
    pub httpVersion: String,
    pub cookies: Vec<HarCookie>,
    pub headers: Vec<HarNameValue>,
    #[serde(rename = "queryString")]
    pub queryString: Vec<HarNameValue>,
    #[serde(rename = "headersSize")]
    pub headersSize: i64,
    #[serde(rename = "bodySize")]
    pub bodySize: i64,
    #[serde(rename = "postData", skip_serializing_if = "Option::is_none")]
    pub postData: Option<HarPostData>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarResponse {
    pub status: i32,
    #[serde(rename = "statusText")]
    pub statusText: String,
    #[serde(rename = "httpVersion")]
    pub httpVersion: String,
    pub cookies: Vec<HarCookie>,
    pub headers: Vec<HarNameValue>,
    pub content: HarContent,
    #[serde(rename = "redirectURL")]
    pub redirectUrl: String,
    #[serde(rename = "headersSize")]
    pub headersSize: i64,
    #[serde(rename = "bodySize")]
    pub bodySize: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarPostData {
    #[serde(rename = "mimeType")]
    pub mimeType: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarContent {
    pub size: i64,
    #[serde(rename = "mimeType")]
    pub mimeType: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarNameValue {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarCookie {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarCache {}

#[derive(Clone, Debug, PartialEq, Serialize)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarTimings {
    pub blocked: f64,
    pub dns: f64,
    pub connect: f64,
    pub send: f64,
    pub wait: f64,
    pub receive: f64,
    pub ssl: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarCaptureExtension {
    pub transactionId: String,
    pub recordingSessionId: String,
    pub sequence: u64,
    pub protocol: TransactionProtocol,
    pub status: TransactionStatus,
    pub flags: crate::TransactionFlags,
    pub notes: String,
    pub tags: Vec<String>,
    pub appliedTools: Vec<String>,
    pub requestBody: Option<HarBodyMetadata>,
    pub responseBody: Option<HarBodyMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub struct HarBodyMetadata {
    pub contentType: String,
    pub encoding: String,
    pub storedBytes: usize,
    pub originalBytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Error)]
/// 描述 HAR 1.2 导出模型字段；结构保持标准字段名称并允许附带事务扩展信息。
pub enum HarExportError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
}

impl HarExportError {
    /// 返回可由控制 API、日志和测试稳定识别的错误码。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Capture(error) => error.code(),
        }
    }

    /// 返回由外层语言包渲染的稳定消息键，不在导出核心硬编码界面文本。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::Capture(error) => error.messageKey(),
        }
    }
}

impl RecordingSession {
    /// 按选择范围构造 HAR 1.2 快照；读取失败时返回 capture 层的结构化错误。
    pub async fn buildHarExport(
        &self,
        request: HarExportRequest,
    ) -> Result<HarArchive, HarExportError> {
        buildHarExport(self, &request).await
    }
}

/// 按选择范围构造 HAR 1.2 快照；读取失败时返回 capture 层的结构化错误。
pub async fn buildHarExport(
    session: &RecordingSession,
    request: &HarExportRequest,
) -> Result<HarArchive, HarExportError> {
    let selectedIds = request
        .transactionIds
        .iter()
        .filter(|transactionId| !transactionId.is_empty())
        .cloned()
        .collect::<BTreeSet<_>>();
    let exportAll = selectedIds.is_empty();
    let summaries = session.listMetadata().await?;
    let mut entries = Vec::with_capacity(summaries.len());
    for summary in summaries
        .iter()
        .filter(|summary| exportAll || selectedIds.contains(&summary.transactionId))
    {
        let detail = session.getTransactionDetail(&summary.transactionId).await?;
        entries.push(buildHarEntry(session, detail, request.includeBodies).await?);
    }
    Ok(HarArchive {
        log: HarLog {
            version: harVersion.to_owned(),
            creator: HarCreator {
                name: creatorName.to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            entries,
        },
    })
}

/// 将一个捕获事务及其可选正文转换为完整 HAR 条目，保持头部顺序和事务扩展字段。
async fn buildHarEntry(
    session: &RecordingSession,
    detail: TransactionDetailRecord,
    includeBodies: bool,
) -> Result<HarEntry, HarExportError> {
    let summary = &detail.transaction;
    let requestBody = loadBody(
        session,
        summary,
        MessageSide::Request,
        detail.requestBody.as_ref(),
        includeBodies,
    )
    .await?;
    let responseBody = loadBody(
        session,
        summary,
        MessageSide::Response,
        detail.responseBody.as_ref(),
        includeBodies,
    )
    .await?;
    let responseMimeType = responseBody
        .as_ref()
        .map(|body| body.mimeType.clone())
        .unwrap_or_else(|| normalizedMimeType(&summary.contentType));
    let requestBodySize = asHarSize(summary.sizes.requestBodyBytes);
    let responseBodySize = asHarSize(summary.sizes.responseBodyBytes);
    Ok(HarEntry {
        startedDateTime: formatHarTimestamp(summary.timings.startAtMilliseconds),
        time: totalTimeMilliseconds(&summary.timings),
        request: HarRequest {
            method: summary.method.clone(),
            url: harUrl(summary),
            httpVersion: defaultHttpVersion.to_owned(),
            cookies: Vec::new(),
            headers: harHeaders(&detail.requestHeaders),
            queryString: harQueryString(&summary.query),
            headersSize: asHarSize(summary.sizes.requestHeaderBytes),
            bodySize: requestBodySize,
            postData: requestBody.as_ref().and_then(|body| body.postData()),
        },
        response: HarResponse {
            status: i32::from(summary.statusCode.unwrap_or(0)),
            statusText: harStatusText(summary.status),
            httpVersion: defaultHttpVersion.to_owned(),
            cookies: Vec::new(),
            headers: harHeaders(&detail.responseHeaders),
            content: responseBody
                .as_ref()
                .map(|body| body.content())
                .unwrap_or_else(|| HarContent {
                    size: responseBodySize,
                    mimeType: responseMimeType,
                    text: None,
                    encoding: None,
                    compression: None,
                }),
            redirectUrl: headerValue(&detail.responseHeaders, "location").unwrap_or_default(),
            headersSize: asHarSize(summary.sizes.responseHeaderBytes),
            bodySize: responseBodySize,
        },
        cache: HarCache::default(),
        timings: harTimings(&summary.timings),
        capture: HarCaptureExtension {
            transactionId: summary.transactionId.clone(),
            recordingSessionId: summary.recordingSessionId.clone(),
            sequence: summary.sequence,
            protocol: summary.protocol,
            status: summary.status,
            flags: summary.flags.clone(),
            notes: summary.notes.clone(),
            tags: summary.tags.clone(),
            appliedTools: summary.appliedTools.clone(),
            requestBody: detail.requestBody.as_ref().map(harBodyMetadata),
            responseBody: detail.responseBody.as_ref().map(harBodyMetadata),
        },
    })
}

/// 复制正文元信息到扩展字段，保留截断边界但不泄漏存储路径。
fn harBodyMetadata(metadata: &BodyHandleMeta) -> HarBodyMetadata {
    HarBodyMetadata {
        contentType: metadata.contentType.clone(),
        encoding: metadata.encoding.clone(),
        storedBytes: metadata.storedBytes,
        originalBytes: metadata.originalBytes,
        truncated: metadata.truncated,
    }
}

/// 根据导出开关按需读取指定侧正文；未记录正文保持为空且不触发磁盘读取。
async fn loadBody(
    session: &RecordingSession,
    summary: &TransactionSummary,
    side: MessageSide,
    metadata: Option<&BodyHandleMeta>,
    includeBodies: bool,
) -> Result<Option<EncodedBody>, HarExportError> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    let response = if includeBodies {
        Some(session.getBody(&summary.transactionId, side).await?)
    } else {
        None
    };
    Ok(Some(EncodedBody::fromMetadata(metadata, response)))
}

#[derive(Clone, Debug)]
struct EncodedBody {
    metadata: BodyHandleMeta,
    mimeType: String,
    text: Option<String>,
    encoding: Option<String>,
}

impl EncodedBody {
    /// 将正文元信息和可选字节转换为 HAR 内容表示，二进制正文使用 base64。
    fn fromMetadata(metadata: &BodyHandleMeta, response: Option<BodyResponse>) -> Self {
        let mimeType = normalizedMimeType(&metadata.contentType);
        let (text, encoding) = response.map_or((None, None), |response| {
            encodeBody(&mimeType, &response.bytes)
        });
        Self {
            metadata: metadata.clone(),
            mimeType,
            text,
            encoding,
        }
    }

    /// 构造请求正文的 HAR 表示；未包含正文时返回空值。
    fn postData(&self) -> Option<HarPostData> {
        self.text.clone().map(|text| HarPostData {
            mimeType: self.mimeType.clone(),
            text,
            encoding: self.encoding.clone(),
        })
    }

    /// 构造响应正文的 HAR 内容对象，长度始终使用原始线上字节数。
    fn content(&self) -> HarContent {
        HarContent {
            size: asHarSize(self.metadata.originalBytes),
            mimeType: self.mimeType.clone(),
            text: self.text.clone(),
            encoding: self.encoding.clone(),
            compression: None,
        }
    }
}

/// 按 MIME 和 UTF-8 有效性选择文本或 base64 编码，避免损坏二进制正文。
fn encodeBody(mimeType: &str, bytes: &[u8]) -> (Option<String>, Option<String>) {
    if isTextMimeType(mimeType)
        && let Ok(text) = String::from_utf8(bytes.to_vec())
    {
        return (Some(text), None);
    }
    (Some(STANDARD.encode(bytes)), Some("base64".to_owned()))
}

/// 判断 MIME 类型是否适合直接作为 UTF-8 文本导出。
fn isTextMimeType(mimeType: &str) -> bool {
    let mimeType = mimeType
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    mimeType.starts_with("text/")
        || matches!(
            mimeType.as_str(),
            "application/json"
                | "application/javascript"
                | "application/xml"
                | "application/x-www-form-urlencoded"
        )
        || mimeType.ends_with("+json")
        || mimeType.ends_with("+xml")
}

/// 规范化缺失内容类型为二进制默认值，保证 HAR 字段始终有效。
fn normalizedMimeType(contentType: &str) -> String {
    let contentType = contentType.trim();
    if contentType.is_empty() {
        defaultMimeType.to_owned()
    } else {
        contentType.to_owned()
    }
}

/// 保留捕获头部的重复项和原始顺序，转换为 HAR 名值数组。
fn harHeaders(headers: &[HeaderField]) -> Vec<HarNameValue> {
    headers
        .iter()
        .map(|header| HarNameValue {
            name: header.name.clone(),
            value: header.value.clone(),
        })
        .collect()
}

/// 拆分原始查询串为 HAR 参数数组，不改变编码与参数排序。
fn harQueryString(query: &str) -> Vec<HarNameValue> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            HarNameValue {
                name: name.to_owned(),
                value: value.to_owned(),
            }
        })
        .collect()
}

/// 以不区分大小写的方式读取首个指定响应头，用于 HAR 重定向字段。
fn headerValue(headers: &[HeaderField], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
}

/// 构造 HAR 所需完整 URL；异常协议记录也获得可读且确定的地址。
fn harUrl(summary: &TransactionSummary) -> String {
    if summary.urlDisplay.starts_with("http://") || summary.urlDisplay.starts_with("https://") {
        return summary.urlDisplay.clone();
    }
    let scheme = match summary.protocol {
        TransactionProtocol::Http | TransactionProtocol::Ws | TransactionProtocol::Socks => "http",
        TransactionProtocol::Https | TransactionProtocol::Wss | TransactionProtocol::Tunnel => {
            "https"
        }
    };
    let host = if summary.host.contains(':') {
        format!("[{}]", summary.host)
    } else {
        summary.host.clone()
    };
    let path = if summary.path.is_empty() {
        "/"
    } else {
        &summary.path
    };
    let authority =
        if (scheme == "http" && summary.port == 80) || (scheme == "https" && summary.port == 443) {
            host
        } else {
            format!("{host}:{}", summary.port)
        };
    if summary.query.is_empty() {
        format!("{scheme}://{authority}{path}")
    } else {
        format!("{scheme}://{authority}{path}?{}", summary.query)
    }
}

/// 将内部事务终态映射为 HAR 状态文本，保持未完成事务的可观察性。
fn harStatusText(status: TransactionStatus) -> String {
    match status {
        TransactionStatus::Pending => "Pending",
        TransactionStatus::Complete => "",
        TransactionStatus::Failed => "Failed",
        TransactionStatus::Blocked => "Blocked",
        TransactionStatus::Cancelled => "Cancelled",
    }
    .to_owned()
}

/// 将无符号捕获大小安全收敛到 HAR 使用的有符号整数范围。
fn asHarSize(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// 计算事务从开始到结束的总耗时；未完成事务返回零而非伪造时间。
fn totalTimeMilliseconds(timings: &TransactionTimings) -> f64 {
    timings
        .endAtMilliseconds
        .map(|endAtMilliseconds| {
            endAtMilliseconds.saturating_sub(timings.startAtMilliseconds) as f64
        })
        .unwrap_or(0.0)
}

/// 由捕获时间点生成 HAR 阶段耗时，未知阶段严格使用负一。
fn harTimings(timings: &TransactionTimings) -> HarTimings {
    let requestStart = timings
        .tlsEndAtMilliseconds
        .or(timings.connectEndAtMilliseconds)
        .or(timings.dnsEndAtMilliseconds)
        .unwrap_or(timings.startAtMilliseconds);
    HarTimings {
        blocked: unknownTiming,
        dns: elapsedMilliseconds(
            Some(timings.startAtMilliseconds),
            timings.dnsEndAtMilliseconds,
        ),
        connect: elapsedMilliseconds(
            timings
                .dnsEndAtMilliseconds
                .or(Some(timings.startAtMilliseconds)),
            timings.connectEndAtMilliseconds,
        ),
        send: elapsedMilliseconds(Some(requestStart), timings.requestSentAtMilliseconds),
        wait: elapsedMilliseconds(
            timings.requestSentAtMilliseconds,
            timings.responseStartAtMilliseconds,
        ),
        receive: elapsedMilliseconds(
            timings.responseStartAtMilliseconds,
            timings.endAtMilliseconds,
        ),
        ssl: elapsedMilliseconds(
            timings.connectEndAtMilliseconds,
            timings.tlsEndAtMilliseconds,
        ),
    }
}

/// 计算两个可选时间点之间的非负毫秒差，缺失时间点返回 HAR 未知值。
fn elapsedMilliseconds(start: Option<u64>, end: Option<u64>) -> f64 {
    match (start, end) {
        (Some(start), Some(end)) => end.saturating_sub(start) as f64,
        _ => unknownTiming,
    }
}

/// 将 Unix 毫秒格式化为 RFC3339 时间戳，异常时间回退到固定 epoch。
fn formatHarTimestamp(milliseconds: u64) -> String {
    let seconds = (milliseconds / 1_000).min(i64::MAX as u64) as i64;
    let nanoseconds = ((milliseconds % 1_000) * 1_000_000) as u32;
    let timestamp = OffsetDateTime::from_unix_timestamp(seconds)
        .and_then(|timestamp| timestamp.replace_nanosecond(nanoseconds))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    timestamp
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}
