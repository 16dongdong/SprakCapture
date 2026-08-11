use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State, rejection::JsonRejection},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD as base64Standard};
use capture_core::{
    BodyResponse, MessageSide, TransactionProtocol, TransactionSummary, currentTimeMilliseconds,
};
use location_core::{
    LocationMatchOptions, LocationPattern, ResolvedLocation, locationMatches,
    validateLocationPattern,
};
use prost_reflect::{DescriptorPool, DynamicMessage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use super::{ApiError, ControlState, ErrorCode, LocalizedApiError};
use crate::localization::RequestLocale;

const maximumDescriptorBytes: usize = 16 * 1024 * 1024;
const maximumValidationReports: usize = 512;
const onlineValidationTimeout: Duration = Duration::from_secs(15);
const defaultW3cEndpoint: &str = "https://validator.w3.org/nu/?out=json";

/// 保存可公开的 Protobuf 描述符条目；路径相对用户数据目录，避免泄露本机绝对目录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtobufSchemaEntry {
    pub id: String,
    pub name: String,
    pub descriptorPath: String,
    pub defaultMessageType: String,
}

/// 将统一 Location 规则绑定到请求或响应消息类型；响应类型缺失时复用请求类型。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtobufRoute {
    pub id: String,
    pub location: LocationPattern,
    pub messageType: String,
    #[serde(default)]
    pub responseMessageType: Option<String>,
    pub schemaId: String,
}

/// 公开 Protobuf 解码器运行配置；描述符字节不会进入快照、事件或日志。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtobufConfiguration {
    pub enabled: bool,
    pub schemas: Vec<ProtobufSchemaEntry>,
    pub routes: Vec<ProtobufRoute>,
}

impl Default for ProtobufConfiguration {
    /// 默认关闭解码且不登记描述符，代理和录制主路径不依赖本功能。
    fn default() -> Self {
        Self {
            enabled: false,
            schemas: Vec::new(),
            routes: Vec::new(),
        }
    }
}

/// 接收配置更新；描述符只能经专用上传端点写入用户数据目录。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProtobufConfigurationUpdate {
    enabled: bool,
    routes: Vec<ProtobufRoute>,
}

/// 接收 FileDescriptorSet 上传；Base64 仅在请求体中短暂存在。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProtobufDescriptorUpload {
    name: String,
    defaultMessageType: String,
    base64: String,
}

/// 指定要解码的消息侧，固定枚举避免自由文本改变正文读取路径。
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum DecodeSide {
    Request,
    Response,
}

impl DecodeSide {
    /// 转换为录制层稳定的消息侧枚举。
    const fn messageSide(self) -> MessageSide {
        match self {
            Self::Request => MessageSide::Request,
            Self::Response => MessageSide::Response,
        }
    }
}

/// 解析 Protobuf 解码查询；缺失 side 时按响应优先，符合检查器默认查看行为。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProtobufDecodeQuery {
    side: Option<DecodeSide>,
}

/// 提供无 schema、压缩帧或字段失败时仍可回退 Hex 的稳定解码结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodedProtobufView {
    pub messageType: Option<String>,
    pub json: Option<Value>,
    pub decodeError: Option<String>,
}

/// 定义本地与在线校验器标识；在线校验必须在执行请求中再次确认上传。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidatorId {
    HtmlWellFormed,
    JsonSchema,
    W3cHtmlOnline,
}

/// 表示单个校验器开关，完整替换避免多个客户端对 enabled 推断不一致。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidatorConfiguration {
    pub id: ValidatorId,
    pub enabled: bool,
}

/// 保存 Validate 完整配置；online 开关与一次性确认共同约束外发正文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidateConfiguration {
    pub enabled: bool,
    pub validators: Vec<ValidatorConfiguration>,
    pub allowOnlineValidators: bool,
    pub w3cEndpoint: String,
}

impl Default for ValidateConfiguration {
    /// 默认只启用离线 HTML 结构校验，任何正文都不会因默认配置上传。
    fn default() -> Self {
        Self {
            enabled: true,
            validators: vec![
                ValidatorConfiguration {
                    id: ValidatorId::HtmlWellFormed,
                    enabled: true,
                },
                ValidatorConfiguration {
                    id: ValidatorId::JsonSchema,
                    enabled: false,
                },
                ValidatorConfiguration {
                    id: ValidatorId::W3cHtmlOnline,
                    enabled: false,
                },
            ],
            allowOnlineValidators: false,
            w3cEndpoint: defaultW3cEndpoint.to_owned(),
        }
    }
}

/// 汇总需要跨进程重启恢复的协议工具配置；校验报告属于会话数据，不进入持久化文件。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub(super) struct PersistedProtocolConfiguration {
    pub protobuf: ProtobufConfiguration,
    pub validate: ValidateConfiguration,
}

/// 将正文校验问题映射为检查器可本地化展示的稳定信息。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    pub severity: ValidationSeverity,
    pub messageKey: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

