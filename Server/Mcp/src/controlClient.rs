use std::{collections::BTreeMap, fmt, net::IpAddr, time::Duration};

use reqwest::{
    Client, Method, Response, StatusCode, Url,
    header::{ACCEPT_LANGUAGE, CONTENT_LENGTH, CONTENT_TYPE},
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const requestTimeout: Duration = Duration::from_secs(35);
const connectTimeout: Duration = Duration::from_secs(3);
// 允许现有一万条会话历史的权威快照，同时把异常控制响应的单次内存占用限制在 16 MiB。
const maximumResponseBytes: usize = 16 * 1024 * 1024;

/// 保存控制面允许透传的唯一稳定错误结构；未知或缺失字段一律视为无效响应。
#[derive(Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredControlError {
    pub code: String,
    pub message: String,
    pub messageKey: String,
    pub params: BTreeMap<String, String>,
}

/// 保存无效控制响应的有界元数据；正文始终留在适配器内部且不会进入 tool 结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponseMetadata {
    pub statusCode: u16,
    pub contentType: Option<String>,
    pub contentLength: Option<u64>,
    pub contentDigest: Option<String>,
}

/// 保存根证书下载的有界公开字节和媒体类型；私钥端点不存在于控制契约。
pub struct BinaryControlResponse {
    pub bytes: Vec<u8>,
    pub contentType: Option<String>,
}

/// 描述控制 API 调用失败的稳定类别；本类型只保存诊断，由 MCP 层按 locale 渲染。
pub enum ControlFailure {
    Unavailable,
    Rejected {
        statusCode: u16,
        error: StructuredControlError,
    },
    InvalidResponse {
        metadata: Option<ResponseMetadata>,
    },
}

impl fmt::Debug for ControlFailure {
    /// 为测试断言和错误传播提供不含正文的诊断；只公开稳定失败类别及 HTTP 状态码。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("ControlFailure::Unavailable"),
            Self::Rejected { statusCode, .. } => formatter
                .debug_struct("ControlFailure::Rejected")
                .field("statusCode", statusCode)
                .finish(),
            Self::InvalidResponse { .. } => formatter.write_str("ControlFailure::InvalidResponse"),
        }
    }
}

/// 封装本机 HTTP 控制契约；不实现业务权限，仅负责有界传输和响应解码。
#[derive(Clone)]
pub struct ControlClient {
    httpClient: Client,
    controlBase: Url,
}

impl ControlClient {
    /// 校验控制基址并创建有界 HTTP 客户端；失败返回启动诊断且不会启动 stdio 服务。
    pub fn new(controlBase: &str) -> Result<Self, String> {
        let normalizedBase = format!("{}/", controlBase.trim().trim_end_matches('/'));
        let controlBase = Url::parse(&normalizedBase).map_err(|_| "控制地址格式无效".to_owned())?;
        if controlBase.scheme() != "http" {
            return Err("控制地址只支持 HTTP".to_owned());
        }
        if controlBase.cannot_be_a_base() {
            return Err("控制地址不能作为 URL 基址".to_owned());
        }
        if !controlBase.username().is_empty() || controlBase.password().is_some() {
            return Err("控制地址禁止携带用户信息".to_owned());
        }
        if controlBase.query().is_some() || controlBase.fragment().is_some() {
            return Err("控制地址禁止携带查询参数或片段".to_owned());
        }
        let loopbackHost = controlBase.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if !loopbackHost {
            return Err("控制地址必须使用本机回环主机".to_owned());
        }
        let httpClient = Client::builder()
            .connect_timeout(connectTimeout)
            .timeout(requestTimeout)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            httpClient,
            controlBase,
        })
    }

    /// 调用一个现有控制路由并有界解码 JSON；非成功状态只接收结构化允许字段。
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        locale: &str,
    ) -> Result<Value, ControlFailure> {
        let endpoint = self
            .controlBase
            .join(path)
            .map_err(|_| ControlFailure::InvalidResponse { metadata: None })?;
        let mut request = self
            .httpClient
            .request(method, endpoint)
            .header(ACCEPT_LANGUAGE, locale);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| ControlFailure::Unavailable)?;
        decodeResponse(response).await
    }

    /// 调用成功无正文的控制动作；仅接受 204，避免 MCP 将意外 HTML/JSON 成功页误当作断点已释放。
    pub async fn requestNoContent(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        locale: &str,
    ) -> Result<(), ControlFailure> {
        let endpoint = self
            .controlBase
            .join(path)
            .map_err(|_| ControlFailure::InvalidResponse { metadata: None })?;
        let mut request = self
            .httpClient
            .request(method, endpoint)
            .header(ACCEPT_LANGUAGE, locale);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| ControlFailure::Unavailable)?;
        let (status, contentType, contentLength, responseBytes) =
            readBoundedResponse(response).await?;
        if status == StatusCode::NO_CONTENT && responseBytes.is_empty() {
            return Ok(());
        }
        if status.is_success() {
            return Err(ControlFailure::InvalidResponse {
                metadata: Some(responseMetadata(
                    status,
                    contentType,
                    contentLength,
                    &responseBytes,
                    true,
                )),
            });
        }
        match decodeResponseBody(status, contentType, contentLength, &responseBytes) {
            Err(error) => Err(error),
            Ok(_) => Err(ControlFailure::InvalidResponse { metadata: None }),
        }
    }

    /// 调用公开二进制下载路由并复用相同大小、超时和结构化错误边界。
    pub async fn requestBytes(
        &self,
        method: Method,
        path: &str,
        locale: &str,
    ) -> Result<BinaryControlResponse, ControlFailure> {
        self.requestBytesWithBody(method, path, None, locale).await
    }

    /// 调用需要 JSON 请求体的公开二进制下载路由；HAR 导出与证书下载共享有界读取和错误解码边界。
    pub async fn requestBytesWithBody(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        locale: &str,
    ) -> Result<BinaryControlResponse, ControlFailure> {
        let endpoint = self
            .controlBase
            .join(path)
            .map_err(|_| ControlFailure::InvalidResponse { metadata: None })?;
        let mut request = self
            .httpClient
            .request(method, endpoint)
            .header(ACCEPT_LANGUAGE, locale);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| ControlFailure::Unavailable)?;
        let (status, contentType, contentLength, responseBytes) =
            readBoundedResponse(response).await?;
        if status.is_success() {
            return Ok(BinaryControlResponse {
                bytes: responseBytes,
                contentType,
            });
        }
        match decodeResponseBody(status, contentType, contentLength, &responseBytes) {
            Err(error) => Err(error),
            Ok(_) => Err(ControlFailure::InvalidResponse { metadata: None }),
        }
    }
}

