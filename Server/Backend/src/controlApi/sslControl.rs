use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Multipart, Path, Query, State,
        multipart::MultipartRejection,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{
        HeaderValue,
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use http_proxy_core::{
    ClientCertificateFormat, ClientCertificateImport, ClientCertificateUpdate,
    SslMitmConfiguration, SslMitmError, SslPublicState,
};
use location_core::LocationPattern;
use serde::Deserialize;

const maximumClientCertificateUploadBytes: usize = 12 * 1024 * 1024;
const maximumClientCertificateFieldBytes: usize = 8 * 1024 * 1024;
const maximumClientCertificateNameBytes: usize = 320;
const maximumClientCertificatePasswordBytes: usize = 4 * 1024;
const maximumClientCertificateLocationsBytes: usize = 64 * 1024;

use super::{ApiError, ControlState, ErrorCode, EventMessage, LocalizedApiError};
use crate::localization::RequestLocale;

/// 约束根证书下载格式；PEM 便于命令行查看，CER 返回标准二进制 DER。
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RootCertificateFormat {
    Pem,
    Cer,
}

/// 解析根证书下载查询；format 必填以避免客户端依据内容猜测扩展名。
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootCertificateQuery {
    format: RootCertificateFormat,
}

impl ControlState {
    /// 返回当前 SSL 配置、公开根证书元数据与握手累计值，不增加全局 revision。
    fn sslState(&self) -> SslPublicState {
        self.ssl.publicState()
    }

    /// 校验、持久化并实时替换 SSL 主机表；现有 TLS 会话保持原配置，新 CONNECT 立即使用新规则。
    fn updateSsl(&self, configuration: SslMitmConfiguration) -> Result<SslPublicState, ApiError> {
        configuration.validate().map_err(mapSslOperationError)?;
        // 配置文件先于共享快照提交；校验已完成，后续替换只执行无 IO 的原子内存发布。
        self.processSelection
            .replaceSslConfiguration(configuration.clone())
            .map_err(|_| ApiError::internal(ErrorCode::ConfigurationPersistenceFailed))?;
        let ssl = self
            .ssl
            .updateConfiguration(configuration)
            .map_err(mapSslOperationError)?;
        let eventState = ssl.clone();
        self.publishProjectionRevisioned(|serverInstanceId, revision| EventMessage::Ssl {
            serverInstanceId,
            revision,
            ssl: eventState,
        });
        Ok(ssl)
    }

    /// 更换根 CA 并发布新指纹；私钥只在证书管理器内存与用户证书目录中存在。
    fn regenerateSslRoot(&self) -> Result<SslPublicState, ApiError> {
        let ssl = self.ssl.regenerateRoot().map_err(mapSslOperationError)?;
        let eventState = ssl.clone();
        self.publishProjectionRevisioned(|serverInstanceId, revision| EventMessage::Ssl {
            serverInstanceId,
            revision,
            ssl: eventState,
        });
        Ok(ssl)
    }

    /// 导入一条上游 mTLS 身份并发布完整公开状态；输入口令和私钥不进入事件。
    fn importClientCertificate(
        &self,
        input: ClientCertificateImport,
    ) -> Result<SslPublicState, ApiError> {
        let ssl = self
            .ssl
            .importClientCertificate(input)
            .map_err(mapSslOperationError)?;
        self.publishSslState(ssl)
    }

    /// 更新客户端身份的规则和启用状态；ID 与密钥材料保持不变。
    fn updateClientCertificate(
        &self,
        id: &str,
        update: ClientCertificateUpdate,
    ) -> Result<SslPublicState, ApiError> {
        let ssl = self
            .ssl
            .updateClientCertificate(id, update)
            .map_err(mapSslOperationError)?;
        self.publishSslState(ssl)
    }

    /// 删除客户端身份并发布结果；删除操作不会返回任何已删除材料。
    fn removeClientCertificate(&self, id: &str) -> Result<SslPublicState, ApiError> {
        let ssl = self
            .ssl
            .removeClientCertificate(id)
            .map_err(mapSslOperationError)?;
        self.publishSslState(ssl)
    }

    /// 统一发布 SSL 状态事件，避免各证书操作复制 revision 逻辑。
    fn publishSslState(&self, ssl: SslPublicState) -> Result<SslPublicState, ApiError> {
        let eventState = ssl.clone();
        self.publishProjectionRevisioned(|serverInstanceId, revision| EventMessage::Ssl {
            serverInstanceId,
            revision,
            ssl: eventState,
        });
        Ok(ssl)
    }
}

/// 将证书与规则错误映射为稳定控制码；底层路径、解析细节和密钥信息不得进入参数。
fn mapSslOperationError(error: SslMitmError) -> ApiError {
    match error {
        SslMitmError::InvalidLocation
        | SslMitmError::InvalidCacheLimit
        | SslMitmError::InvalidClientCertificate
        | SslMitmError::DuplicateClientCertificate
        | SslMitmError::ClientCertificateLimit => {
            ApiError::badRequest(ErrorCode::InvalidSslConfiguration)
        }
        SslMitmError::ClientCertificateNotFound => {
            ApiError::notFound(ErrorCode::InvalidSslConfiguration)
        }
        _ => ApiError::internal(ErrorCode::SslOperationFailed).withParam("sslCode", error.code()),
    }
}

/// 将 M2 路由集中附加到现有控制 Router；所有处理器仍共享同一个 ControlState。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router
        .route("/api/v1/ssl", get(getSsl).put(updateSsl))
        .route("/api/v1/ssl/ca/generate", post(regenerateSslRoot))
        .route("/api/v1/ssl/ca/export", get(exportSslRoot))
        .route(
            "/api/v1/ssl/client-certificates",
            post(importClientCertificate)
                .layer(DefaultBodyLimit::max(maximumClientCertificateUploadBytes)),
        )
        .route(
            "/api/v1/ssl/client-certificates/{id}",
            put(updateClientCertificate).delete(removeClientCertificate),
        )
}