/// 限定校验问题严重度，避免自由文本导致检查器样式漂移。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationSeverity {
    Info,
    Warning,
    Error,
}

/// 记录一次完整校验结果；正文不写入报告，避免重复占用录制预算。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub transactionId: String,
    pub validatorId: ValidatorId,
    pub issues: Vec<ValidationIssue>,
    pub validatedAtMilliseconds: u64,
}

/// 接收单次校验器选择和在线上传确认；确认缺失时外部请求不会发生。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ValidateRequest {
    validatorId: ValidatorId,
    #[serde(default)]
    onlineUploadConfirmed: bool,
}

/// 统一管理 M6 协议查看和校验状态；锁内仅保留配置、描述符索引及有界报告。
#[derive(Clone)]
pub(super) struct ProtocolRuntime {
    descriptorDirectory: Arc<PathBuf>,
    protobuf: Arc<RwLock<ProtobufConfiguration>>,
    descriptorPools: Arc<RwLock<HashMap<String, DescriptorPool>>>,
    validate: Arc<RwLock<ValidateConfiguration>>,
    validationReports: Arc<RwLock<HashMap<String, Vec<ValidationReport>>>>,
    updateLock: Arc<Mutex<()>>,
    httpClient: reqwest::Client,
}

impl ProtocolRuntime {
    /// 创建用户数据目录下的协议状态；目录失败会阻止启动，避免上传写入未知工作目录。
    pub(super) async fn new(
        dataDirectory: &Path,
        configuration: PersistedProtocolConfiguration,
    ) -> Result<Self, std::io::Error> {
        let descriptorDirectory = dataDirectory.join("protobufDescriptors");
        tokio::fs::create_dir_all(&descriptorDirectory).await?;
        validateValidateConfiguration(&configuration.validate)
            .map_err(protocolConfigurationIoError)?;
        let descriptorPools = loadDescriptorPools(dataDirectory, &configuration.protobuf).await?;
        validateProtobufRoutes(
            &configuration.protobuf.routes,
            &configuration.protobuf.schemas,
            &descriptorPools,
        )
        .map_err(protocolConfigurationIoError)?;
        let httpClient = reqwest::Client::builder()
            .timeout(onlineValidationTimeout)
            .build()
            .expect("创建 Validate HTTP 客户端失败");
        Ok(Self {
            descriptorDirectory: Arc::new(descriptorDirectory),
            protobuf: Arc::new(RwLock::new(configuration.protobuf)),
            descriptorPools: Arc::new(RwLock::new(descriptorPools)),
            validate: Arc::new(RwLock::new(configuration.validate)),
            validationReports: Arc::new(RwLock::new(HashMap::new())),
            updateLock: Arc::new(Mutex::new(())),
            httpClient,
        })
    }

    /// 返回需要写入统一配置文件的协议工具快照；调用方用它实现写盘成功后再发布运行时状态。
    async fn persistedConfiguration(&self) -> PersistedProtocolConfiguration {
        PersistedProtocolConfiguration {
            protobuf: self.protobuf.read().await.clone(),
            validate: self.validate.read().await.clone(),
        }
    }

    /// 返回完整 Protobuf 配置快照；描述符池不复制到控制响应。
    async fn protobufConfiguration(&self) -> ProtobufConfiguration {
        self.protobuf.read().await.clone()
    }

    /// 先校验全部路由引用、Location 和消息类型，再原子替换运行配置。
    async fn replaceProtobufConfiguration(
        &self,
        update: ProtobufConfigurationUpdate,
        persist: impl FnOnce(PersistedProtocolConfiguration) -> Result<(), std::io::Error>,
    ) -> Result<ProtobufConfiguration, ProtocolError> {
        let _updateGuard = self.updateLock.lock().await;
        let current = self.protobuf.read().await;
        let descriptorPools = self.descriptorPools.read().await;
        validateProtobufRoutes(&update.routes, &current.schemas, &descriptorPools)?;
        let configuration = ProtobufConfiguration {
            enabled: update.enabled,
            schemas: current.schemas.clone(),
            routes: update.routes,
        };
        drop(descriptorPools);
        drop(current);
        let mut persisted = self.persistedConfiguration().await;
        persisted.protobuf = configuration.clone();
        persist(persisted).map_err(|_| ProtocolError::Storage)?;
        *self.protobuf.write().await = configuration.clone();
        Ok(configuration)
    }