/// 读取至多 maximumResponseBytes 的控制响应；越界立即停止，不让错误页占满 MCP 进程内存。
async fn decodeResponse(response: Response) -> Result<Value, ControlFailure> {
    let (status, contentType, contentLength, responseBytes) = readBoundedResponse(response).await?;
    decodeResponseBody(status, contentType, contentLength, &responseBytes)
}

/// 将任意控制响应读取为有界字节；JSON 与证书下载共享完全相同的资源和诊断语义。
async fn readBoundedResponse(
    mut response: Response,
) -> Result<(StatusCode, Option<String>, Option<u64>, Vec<u8>), ControlFailure> {
    let status = response.status();
    let declaredLength = response.content_length().or_else(|| {
        response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
    });
    let contentType = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if declaredLength.is_some_and(|length| length > maximumResponseBytes as u64) {
        return Err(ControlFailure::InvalidResponse {
            metadata: Some(ResponseMetadata {
                statusCode: status.as_u16(),
                contentType,
                contentLength: declaredLength,
                contentDigest: None,
            }),
        });
    }

    let mut responseBytes = Vec::with_capacity(
        declaredLength
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or(0)
            .min(maximumResponseBytes),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ControlFailure::InvalidResponse {
            metadata: Some(responseMetadata(
                status,
                contentType.clone(),
                declaredLength,
                &responseBytes,
                false,
            )),
        })?
    {
        if responseBytes.len().saturating_add(chunk.len()) > maximumResponseBytes {
            let remainingBytes = maximumResponseBytes.saturating_sub(responseBytes.len());
            responseBytes.extend_from_slice(&chunk[..remainingBytes]);
            return Err(ControlFailure::InvalidResponse {
                metadata: Some(responseMetadata(
                    status,
                    contentType,
                    declaredLength,
                    &responseBytes,
                    false,
                )),
            });
        }
        responseBytes.extend_from_slice(&chunk);
    }
    Ok((
        status,
        contentType,
        declaredLength.or(Some(responseBytes.len() as u64)),
        responseBytes,
    ))
}

/// 按 HTTP 状态解码已受限的 JSON；失败状态只接受后端声明的结构化错误字段。
fn decodeResponseBody(
    status: StatusCode,
    contentType: Option<String>,
    contentLength: Option<u64>,
    responseBytes: &[u8],
) -> Result<Value, ControlFailure> {
    if status.is_success() {
        return serde_json::from_slice::<Value>(responseBytes).map_err(|_| {
            ControlFailure::InvalidResponse {
                metadata: Some(responseMetadata(
                    status,
                    contentType,
                    contentLength,
                    responseBytes,
                    true,
                )),
            }
        });
    }
    serde_json::from_slice::<StructuredControlError>(responseBytes)
        .map_err(|_| ControlFailure::InvalidResponse {
            metadata: Some(responseMetadata(
                status,
                contentType,
                contentLength,
                responseBytes,
                true,
            )),
        })
        .and_then(|error| {
            Err(ControlFailure::Rejected {
                statusCode: status.as_u16(),
                error,
            })
        })
}

/// 从已读取的有界正文生成可公开诊断；摘要用于关联响应，不泄露正文内容。
fn responseMetadata(
    status: StatusCode,
    contentType: Option<String>,
    contentLength: Option<u64>,
    responseBytes: &[u8],
    digestComplete: bool,
) -> ResponseMetadata {
    ResponseMetadata {
        statusCode: status.as_u16(),
        contentType,
        contentLength,
        contentDigest: digestComplete.then(|| digestBytes(responseBytes)),
    }
}

/// 计算固定小写十六进制 SHA-256，避免引入只为编码摘要使用的额外依赖。
fn digestBytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 返回 reqwest 方法常量，避免工具模块依赖 reqwest 的完整导入表。
pub fn getMethod() -> Method {
    Method::GET
}

/// 返回 POST 方法常量。
pub fn postMethod() -> Method {
    Method::POST
}

/// 返回 PUT 方法常量。
pub fn putMethod() -> Method {
    Method::PUT
}

/// 返回 DELETE 方法常量。
pub fn deleteMethod() -> Method {
    Method::DELETE
}