/// 保存 multipart 已收集的客户端身份字段；缺失字段在解析完成后统一拒绝。
#[derive(Default)]
struct ClientCertificateFields {
    name: Option<String>,
    format: Option<ClientCertificateFormat>,
    enabled: Option<bool>,
    locations: Option<Vec<LocationPattern>>,
    certificateBytes: Option<Vec<u8>>,
    keyBytes: Option<Vec<u8>>,
    password: String,
}

impl ClientCertificateFields {
    /// 转换为核心导入请求；必填字段缺失返回同一稳定请求错误。
    fn intoImport(self) -> Result<ClientCertificateImport, ApiError> {
        Ok(ClientCertificateImport {
            name: self.name.ok_or_else(invalidClientCertificateRequest)?,
            format: self.format.ok_or_else(invalidClientCertificateRequest)?,
            enabled: self.enabled.unwrap_or(true),
            locations: self.locations.ok_or_else(invalidClientCertificateRequest)?,
            certificateBytes: self
                .certificateBytes
                .ok_or_else(invalidClientCertificateRequest)?,
            keyBytes: self.keyBytes,
            password: self.password,
        })
    }
}

/// 读取有界客户端证书 multipart；字段名固定，未知字段直接拒绝协议漂移。
async fn receiveClientCertificate(
    mut multipart: Multipart,
) -> Result<ClientCertificateImport, ApiError> {
    let mut fields = ClientCertificateFields::default();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| invalidClientCertificateRequest())?
    {
        let name = field
            .name()
            .ok_or_else(invalidClientCertificateRequest)?
            .to_owned();
        let bytes = field
            .bytes()
            .await
            .map_err(|_| invalidClientCertificateRequest())?;
        if bytes.len() > maximumClientCertificateFieldBytes {
            return Err(invalidClientCertificateRequest());
        }
        match name.as_str() {
            "name" => {
                fields.name = Some(parseLimitedUtf8(
                    bytes.as_ref(),
                    maximumClientCertificateNameBytes,
                )?)
            }
            "format" if bytes.len() <= 32 => fields.format = Some(parseJson(bytes.as_ref())?),
            "enabled" if bytes.len() <= 8 => fields.enabled = Some(parseJson(bytes.as_ref())?),
            "locations" if bytes.len() <= maximumClientCertificateLocationsBytes => {
                fields.locations = Some(parseJson(bytes.as_ref())?)
            }
            "certificate" => fields.certificateBytes = Some(bytes.to_vec()),
            "privateKey" => fields.keyBytes = Some(bytes.to_vec()),
            "password" => {
                fields.password =
                    parseLimitedUtf8(bytes.as_ref(), maximumClientCertificatePasswordBytes)?
            }
            _ => return Err(invalidClientCertificateRequest()),
        }
    }
    fields.intoImport()
}