    /// 写入并登记经 FileDescriptorSet 校验的描述符；失败不会留下半写入文件或不可用 schema。
    async fn uploadDescriptor(
        &self,
        upload: ProtobufDescriptorUpload,
        persist: impl FnOnce(PersistedProtocolConfiguration) -> Result<(), std::io::Error>,
    ) -> Result<ProtobufConfiguration, ProtocolError> {
        let _updateGuard = self.updateLock.lock().await;
        validateDescriptorUpload(&upload)?;
        let bytes = base64Standard
            .decode(upload.base64.as_bytes())
            .map_err(|_| ProtocolError::InvalidDescriptor)?;
        if bytes.is_empty() || bytes.len() > maximumDescriptorBytes {
            return Err(ProtocolError::InvalidDescriptor);
        }
        let pool = DescriptorPool::decode(bytes.as_slice())
            .map_err(|_| ProtocolError::InvalidDescriptor)?;
        if pool
            .get_message_by_name(&upload.defaultMessageType)
            .is_none()
        {
            return Err(ProtocolError::InvalidDescriptorMessageType);
        }
        let id = Uuid::new_v4().to_string();
        let fileName = format!("{id}.desc");
        let destination = self.descriptorDirectory.join(&fileName);
        let temporary = self.descriptorDirectory.join(format!(".{id}.pending"));
        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|_| ProtocolError::Storage)?;
        if let Err(error) = tokio::fs::rename(&temporary, &destination).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(ProtocolError::from(error));
        }
        let entry = ProtobufSchemaEntry {
            id: id.clone(),
            name: upload.name.trim().to_owned(),
            descriptorPath: format!("protobufDescriptors/{fileName}"),
            defaultMessageType: upload.defaultMessageType,
        };
        let mut configuration = self.protobuf.read().await.clone();
        configuration.schemas.push(entry);
        let mut persisted = self.persistedConfiguration().await;
        persisted.protobuf = configuration.clone();
        if persist(persisted).is_err() {
            let _ = tokio::fs::remove_file(&destination).await;
            return Err(ProtocolError::Storage);
        }
        self.descriptorPools.write().await.insert(id, pool);
        *self.protobuf.write().await = configuration.clone();
        Ok(configuration)
    }

    /// 按路由和消息侧解码录制正文；全部失败都返回解码结果，检查器可保留 Hex 视图。
    async fn decodeProtobuf(
        &self,
        transaction: &TransactionSummary,
        body: BodyResponse,
        side: DecodeSide,
    ) -> DecodedProtobufView {
        if body.meta.truncated {
            return DecodedProtobufView {
                messageType: None,
                json: None,
                decodeError: Some("bodyTruncated".to_owned()),
            };
        }
        let configuration = self.protobuf.read().await.clone();
        if !configuration.enabled {
            return DecodedProtobufView {
                messageType: None,
                json: None,
                decodeError: Some("protobufDisabled".to_owned()),
            };
        }
        let Some(route) = selectProtobufRoute(&configuration, transaction) else {
            return DecodedProtobufView {
                messageType: None,
                json: None,
                decodeError: Some("protobufRouteNotFound".to_owned()),
            };
        };
        let messageType = if matches!(side, DecodeSide::Response) {
            route
                .responseMessageType
                .as_deref()
                .unwrap_or(&route.messageType)
        } else {
            &route.messageType
        };
        let pools = self.descriptorPools.read().await;
        let Some(pool) = pools.get(&route.schemaId) else {
            return DecodedProtobufView {
                messageType: Some(messageType.to_owned()),
                json: None,
                decodeError: Some("descriptorUnavailable".to_owned()),
            };
        };
        let Some(descriptor) = pool.get_message_by_name(messageType) else {
            return DecodedProtobufView {
                messageType: Some(messageType.to_owned()),
                json: None,
                decodeError: Some("messageTypeNotFound".to_owned()),
            };
        };
        let bytes = match grpcPayload(&body.meta.contentType, &body.bytes) {
            Ok(bytes) => bytes,
            Err(error) => {
                return DecodedProtobufView {
                    messageType: Some(messageType.to_owned()),
                    json: None,
                    decodeError: Some(error.to_owned()),
                };
            }
        };
        match DynamicMessage::decode(descriptor, bytes) {
            Ok(message) => match serde_json::to_value(message) {
                Ok(json) => DecodedProtobufView {
                    messageType: Some(messageType.to_owned()),
                    json: Some(json),
                    decodeError: None,
                },
                Err(_) => DecodedProtobufView {
                    messageType: Some(messageType.to_owned()),
                    json: None,
                    decodeError: Some("protobufJsonSerializationFailed".to_owned()),
                },
            },
            Err(_) => DecodedProtobufView {
                messageType: Some(messageType.to_owned()),
                json: None,
                decodeError: Some("protobufDecodeFailed".to_owned()),
            },
        }
    }

    /// 返回 Validate 配置，在线允许状态可驱动人工或 CLI 的确认流程。
    async fn validateConfiguration(&self) -> ValidateConfiguration {
        self.validate.read().await.clone()
    }

    /// 校验并替换 Validate 配置；无效地址或重复校验器不会覆盖当前运行配置。
    async fn replaceValidateConfiguration(
        &self,
        configuration: ValidateConfiguration,
        persist: impl FnOnce(PersistedProtocolConfiguration) -> Result<(), std::io::Error>,
    ) -> Result<ValidateConfiguration, ProtocolError> {
        let _updateGuard = self.updateLock.lock().await;
        validateValidateConfiguration(&configuration)?;
        let mut persisted = self.persistedConfiguration().await;
        persisted.validate = configuration.clone();
        persist(persisted).map_err(|_| ProtocolError::Storage)?;
        *self.validate.write().await = configuration.clone();
        Ok(configuration)
    }

    /// 对已录制正文执行单一校验器；在线请求必须同时启用配置和一次性确认标记。
    async fn validateBody(
        &self,
        transactionId: &str,
        contentType: &str,
        body: &[u8],
        request: ValidateRequest,
    ) -> Result<ValidationReport, ProtocolError> {
        if !isTextualContent(contentType) {
            return Err(ProtocolError::UnsupportedValidationContent);
        }
        let configuration = self.validate.read().await.clone();
        if !configuration.enabled || !validatorEnabled(&configuration, request.validatorId) {
            return Err(ProtocolError::ValidatorDisabled);
        }
        let text =
            std::str::from_utf8(body).map_err(|_| ProtocolError::UnsupportedValidationContent)?;
        let issues = match request.validatorId {
            ValidatorId::HtmlWellFormed => validateHtmlWellFormed(text),
            ValidatorId::JsonSchema => validateJsonSyntax(text),
            ValidatorId::W3cHtmlOnline => {
                if !configuration.allowOnlineValidators || !request.onlineUploadConfirmed {
                    return Err(ProtocolError::OnlineUploadConfirmationRequired);
                }
                validateOnlineHtml(&self.httpClient, &configuration.w3cEndpoint, text).await?
            }
        };
        let report = ValidationReport {
            transactionId: transactionId.to_owned(),
            validatorId: request.validatorId,
            issues,
            validatedAtMilliseconds: currentTimeMilliseconds(),
        };
        let mut reports = self.validationReports.write().await;
        let entry = reports.entry(transactionId.to_owned()).or_default();
        entry.retain(|existing| existing.validatorId != report.validatorId);
        entry.push(report.clone());
        trimValidationReports(&mut reports);
        Ok(report)
    }

    /// 返回指定事务最近的各校验器报告；没有报告用空数组表达，不把普通读取错误化。
    async fn validationReports(&self, transactionId: &str) -> Vec<ValidationReport> {
        self.validationReports
            .read()
            .await
            .get(transactionId)
            .cloned()
            .unwrap_or_default()
    }
}

