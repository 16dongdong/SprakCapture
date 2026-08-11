//! 承载控制面的 HTTP 传输契约、路由装配与本地化错误响应。
//!
//! 该模块只处理 Axum 边界：核心生命周期与事务读写仍由 `ControlState` 负责，避免路由层
//! 持有第二份运行时状态。公开类型继续由父模块重导出，因此外部 ABI、序列化字段和路径不变。

use axum::{
    Json, Router,
    extract::{
        Path, Query, Request, State,
        rejection::{JsonRejection, QueryRejection},
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{
            ACCEPT_LANGUAGE, ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE,
            CONTENT_TYPE, ORIGIN, RANGE,
        },
    },
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event as ServerSentEvent, KeepAlive, Sse},
    },
    routing::{delete, get, post, put},
};
use capture_core::{
    BodyHandleMeta, CaptureError, HeaderField, MessageSide, RecordingSettingsUpdate,
    RecordingSnapshot, StreamPacket, TransactionSummary,
};
use http_proxy_core::SslPublicState;
use process_capture_core::ProcessCaptureSnapshot;
use serde::{Deserialize, Serialize};
use socks5_core::{ServiceMetrics, SessionSnapshot};
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

use std::{convert::Infallible, time::Duration};

use super::{
    ConfigurationUpdate, ControlSnapshot, ControlState, ListenerSnapshots, PublicConfiguration,
    ServiceState, ToolsPublicState, listenerControl, mapLocalImport, mcpControl,
    mediaPreviewControl, pluginControl, processControl, protocolControl, repeatControl, sslControl,
    toolControl, waitForControlShutdown,
};
use crate::localization::{
    ErrorCode, Locale, MessageParams, RequestLocale, localizeError, resolveRequestLocale,
};
use crate::transactionProjection::TransactionPage;

// Web 开发、桌面联调和端到端验证使用固定本机端口；显式列出 Origin，避免放宽为通配符。
const allowedControlOrigins: [&str; 7] = [
    "http://127.0.0.1:5173",
    "http://localhost:5173",
    "http://127.0.0.1:5174",
    "http://localhost:5174",
    "http://127.0.0.1:5175",
    "http://localhost:5175",
    "http://tauri.localhost",
];

/// 将 Capture 写操作错误映射到控制面稳定机器码；底层诊断只进入 detail 参数。
pub(super) fn mapCaptureOperationError(error: CaptureError) -> ApiError {
    match error {
        CaptureError::InvalidLimits => ApiError::badRequest(ErrorCode::InvalidRecordingLimits),
        CaptureError::CollectionChanged => {
            ApiError::conflict(ErrorCode::TransactionsCollectionChanged)
        }
        CaptureError::Location(_) => ApiError::badRequest(ErrorCode::InvalidRecordingLocation)
            .withParam("detail", error.to_string()),
        _ => ApiError::internal(ErrorCode::RecordingOperationFailed)
            .withParam("detail", error.to_string()),
    }
}

/// 将事务查找错误映射为 404，其余 Capture 失败保持后端运行错误。
pub(super) fn mapCaptureLookupError(error: CaptureError) -> ApiError {
    match error {
        CaptureError::TransactionNotFound => ApiError::notFound(ErrorCode::TransactionNotFound),
        CaptureError::BodyNotFound => ApiError::notFound(ErrorCode::BodyNotFound),
        _ => mapCaptureOperationError(error),
    }
}

/// 给独立录制端点补充当前全局 revision，调用方可与 WebSocket 事件建立顺序关系。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RecordingResponse {
    pub(super) serverInstanceId: String,
    pub(super) revision: u64,
    pub(super) recording: RecordingSnapshot,
}

/// 提供无 UI 运行时探测结果；不包含监听配置、事务或任何认证材料。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    serverInstanceId: String,
    revision: u64,
}

/// 返回控制服务语义版本，CLI 可用它在自动化前确认协议代际。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionResponse {
    version: &'static str,
}

/// 返回单条事务的摘要、两侧头和正文元信息；正文实际字节仍通过 body 端点按需读取。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TransactionDetail {
    pub(super) revision: u64,
    pub(super) transaction: TransactionSummary,
    pub(super) requestHeaders: Vec<HeaderField>,
    pub(super) responseHeaders: Vec<HeaderField>,
    pub(super) requestBody: Option<BodyHandleMeta>,
    pub(super) responseBody: Option<BodyHandleMeta>,
    pub(super) requestPackets: Vec<StreamPacket>,
    pub(super) responsePackets: Vec<StreamPacket>,
}