/// 把文本字段按 UTF-8 解码；无效文本不进入证书存储或日志。
fn parseUtf8(bytes: &[u8]) -> Result<String, ApiError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| invalidClientCertificateRequest())
}

/// 在分配字符串前约束文本字段；口令、名称和规则不会占用证书文件级别的内存上限。
fn parseLimitedUtf8(bytes: &[u8], maximumBytes: usize) -> Result<String, ApiError> {
    if bytes.len() > maximumBytes {
        return Err(invalidClientCertificateRequest());
    }
    parseUtf8(bytes)
}

/// 解析 multipart 内嵌 JSON 字段；错误详情不回显原始内容。
fn parseJson<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|_| invalidClientCertificateRequest())
}

/// 构造客户端证书导入的稳定请求错误，避免重复分支泄露解析细节。
fn invalidClientCertificateRequest() -> ApiError {
    ApiError::badRequest(ErrorCode::InvalidSslConfiguration)
}

/// 返回当前 SSL 公开状态；响应不包含根私钥、密钥路径或叶证书私钥。
async fn getSsl(State(state): State<ControlState>) -> Json<SslPublicState> {
    Json(state.sslState())
}

/// 校验并实时替换 SSL 主机范围、缓存上限和 SNI 策略。
async fn updateSsl(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    updateResult: Result<Json<SslMitmConfiguration>, JsonRejection>,
) -> Result<Json<SslPublicState>, LocalizedApiError> {
    let Json(configuration) = updateResult.map_err(|error| {
        ApiError::badRequest(ErrorCode::InvalidSslRequest)
            .withParam("detail", error.body_text())
            .withLocale(locale)
    })?;
    state
        .updateSsl(configuration)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 接收 PKCS#12/PFX、PEM 或 DER 客户端身份；口令只传给同步核心解析并随请求释放。
async fn importClientCertificate(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    multipartResult: Result<Multipart, MultipartRejection>,
) -> Result<Json<SslPublicState>, LocalizedApiError> {
    let multipart =
        multipartResult.map_err(|_| invalidClientCertificateRequest().withLocale(locale))?;
    let input = receiveClientCertificate(multipart)
        .await
        .map_err(|error| error.withLocale(locale))?;
    state
        .importClientCertificate(input)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 更新一条客户端身份的匹配规则与启用状态；JSON 中不接受证书或私钥字段。
async fn updateClientCertificate(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(id): Path<String>,
    updateResult: Result<Json<ClientCertificateUpdate>, JsonRejection>,
) -> Result<Json<SslPublicState>, LocalizedApiError> {
    let Json(update) = updateResult.map_err(|error| {
        ApiError::badRequest(ErrorCode::InvalidSslRequest)
            .withParam("detail", error.body_text())
            .withLocale(locale)
    })?;
    state
        .updateClientCertificate(&id, update)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 删除一条客户端身份；ID 只作为哈希键使用，不参与文件路径拼接。
async fn removeClientCertificate(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(id): Path<String>,
) -> Result<Json<SslPublicState>, LocalizedApiError> {
    state
        .removeClientCertificate(&id)
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 原子更换根 CA 并返回新指纹；调用方应在执行前完成交互确认。
async fn regenerateSslRoot(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<SslPublicState>, LocalizedApiError> {
    state
        .regenerateSslRoot()
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 下载公开根证书；PEM 返回文本，CER 返回 DER，响应头固定附件文件名并禁止缓存。
async fn exportSslRoot(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    queryResult: Result<Query<RootCertificateQuery>, QueryRejection>,
) -> Result<Response, LocalizedApiError> {
    let Query(query) = queryResult.map_err(|error| {
        ApiError::badRequest(ErrorCode::InvalidSslRequest)
            .withParam("detail", error.body_text())
            .withLocale(locale)
    })?;
    let (bytes, contentType, fileName) = match query.format {
        RootCertificateFormat::Pem => (
            state.ssl.exportRootPem(),
            "application/x-pem-file",
            "root.pem",
        ),
        RootCertificateFormat::Cer => (
            state.ssl.exportRootDer(),
            "application/pkix-cert",
            "root.cer",
        ),
    };
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(contentType));
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{fileName}\""))
            .expect("固定 ASCII 文件名必须形成有效响应头"),
    );
    Ok(response)
}