/// 为 ControlState 提供协议模块访问；实现留在独立模块，服务生命周期代码不感知具体协议。
impl ControlState {
    /// 返回 Protobuf 公开配置，供 Web、CLI 和 MCP 共用单一事实源。
    async fn protobufConfiguration(&self) -> ProtobufConfiguration {
        self.protocols.protobufConfiguration().await
    }

    /// 原子替换 Protobuf 解码路由；失败不会破坏已登记描述符或当前配置。
    async fn replaceProtobufConfiguration(
        &self,
        update: ProtobufConfigurationUpdate,
    ) -> Result<ProtobufConfiguration, ApiError> {
        self.protocols
            .replaceProtobufConfiguration(update, |configuration| {
                self.processSelection
                    .replaceProtocolConfiguration(configuration)
            })
            .await
            .map_err(mapProtocolError)
    }

    /// 登记已校验 FileDescriptorSet；描述符文件位于用户数据目录而不是工作区。
    async fn uploadProtobufDescriptor(
        &self,
        upload: ProtobufDescriptorUpload,
    ) -> Result<ProtobufConfiguration, ApiError> {
        self.protocols
            .uploadDescriptor(upload, |configuration| {
                self.processSelection
                    .replaceProtocolConfiguration(configuration)
            })
            .await
            .map_err(mapProtocolError)
    }

    /// 从录制正文读取指定消息侧并进行 Protobuf 解码；解码失败在响应中以 decodeError 表示。
    async fn decodeTransactionProtobuf(
        &self,
        transactionId: &str,
        side: DecodeSide,
    ) -> Result<DecodedProtobufView, ApiError> {
        let transaction = self
            .recording
            .getTransaction(transactionId)
            .await
            .map_err(super::mapCaptureLookupError)?;
        let body = self
            .recording
            .getBody(transactionId, side.messageSide())
            .await
            .map_err(super::mapCaptureLookupError)?;
        Ok(self
            .protocols
            .decodeProtobuf(&transaction, body, side)
            .await)
    }

    /// 返回 Validate 配置，调用方可在发送在线正文前展示明确确认界面。
    async fn validateConfiguration(&self) -> ValidateConfiguration {
        self.protocols.validateConfiguration().await
    }

    /// 原子替换 Validate 配置；不合格 endpoint 或重复 id 不会污染现有状态。
    async fn replaceValidateConfiguration(
        &self,
        configuration: ValidateConfiguration,
    ) -> Result<ValidateConfiguration, ApiError> {
        self.protocols
            .replaceValidateConfiguration(configuration, |configuration| {
                self.processSelection
                    .replaceProtocolConfiguration(configuration)
            })
            .await
            .map_err(mapProtocolError)
    }