/// 描述自动识别得到的应用层派生正文；算法标识稳定供界面说明，字节不回写原事务。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DecodedBodyResponse {
    pub(super) algorithm: String,
    pub(super) contentType: String,
    pub(super) decodedBytes: usize,
    pub(super) base64: String,
}

/// 将 capture-core 的原始正文及可选应用层派生正文编码为 Web 可稳定传输的标准 base64。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EncodedBodyResponse {
    pub(super) revision: u64,
    pub(super) meta: BodyHandleMeta,
    pub(super) base64: String,
    pub(super) decoded: Option<DecodedBodyResponse>,
}

/// 约束事务列表分页；默认只拉取最近一页，防止控制请求无界复制摘要。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TransactionQuery {
    pub(super) offset: Option<usize>,
    pub(super) limit: Option<usize>,
    pub(super) collectionToken: Option<String>,
}

/// 定义 SSE 与 WebSocket 共用的判别联合；每种增量消息都携带对应 revision。
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EventMessage {
    Snapshot {
        serverInstanceId: String,
        snapshot: Box<ControlSnapshot>,
    },
    ServiceState {
        serverInstanceId: String,
        revision: u64,
        serviceState: ServiceState,
        listeners: ListenerSnapshots,
    },
    Metrics {
        serverInstanceId: String,
        revision: u64,
        metrics: ServiceMetrics,
    },
    AdvancedRepeats {
        serverInstanceId: String,
        revision: u64,
        jobs: Vec<repeatControl::AdvancedRepeatJob>,
    },
    Plugins {
        serverInstanceId: String,
        revision: u64,
        plugins: Vec<plugin_host::PluginSnapshot>,
    },
    Mcp {
        serverInstanceId: String,
        revision: u64,
        mcp: mcpControl::McpPublicState,
    },
    ProcessCapture {
        serverInstanceId: String,
        revision: u64,
        processCapture: ProcessCaptureSnapshot,
    },
    Sessions {
        serverInstanceId: String,
        revision: u64,
        sessions: Vec<SessionSnapshot>,
    },
    Configuration {
        serverInstanceId: String,
        revision: u64,
        configuration: PublicConfiguration,
    },
    Ssl {
        serverInstanceId: String,
        revision: u64,
        ssl: SslPublicState,
    },
    Recording {
        serverInstanceId: String,
        revision: u64,
        recording: RecordingSnapshot,
    },
    Transactions {
        serverInstanceId: String,
        revision: u64,
        /// 事件发布权威全量摘要；clear/FIFO 淘汰通过缺失项明确表达删除，避免前端残留幽灵事务。
        transactions: TransactionPage,
    },
    Tools {
        serverInstanceId: String,
        revision: u64,
        // 工具状态显著大于其他事件载荷；独立分配可把高频事件枚举本体保持在较小固定尺寸。
        tools: Box<ToolsPublicState>,
    },
    Breakpoints {
        serverInstanceId: String,
        revision: u64,
        suspended: Vec<http_proxy_core::SuspendedBreakpoint>,
    },
}

/// 判断请求中的单个 Origin 是否属于桌面与开发 Web 的精确允许列表；重复 Origin 一律拒绝。
fn isAllowedControlOrigin(headers: &HeaderMap) -> bool {
    let mut origins = headers.get_all(ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return true;
    };
    if origins.next().is_some() {
        return false;
    }
    origin
        .to_str()
        .is_ok_and(|origin| allowedControlOrigins.contains(&origin))
}

/// 在路由执行前拒绝未声明 Origin；无 Origin 的本机 CLI 保持可用，WebSocket 使用同一边界。
async fn validateControlOrigin(request: Request, next: Next) -> Response {
    if !isAllowedControlOrigin(request.headers()) {
        let locale = resolveRequestLocale(request.uri(), request.headers());
        return ApiError::forbidden(ErrorCode::OriginForbidden).intoLocalizedResponse(locale);
    }
    next.run(request).await
}