    /// 校验事务响应正文；线上 W3C 路径由 ProtocolRuntime 强制检查显式上传确认。
    async fn validateTransaction(
        &self,
        transactionId: &str,
        request: ValidateRequest,
    ) -> Result<ValidationReport, ApiError> {
        let BodyResponse { meta, bytes } = self
            .recording
            .getBody(transactionId, MessageSide::Response)
            .await
            .map_err(super::mapCaptureLookupError)?;
        self.protocols
            .validateBody(transactionId, &meta.contentType, &bytes, request)
            .await
            .map_err(mapProtocolError)
    }

    /// 返回按需校验报告；事务不存在仍返回既有 404，空数组只表示尚未校验。
    async fn validationReports(
        &self,
        transactionId: &str,
    ) -> Result<Vec<ValidationReport>, ApiError> {
        self.recording
            .getTransaction(transactionId)
            .await
            .map_err(super::mapCaptureLookupError)?;
        Ok(self.protocols.validationReports(transactionId).await)
    }
}

/// 将 M6 端点附加到统一控制路由，继承现有 Origin、缓存和语言错误边界。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router
        .route(
            "/api/v1/tools/protobuf",
            get(getProtobuf).put(updateProtobuf),
        )
        .route("/api/v1/tools/protobuf/schemas", post(uploadProtobufSchema))
        .route(
            "/api/v1/transactions/{transactionId}/decode/protobuf",
            get(decodeProtobuf),
        )
        .route(
            "/api/v1/tools/validate",
            get(getValidate).put(updateValidate),
        )
        .route(
            "/api/v1/transactions/{transactionId}/validate",
            post(validateTransaction),
        )
        .route(
            "/api/v1/transactions/{transactionId}/validation",
            get(getValidationReports),
        )
}

/// 返回登记的描述符和路由；原始描述符字节只能由专用上传操作写入。
async fn getProtobuf(State(state): State<ControlState>) -> Json<ProtobufConfiguration> {
    Json(state.protobufConfiguration().await)
}

/// 替换 Protobuf 开关和路由表；未知字段被拒绝以避免控制契约静默漂移。
async fn updateProtobuf(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    updateResult: Result<Json<ProtobufConfigurationUpdate>, JsonRejection>,
) -> Result<Json<ProtobufConfiguration>, LocalizedApiError> {
    let Json(update) = updateResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidToolRequest).withLocale(locale))?;
    state
        .replaceProtobufConfiguration(update)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 上传一个 FileDescriptorSet 并在服务用户目录登记，上传文件绝不进入仓库或 Git。
async fn uploadProtobufSchema(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    uploadResult: Result<Json<ProtobufDescriptorUpload>, JsonRejection>,
) -> Result<Json<ProtobufConfiguration>, LocalizedApiError> {
    let Json(upload) = uploadResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidToolRequest).withLocale(locale))?;
    state
        .uploadProtobufDescriptor(upload)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 解码已录制正文；缺描述符、压缩帧和解码失败均作为 decodeError 返回，调用方可回退 Hex。
async fn decodeProtobuf(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    AxumPath(transactionId): AxumPath<String>,
    Query(query): Query<ProtobufDecodeQuery>,
) -> Result<Json<DecodedProtobufView>, LocalizedApiError> {
    state
        .decodeTransactionProtobuf(&transactionId, query.side.unwrap_or(DecodeSide::Response))
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 返回 Validate 配置，客户端据此决定是否展示正文上传确认对话框。
async fn getValidate(State(state): State<ControlState>) -> Json<ValidateConfiguration> {
    Json(state.validateConfiguration().await)
}

/// 原子替换 Validate 配置；重复校验器或非 http(s) endpoint 会返回结构化错误。
async fn updateValidate(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    updateResult: Result<Json<ValidateConfiguration>, JsonRejection>,
) -> Result<Json<ValidateConfiguration>, LocalizedApiError> {
    let Json(configuration) = updateResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidToolRequest).withLocale(locale))?;
    state
        .replaceValidateConfiguration(configuration)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 触发响应正文校验；W3C 必须携带 onlineUploadConfirmed=true 才会发生任何外部传输。
async fn validateTransaction(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    AxumPath(transactionId): AxumPath<String>,
    requestResult: Result<Json<ValidateRequest>, JsonRejection>,
) -> Result<Json<ValidationReport>, LocalizedApiError> {
    let Json(request) = requestResult
        .map_err(|_| ApiError::badRequest(ErrorCode::InvalidToolRequest).withLocale(locale))?;
    state
        .validateTransaction(&transactionId, request)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 返回事务的按需报告；报告不含正文，以防校验结果额外保存流量副本。
async fn getValidationReports(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    AxumPath(transactionId): AxumPath<String>,
) -> Result<Json<Vec<ValidationReport>>, LocalizedApiError> {
    state
        .validationReports(&transactionId)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 在配置替换前校验路由引用和消息类型；任一失败均不改变当前描述符或路由集合。
fn validateProtobufRoutes(
    routes: &[ProtobufRoute],
    schemas: &[ProtobufSchemaEntry],
    pools: &HashMap<String, DescriptorPool>,
) -> Result<(), ProtocolError> {
    let mut identifiers = std::collections::HashSet::new();
    for route in routes {
        if route.id.trim().is_empty() || !identifiers.insert(route.id.as_str()) {
            return Err(ProtocolError::InvalidProtobufRoute);
        }
        validateLocationPattern(&route.location)
            .map_err(|_| ProtocolError::InvalidProtobufRoute)?;
        let schema = schemas
            .iter()
            .find(|schema| schema.id == route.schemaId)
            .ok_or(ProtocolError::InvalidProtobufRoute)?;
        let pool = pools
            .get(&schema.id)
            .ok_or(ProtocolError::InvalidProtobufRoute)?;
        if route.messageType.is_empty() || pool.get_message_by_name(&route.messageType).is_none() {
            return Err(ProtocolError::InvalidProtobufRoute);
        }
        if route
            .responseMessageType
            .as_deref()
            .is_some_and(|name| name.is_empty() || pool.get_message_by_name(name).is_none())
        {
            return Err(ProtocolError::InvalidProtobufRoute);
        }
    }
    Ok(())
}

/// 校验上传元数据与编码载荷上限，避免文件名或 Base64 载荷驱动无界内存分配。
fn validateDescriptorUpload(upload: &ProtobufDescriptorUpload) -> Result<(), ProtocolError> {
    if upload.name.trim().is_empty()
        || upload.name.len() > 256
        || upload.defaultMessageType.trim().is_empty()
        || upload.defaultMessageType.len() > 512
        || upload.base64.len() > maximumDescriptorBytes.saturating_mul(2)
    {
        return Err(ProtocolError::InvalidDescriptor);
    }
    Ok(())
}

/// 从事务摘要构造统一 Location 候选，避免 Protobuf 路由重新解析 URL 或拥有第二套匹配语义。
fn transactionLocation(transaction: &TransactionSummary) -> ResolvedLocation {
    let protocol = match transaction.protocol {
        TransactionProtocol::Http => "http",
        TransactionProtocol::Https => "https",
        TransactionProtocol::Ws => "ws",
        TransactionProtocol::Wss => "wss",
        TransactionProtocol::Tunnel => "https",
        TransactionProtocol::Socks => "socks",
    };
    ResolvedLocation {
        protocol: protocol.to_owned(),
        host: transaction.host.clone(),
        port: transaction.port,
        path: transaction.path.clone(),
        query: transaction.query.clone(),
        display: transaction.urlDisplay.clone(),
    }
}

/// 按配置顺序选择第一个 Location 命中路由，顺序即用户可控的优先级。
fn selectProtobufRoute(
    configuration: &ProtobufConfiguration,
    transaction: &TransactionSummary,
) -> Option<ProtobufRoute> {
    let location = transactionLocation(transaction);
    configuration
        .routes
        .iter()
        .find(|route| {
            locationMatches(&route.location, &location, LocationMatchOptions::default())
                .unwrap_or(false)
        })
        .cloned()
}

/// 剥离 gRPC 单帧 5 字节前缀；压缩帧必须回退 Hex，禁止把压缩数据当 Protobuf 解析。
fn grpcPayload<'a>(contentType: &str, bytes: &'a [u8]) -> Result<&'a [u8], &'static str> {
    if !contentType
        .to_ascii_lowercase()
        .starts_with("application/grpc")
    {
        return Ok(bytes);
    }
    if bytes.len() < 5 {
        return Err("grpcFrameTooShort");
    }
    if bytes[0] != 0 {
        return Err("grpcCompressionUnsupported");
    }
    let length = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    if length != bytes.len().saturating_sub(5) {
        return Err("grpcFrameLengthMismatch");
    }
    Ok(&bytes[5..])
}

/// 校验 Validate 的唯一 validator id 与安全的在线 endpoint；在线执行仍需单次确认。
fn validateValidateConfiguration(
    configuration: &ValidateConfiguration,
) -> Result<(), ProtocolError> {
    let mut identifiers = std::collections::HashSet::new();
    if configuration.validators.is_empty()
        || configuration
            .validators
            .iter()
            .any(|validator| !identifiers.insert(validator.id))
    {
        return Err(ProtocolError::InvalidValidateConfiguration);
    }
    let endpoint = reqwest::Url::parse(&configuration.w3cEndpoint)
        .map_err(|_| ProtocolError::InvalidValidateConfiguration)?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(ProtocolError::InvalidValidateConfiguration);
    }
    Ok(())
}