/// 给控制面的成功、错误和预检响应统一设置 no-store，防止抓包元数据与配置进入浏览器缓存。
async fn applyControlCachePolicy(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// 公开完整 v1 控制路由，所有路径均由同一 ControlState 管理。
pub fn createControlRouter(state: ControlState) -> Router {
    let allowedOrigins = allowedControlOrigins.map(HeaderValue::from_static);
    let corsLayer = CorsLayer::new()
        .allow_origin(allowedOrigins)
        .allow_methods([
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::DELETE,
        ])
        .allow_headers([CONTENT_TYPE, ACCEPT_LANGUAGE, RANGE])
        .expose_headers([
            CONTENT_TYPE,
            CONTENT_LENGTH,
            CONTENT_RANGE,
            ACCEPT_RANGES,
            HeaderName::from_static("x-media-preview-status"),
            HeaderName::from_static("x-media-preview-captured-bytes"),
            HeaderName::from_static("x-media-preview-total-bytes"),
            HeaderName::from_static("x-media-preview-segment-count"),
        ]);
    let router = Router::new()
        .route("/api/v1/health", get(getHealth))
        .route("/api/v1/version", get(getVersion))
        .route("/api/v1/snapshot", get(getSnapshot))
        .route("/api/v1/service/start", post(startService))
        .route("/api/v1/service/stop", post(stopService))
        .route("/api/v1/configuration", put(replaceConfiguration));
    mcpControl::addRoutes(mediaPreviewControl::addRoutes(processControl::addRoutes(
        protocolControl::addRoutes(repeatControl::addRoutes(mapLocalImport::addRoutes(
            toolControl::addRoutes(pluginControl::addRoutes(listenerControl::addRoutes(
                sslControl::addRoutes(router),
            ))),
        ))),
    )))
    .route("/api/v1/sessions", delete(clearSessions))
    .route("/api/v1/recording", get(getRecording).put(updateRecording))
    .route("/api/v1/recording/clear", post(clearRecording))
    .route("/api/v1/transactions", get(listTransactions))
    .route("/api/v1/transactions/{transactionId}", get(getTransaction))
    .route(
        "/api/v1/transactions/{transactionId}/request/body",
        get(getRequestBody),
    )
    .route(
        "/api/v1/transactions/{transactionId}/response/body",
        get(getResponseBody),
    )
    .route("/api/v1/events", get(upgradeEvents))
    .route("/api/v1/events/sse", get(streamServerSentEvents))
    .layer(corsLayer)
    .layer(middleware::from_fn(validateControlOrigin))
    .layer(middleware::from_fn(applyControlCachePolicy))
    .with_state(state)
}

/// 返回控制服务存活信号；只读请求不推进 revision 或创建状态变更。
async fn getHealth(State(state): State<ControlState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        serverInstanceId: state.serverInstanceId.to_string(),
        revision: state.currentRevision(),
    })
}