/// 从统一配置引用的受控描述符文件重建运行时池；路径、大小或消息类型不一致都会阻止带坏配置启动。
async fn loadDescriptorPools(
    dataDirectory: &Path,
    configuration: &ProtobufConfiguration,
) -> Result<HashMap<String, DescriptorPool>, std::io::Error> {
    let mut pools = HashMap::new();
    for schema in &configuration.schemas {
        let expectedPath = format!("protobufDescriptors/{}.desc", schema.id);
        if Uuid::parse_str(&schema.id).is_err() || schema.descriptorPath != expectedPath {
            return Err(protocolConfigurationIoError(
                ProtocolError::InvalidDescriptor,
            ));
        }
        let bytes = tokio::fs::read(dataDirectory.join(&schema.descriptorPath)).await?;
        if bytes.is_empty() || bytes.len() > maximumDescriptorBytes {
            return Err(protocolConfigurationIoError(
                ProtocolError::InvalidDescriptor,
            ));
        }
        let pool = DescriptorPool::decode(bytes.as_slice())
            .map_err(|_| protocolConfigurationIoError(ProtocolError::InvalidDescriptor))?;
        if pool
            .get_message_by_name(&schema.defaultMessageType)
            .is_none()
            || pools.insert(schema.id.clone(), pool).is_some()
        {
            return Err(protocolConfigurationIoError(
                ProtocolError::InvalidDescriptorMessageType,
            ));
        }
    }
    Ok(pools)
}

/// 把持久化协议配置的语义错误转换为启动阶段可诊断的无效数据错误，不暴露本机文件路径。
fn protocolConfigurationIoError(error: ProtocolError) -> std::io::Error {
    let detail = match error {
        ProtocolError::InvalidDescriptor => "保存的 Protobuf 描述符无效",
        ProtocolError::InvalidDescriptorMessageType => "保存的 Protobuf 消息类型无效",
        ProtocolError::InvalidProtobufRoute => "保存的 Protobuf 路由无效",
        ProtocolError::InvalidValidateConfiguration => "保存的正文校验配置无效",
        _ => "保存的协议工具配置无效",
    };
    std::io::Error::new(std::io::ErrorKind::InvalidData, detail)
}

/// 判断指定校验器是否启用；缺失条目等价于关闭，避免意外外发正文。
fn validatorEnabled(configuration: &ValidateConfiguration, id: ValidatorId) -> bool {
    configuration
        .validators
        .iter()
        .find(|validator| validator.id == id)
        .is_some_and(|validator| validator.enabled)
}

/// 仅允许文本、JSON、XML、HTML 与脚本内容进入 Validate；二进制由 Hex 或 Protobuf 处理。
fn isTextualContent(contentType: &str) -> bool {
    let contentType = contentType.to_ascii_lowercase();
    contentType.starts_with("text/")
        || contentType.contains("json")
        || contentType.contains("xml")
        || contentType.contains("javascript")
        || contentType.contains("html")
}

/// 对 HTML 标签做线性栈校验；该离线检查只负责基础配对，不伪装为浏览器或 W3C 全规范验证。
fn validateHtmlWellFormed(source: &str) -> Vec<ValidationIssue> {
    let mut stack: Vec<(String, usize, usize)> = Vec::new();
    let mut issues = Vec::new();
    let mut offset = 0_usize;
    while let Some(relativeStart) = source[offset..].find('<') {
        let start = offset + relativeStart;
        let Some(relativeEnd) = source[start..].find('>') else {
            let (line, column) = lineAndColumn(source, start);
            issues.push(validationIssue("validate.html.unclosedTag", line, column));
            break;
        };
        let end = start + relativeEnd;
        let token = source[start + 1..end].trim();
        offset = end.saturating_add(1);
        if token.is_empty() || token.starts_with('!') || token.starts_with('?') {
            continue;
        }
        let closing = token.starts_with('/');
        let name = token
            .trim_start_matches('/')
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        if name.is_empty() || !name.bytes().all(isHtmlNameByte) {
            continue;
        }
        let name = name.to_ascii_lowercase();
        let (line, column) = lineAndColumn(source, start);
        if closing {
            match stack.pop() {
                Some((opened, _, _)) if opened == name => {}
                Some((_, openedLine, openedColumn)) => {
                    issues.push(validationIssue("validate.html.mismatchedTag", line, column));
                    issues.push(validationIssue(
                        "validate.html.openedHere",
                        openedLine,
                        openedColumn,
                    ));
                    stack.clear();
                }
                None => issues.push(validationIssue(
                    "validate.html.unexpectedClosingTag",
                    line,
                    column,
                )),
            }
        } else if !token.ends_with('/') && !isHtmlVoidElement(&name) {
            stack.push((name, line, column));
        }
    }
    for (_, line, column) in stack {
        issues.push(validationIssue("validate.html.unclosedTag", line, column));
    }
    issues
}