/// 返回后端包版本，供 CLI 与外部编排读取而不依赖可变构建路径。
async fn getVersion() -> Json<VersionResponse> {
    Json(VersionResponse {
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// 返回当前控制快照。
async fn getSnapshot(State(state): State<ControlState>) -> Json<ControlSnapshot> {
    Json(state.snapshot().await)
}

/// 启动 SOCKS5 数据面并返回最新快照。
async fn startService(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<ControlSnapshot>, LocalizedApiError> {
    state
        .startService()
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 停止 SOCKS5 数据面并返回最新快照。
async fn stopService(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<ControlSnapshot>, LocalizedApiError> {
    state
        .stopService()
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 校验并替换完整服务配置；运行中的数据面由状态对象强制断连并重启，原始口令不进入返回对象。
async fn replaceConfiguration(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    updateResult: Result<Json<ConfigurationUpdate>, JsonRejection>,
) -> Result<Json<ControlSnapshot>, LocalizedApiError> {
    // 将框架默认文本拒绝统一映射为 JSON 错误，控制客户端不需要维护第二套错误解码。
    let Json(update) = updateResult.map_err(|error| {
        ApiError::badRequest(ErrorCode::InvalidConfigurationRequest)
            .withParam("detail", error.body_text())
            .withLocale(locale)
    })?;
    state
        .replaceConfiguration(update)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 清除已结束会话记录并返回最新快照。
async fn clearSessions(State(state): State<ControlState>) -> Json<ControlSnapshot> {
    Json(state.clearSessions().await)
}

/// 返回不含事务正文的录制状态。
async fn getRecording(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<RecordingResponse>, LocalizedApiError> {
    state
        .recordingResponse()
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 校验并原子更新录制状态、限额和忽略规则。
async fn updateRecording(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    updateResult: Result<Json<RecordingSettingsUpdate>, JsonRejection>,
) -> Result<Json<RecordingResponse>, LocalizedApiError> {
    let Json(update) = updateResult.map_err(|error| {
        ApiError::badRequest(ErrorCode::InvalidRecordingRequest)
            .withParam("detail", error.body_text())
            .withLocale(locale)
    })?;
    state
        .updateRecording(update)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 清空当前 RecordingSession 的事务、头和正文，保留会话标识与累计 droppedCount。
async fn clearRecording(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<RecordingResponse>, LocalizedApiError> {
    state
        .clearRecording()
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 返回分页事务摘要；查询字段非法时统一映射结构化控制错误。
async fn listTransactions(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    queryResult: Result<Query<TransactionQuery>, QueryRejection>,
) -> Result<Json<TransactionPage>, LocalizedApiError> {
    let Query(query) = queryResult.map_err(|error| {
        ApiError::badRequest(ErrorCode::InvalidTransactionsQuery)
            .withParam("detail", error.body_text())
            .withLocale(locale)
    })?;
    state
        .transactionPage(query)
        .await
        .map(Json)
        .map_err(|error| error.withLocale(locale))
}

/// 返回单条事务详情，不在响应中内联正文实际字节。
async fn getTransaction(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(transactionId): Path<String>,
) -> Result<Json<TransactionDetail>, LocalizedApiError> {
    let detail = state
        .transactionDetail(&transactionId)
        .await
        .map_err(|error| error.withLocale(locale))?;
    Ok(Json(detail))
}

/// 返回请求正文元信息和标准 base64。
async fn getRequestBody(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(transactionId): Path<String>,
) -> Result<Json<EncodedBodyResponse>, LocalizedApiError> {
    let body = state
        .transactionBody(&transactionId, MessageSide::Request)
        .await
        .map_err(|error| error.withLocale(locale))?;
    Ok(Json(body))
}

/// 返回响应正文元信息和标准 base64。
async fn getResponseBody(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    Path(transactionId): Path<String>,
) -> Result<Json<EncodedBodyResponse>, LocalizedApiError> {
    let body = state
        .transactionBody(&transactionId, MessageSide::Response)
        .await
        .map_err(|error| error.withLocale(locale))?;
    Ok(Json(body))
}

/// 以 SSE 持续推送控制面增量；只读页面不再依赖定时 GET，也无需维护双向 WebSocket 握手。
///
/// 运行上下文：工作台及其他只读实时视图建立连接时调用。订阅先于首快照生成，确保首帧与广播队列
/// 之间没有丢事件窗口；重复帧由前端使用 `serverInstanceId + revision` 幂等合并。
/// 失败语义：订阅关闭、服务退出或序列化失败会结束响应，由浏览器 EventSource 自动重连。
async fn streamServerSentEvents(
    State(state): State<ControlState>,
) -> Sse<impl futures_util::Stream<Item = Result<ServerSentEvent, Infallible>>> {
    let receiver = state.subscribeEvents();
    let shutdownReceiver = state.subscribeShutdown();
    let initialEvent = state.snapshotEvent().await;
    let stream = futures_util::stream::unfold(
        (Some(initialEvent), receiver, shutdownReceiver, state),
        |(mut initialEvent, mut receiver, mut shutdownReceiver, state)| async move {
            if let Some(event) = initialEvent.take() {
                let encoded = encodeServerSentEvent(event)?;
                return Some((
                    Ok(encoded),
                    (initialEvent, receiver, shutdownReceiver, state),
                ));
            }

            tokio::select! {
                // `waitForControlShutdown` 会先读取 watch 当前值，覆盖关闭通知早于 SSE
                // 流首次轮询的竞态；直接等待 `changed` 会让迟到订阅者挂到强制排空超时。
                _ = waitForControlShutdown(&mut shutdownReceiver) => None,
                event = receiver.recv() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(broadcast::error::RecvError::Lagged(_)) => state.snapshotEvent().await,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    };
                    let encoded = encodeServerSentEvent(event)?;
                    Some((
                        Ok(encoded),
                        (initialEvent, receiver, shutdownReceiver, state),
                    ))
                }
            }
        },
    );

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("control-event-stream"),
    )
}

/// 把内部判别联合编码为单条 SSE 帧；序列化异常终止连接，禁止半帧污染客户端状态。
fn encodeServerSentEvent(event: EventMessage) -> Option<ServerSentEvent> {
    match serde_json::to_string(&event) {
        Ok(json) => Some(ServerSentEvent::default().event("control").data(json)),
        Err(error) => {
            eprintln!("控制事件序列化失败：{error}");
            None
        }
    }
}

/// 升级到 WebSocket 并订阅后续控制事件；保留双向兼容通道供交互式控制客户端使用。
async fn upgradeEvents(
    webSocket: WebSocketUpgrade,
    State(state): State<ControlState>,
) -> impl IntoResponse {
    webSocket.on_upgrade(move |socket| streamEvents(socket, state))
}

/// 先发送完整 snapshot，再推送 revision 单调递增的后续事件。
async fn streamEvents(mut socket: WebSocket, state: ControlState) {
    let mut receiver = state.eventSender.subscribe();
    let mut shutdownReceiver = state.shutdownSender.subscribe();
    let initialMessage = state.snapshotEvent().await;
    let Ok(initialText) = serde_json::to_string(&initialMessage) else {
        return;
    };
    if socket
        .send(Message::Text(initialText.into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            _ = waitForControlShutdown(&mut shutdownReceiver) => break,
            event = receiver.recv() => {
                let event = match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => state.snapshotEvent().await,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let Ok(text) = serde_json::to_string(&event) else {
                    break;
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

/// 保存 HTTP 错误的机器码与插值参数；原始错误文本只允许作为受控 detail 参数存在。
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    pub(super) code: ErrorCode,
    params: MessageParams,
}

/// 绑定请求协商语言与错误；Axum 只在离开处理器时生成最终本地化响应。
#[derive(Debug)]
pub(super) struct LocalizedApiError {
    error: ApiError,
    locale: Locale,
}

/// 定义唯一结构化错误响应；message 是本地化显示文本，机器处理只依赖 code 和 messageKey。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    code: ErrorCode,
    message: String,
    messageKey: &'static str,
    params: MessageParams,
}

impl ApiError {
    /// 返回适合中文进程日志的错误文本；HTTP 请求仍按各自协商语言独立渲染。
    pub fn message(&self) -> String {
        localizeError(self.code, Locale::ZhHans, &self.params)
    }

    /// 创建请求字段错误；具体字段值通过 withParam 补充，禁止拼接进稳定错误码。
    pub(super) fn badRequest(code: ErrorCode) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            params: MessageParams::new(),
        }
    }

    /// 创建生命周期冲突错误。
    pub(super) fn conflict(code: ErrorCode) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            params: MessageParams::new(),
        }
    }

    /// 创建控制来源拒绝错误。
    fn forbidden(code: ErrorCode) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code,
            params: MessageParams::new(),
        }
    }

    /// 创建资源不存在错误，供被清空或 FIFO 淘汰的事务与正文稳定返回 404。
    pub(super) fn notFound(code: ErrorCode) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            params: MessageParams::new(),
        }
    }

    /// 创建后端运行错误。
    pub(super) fn internal(code: ErrorCode) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code,
            params: MessageParams::new(),
        }
    }

    /// 添加一个公开插值参数；参数进入结构化响应，调用方不得传入口令或其他秘密字段。
    pub(super) fn withParam(mut self, name: &str, value: impl Into<String>) -> Self {
        self.params.insert(name.to_owned(), value.into());
        self
    }

    /// 绑定当前请求语言；错误的状态码、机器码和参数在绑定过程中保持不变。
    pub(super) fn withLocale(self, locale: Locale) -> LocalizedApiError {
        LocalizedApiError {
            error: self,
            locale,
        }
    }

    /// 生成完整 JSON 响应；状态码、机器码、本地化文案与插值参数来自同一错误实例。
    fn intoLocalizedResponse(self, locale: Locale) -> Response {
        let message = localizeError(self.code, locale, &self.params);
        let response = ErrorResponse {
            code: self.code,
            message,
            messageKey: self.code.messageKey(),
            params: self.params,
        };
        (self.status, Json(response)).into_response()
    }
}

impl IntoResponse for ApiError {
    /// 未经过请求提取器的错误使用英文基线，禁止由进程区域设置隐式改变协议。
    fn into_response(self) -> Response {
        self.intoLocalizedResponse(Locale::En)
    }
}

impl IntoResponse for LocalizedApiError {
    /// 使用处理器入口确定的语言序列化错误，禁止错误路径重新读取易变请求状态。
    fn into_response(self) -> Response {
        self.error.intoLocalizedResponse(self.locale)
    }
}