/// 解析 JSON 基础语法；没有 schema 时不伪造字段级校验结果。
fn validateJsonSyntax(source: &str) -> Vec<ValidationIssue> {
    match serde_json::from_str::<Value>(source) {
        Ok(_) => Vec::new(),
        Err(error) => vec![validationIssue(
            "validate.json.invalidSyntax",
            error.line(),
            error.column(),
        )],
    }
}

/// 向用户配置的 W3C endpoint 发送最小正文并将公开 messages 映射为本地化键驱动的问题列表。
async fn validateOnlineHtml(
    client: &reqwest::Client,
    endpoint: &str,
    source: &str,
) -> Result<Vec<ValidationIssue>, ProtocolError> {
    let response = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(source.to_owned())
        .send()
        .await
        .map_err(|_| ProtocolError::OnlineValidationFailed)?;
    if !response.status().is_success() {
        return Err(ProtocolError::OnlineValidationFailed);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| ProtocolError::OnlineValidationFailed)?;
    let payload: Value =
        serde_json::from_slice(&bytes).map_err(|_| ProtocolError::OnlineValidationFailed)?;
    Ok(payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .map(|message| ValidationIssue {
                    severity: match message.get("type").and_then(Value::as_str) {
                        Some("error") => ValidationSeverity::Error,
                        Some("warning") | Some("info warning") => ValidationSeverity::Warning,
                        _ => ValidationSeverity::Info,
                    },
                    messageKey: "validate.online.message".to_owned(),
                    line: message
                        .get("lastLine")
                        .and_then(Value::as_u64)
                        .map(|value| value as usize),
                    column: message
                        .get("lastColumn")
                        .and_then(Value::as_u64)
                        .map(|value| value as usize),
                })
                .collect()
        })
        .unwrap_or_default())
}

/// 将报告索引限制在固定事务数内；按每个事务最早报告时间淘汰，避免长期会话无界增长。
fn trimValidationReports(reports: &mut HashMap<String, Vec<ValidationReport>>) {
    if reports.len() <= maximumValidationReports {
        return;
    }
    let mut oldest = reports
        .iter()
        .map(|(id, entries)| {
            (
                id.clone(),
                entries
                    .iter()
                    .map(|entry| entry.validatedAtMilliseconds)
                    .min()
                    .unwrap_or(u64::MAX),
            )
        })
        .collect::<Vec<_>>();
    oldest.sort_by_key(|(_, time)| *time);
    for (id, _) in oldest
        .into_iter()
        .take(reports.len() - maximumValidationReports)
    {
        reports.remove(&id);
    }
}

/// 按 UTF-8 字符前缀计算 1 基行列；调用方仅传入 find 返回的合法字符边界。
fn lineAndColumn(source: &str, byteOffset: usize) -> (usize, usize) {
    let prefix = &source[..byteOffset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.chars().count() + 1, |(_, value)| {
            value.chars().count() + 1
        });
    (line, column)
}

/// 判断 HTML 标签名称允许的 ASCII 字节；自定义元素允许短横线，属性由浏览器/W3C 负责完整解析。
fn isHtmlNameByte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':')
}

/// 返回 HTML void 元素集合，避免 img、meta 等被误记为未闭合标签。
fn isHtmlVoidElement(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// 创建本地化键驱动问题对象，数值位置由检查器映射至原始正文。
fn validationIssue(messageKey: &str, line: usize, column: usize) -> ValidationIssue {
    ValidationIssue {
        severity: ValidationSeverity::Error,
        messageKey: messageKey.to_owned(),
        line: Some(line),
        column: Some(column),
    }
}

/// 将内部协议错误映射为既有本地化控制错误；不泄露描述符路径、网络错误或正文内容。
fn mapProtocolError(error: ProtocolError) -> ApiError {
    match error {
        ProtocolError::Storage | ProtocolError::OnlineValidationFailed => {
            ApiError::internal(ErrorCode::ToolOperationFailed)
        }
        ProtocolError::InvalidDescriptor
        | ProtocolError::InvalidDescriptorMessageType
        | ProtocolError::InvalidProtobufRoute
        | ProtocolError::InvalidValidateConfiguration
        | ProtocolError::UnsupportedValidationContent
        | ProtocolError::ValidatorDisabled
        | ProtocolError::OnlineUploadConfirmationRequired => {
            ApiError::badRequest(ErrorCode::InvalidToolConfiguration)
        }
    }
}

/// 定义协议模块内部失败类别；所有变体在控制边界映射为稳定、可本地化的已有错误码。
enum ProtocolError {
    InvalidDescriptor,
    InvalidDescriptorMessageType,
    InvalidProtobufRoute,
    InvalidValidateConfiguration,
    UnsupportedValidationContent,
    ValidatorDisabled,
    OnlineUploadConfirmationRequired,
    OnlineValidationFailed,
    Storage,
}

impl From<std::io::Error> for ProtocolError {
    /// 将描述符目录 I/O 折叠为不泄露用户文件系统细节的存储失败。
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
