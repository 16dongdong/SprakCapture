use std::{
    collections::{HashMap, VecDeque},
    future::IntoFuture,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, Path, Query, State, rejection::QueryRejection},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{any, get, post, put},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use hmac::{Hmac, Mac};
use parking_lot::RwLock;
use rand::{RngCore, rng};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};
use tower_http::services::{ServeDir, ServeFile};

use crate::{
    AccountDomainService, AccountQuery, AccountServiceError, AccountStore,
    BatchDeleteAccountsRequest, BatchDeleteRuleSetsRequest, BatchUpdateAccountsRequest,
    CreateAccountRequest, CreateRuleSetRequest, LeaseAuthenticationRequest,
    LeaseSynchronizationRequest, ManagementLoginRequest, Result, SetPasswordRequest,
    SetRuleSetEnabledRequest, UpdateAccountRequest, UpdateManagementIdentityRequest,
    UpdateRuleSetRequest, VerifyAccountCredentialsRequest, store::currentTimeMilliseconds,
};

const defaultAdministratorUsername: &str = "Admin";
const defaultAdministratorPassword: &str = "Admin123";
const persistentSessionCookieSeconds: i64 = 10 * 365 * 24 * 60 * 60;
const loginAttemptWindowMilliseconds: i64 = 60_000;
const maximumLoginAttemptsPerWindow: usize = 5;
const maximumTrackedLoginIdentities: usize = 4_096;
const localLoginTicketLifetimeMilliseconds: i64 = 30_000;
const maximumLocalLoginTickets: usize = 128;
const browserSessionContext: &[u8] = b"account-browser-session-v1";
type BrowserSessionMac = Hmac<Sha256>;

const maximumProxiedControlBodyBytes: usize = 64 * 1024 * 1024;
const maximumPublicPackageRequestBodyBytes: usize = 2 * 1024 * 1024;
const maximumBasicAuthorizationBytes: usize = 1024;
const maximumRootCertificateBytes: usize = 1024 * 1024;
const rootCertificateControlPath: &str = "/api/v1/ssl/ca/export?format=cer";

/// 账号服务运行配置由 SprakCapture 监督器构造，内部令牌只在进程内存和匿名管道中传递。
#[derive(Clone, Debug)]
pub struct AccountServerConfig {
    pub databasePath: PathBuf,
    pub publicAddress: SocketAddr,
    pub internalAddress: SocketAddr,
    pub internalToken: String,
    pub controlBaseUrl: String,
    pub webAssetsDirectory: Option<PathBuf>,
}

/// 保存已绑定端点和关闭句柄；停止会同时排空公共与内部监听。
pub struct RunningAccountService {
    pub publicAddress: SocketAddr,
    pub internalAddress: SocketAddr,
    pub serviceInstanceId: String,
    shutdownSender: watch::Sender<bool>,
    publicTask: JoinHandle<std::io::Result<()>>,
    internalTask: JoinHandle<std::io::Result<()>>,
}

impl RunningAccountService {
    /// 订阅内部关闭请求；独立进程主循环据此把 HTTP 关闭端点提升为完整进程退出。
    pub fn subscribeShutdown(&self) -> watch::Receiver<bool> {
        self.shutdownSender.subscribe()
    }

    /// 通知两个 HTTP 服务停止并等待任务退出；重复发送关闭标记保持幂等。
    pub async fn stop(self) -> Result<()> {
        self.shutdownSender.send_replace(true);
        self.publicTask.await.map_err(std::io::Error::other)??;
        self.internalTask.await.map_err(std::io::Error::other)??;
        Ok(())
    }
}

#[derive(Clone)]
struct HttpState {
    service: AccountDomainService,
    internalToken: Arc<str>,
    controlBaseUrl: Arc<str>,
    controlClient: reqwest::Client,
    localLoginTickets: Arc<RwLock<HashMap<String, i64>>>,
    loginAttempts: Arc<RwLock<HashMap<String, VecDeque<i64>>>>,
    shutdownSender: watch::Sender<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageQuery {
    #[serde(default)]
    offset: i64,
    #[serde(default = "defaultAccountPageLimit")]
    limit: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    serviceInstanceId: String,
    schemaVersion: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponse {
    authenticated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DisconnectResponse {
    revokedConnections: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalLoginQuery {
    ticket: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalLoginTicketResponse {
    path: String,
}

/// 内部概览响应只保留主工作台实际展示的聚合值，避免累计用量或在线 IP 统计被带入长期快照。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InternalStatisticsResponse {
    onlineAccounts: usize,
    activeConnections: usize,
    uploadBytesPerSecond: u64,
    downloadBytesPerSecond: u64,
}

/// 创建数据目录、打开 SQLite、初始化默认管理身份并绑定公共/内部两个独立端点。
pub async fn startAccountService(config: AccountServerConfig) -> Result<RunningAccountService> {
    if config.internalToken.len() < 32 {
        return Err(AccountServiceError::Validation(
            "内部接口令牌不能少于 32 个字符".to_owned(),
        ));
    }
    let controlBaseUrl = validateControlBaseUrl(&config.controlBaseUrl)?;
    if let Some(webAssetsDirectory) = config.webAssetsDirectory.as_deref() {
        validateWebAssetsDirectory(webAssetsDirectory)?;
    }
    let parentDirectory = config
        .databasePath
        .parent()
        .ok_or_else(|| AccountServiceError::Validation("数据库路径没有父目录".to_owned()))?;
    std::fs::create_dir_all(parentDirectory)?;
    let service = AccountDomainService::new(AccountStore::open(&config.databasePath)?);
    // 默认身份在账号服务首次启动时直接建立，用户不需要完成额外初始化向导。
    if !service.managementInitialized()? {
        service.bootstrapManagement(defaultAdministratorUsername, defaultAdministratorPassword)?;
    }
    let publicListener = TcpListener::bind(config.publicAddress).await?;
    let internalListener = TcpListener::bind(config.internalAddress).await?;
    if !internalListener.local_addr()?.ip().is_loopback() {
        return Err(AccountServiceError::Validation(
            "内部接口只允许绑定回环地址".to_owned(),
        ));
    }
    let publicAddress = publicListener.local_addr()?;
    let internalAddress = internalListener.local_addr()?;
    let serviceInstanceId = service.serviceInstanceId().to_owned();
    let (shutdownSender, shutdownReceiver) = watch::channel(false);
    let state = HttpState {
        service,
        internalToken: Arc::from(config.internalToken),
        controlBaseUrl: Arc::from(controlBaseUrl),
        controlClient: reqwest::Client::new(),
        localLoginTickets: Arc::new(RwLock::new(HashMap::new())),
        loginAttempts: Arc::new(RwLock::new(HashMap::new())),
        shutdownSender: shutdownSender.clone(),
    };
    let publicTask = tokio::spawn(
        axum::serve(
            publicListener,
            publicRouter(state.clone(), config.webAssetsDirectory),
        )
        .with_graceful_shutdown(waitForShutdown(shutdownReceiver.clone()))
        .into_future(),
    );
    let internalTask = tokio::spawn(
        axum::serve(internalListener, internalRouter(state))
            .with_graceful_shutdown(waitForShutdown(shutdownReceiver))
            .into_future(),
    );
    Ok(RunningAccountService {
        publicAddress,
        internalAddress,
        serviceInstanceId,
        shutdownSender,
        publicTask,
        internalTask,
    })
}

/// 组合唯一远程入口：根路径承载 Sprak Capture，账号管理映射到固定子路径，控制 API 在统一认证后转发。
///
/// 运行上下文：仅公共监听调用；`webAssetsDirectory` 必须是本次安装随附的构建目录。
/// 失败语义：静态资源由 `ServeDir` 返回明确 404，受保护 API 的认证或上游错误不会回退到 SPA 页面。
fn publicRouter(state: HttpState, webAssetsDirectory: Option<PathBuf>) -> Router {
    let router = Router::new()
        .route("/client", get(clientDownloadPage))
        .route("/client/", get(clientDownloadPage))
        .route("/client/styles.css", get(clientDownloadStylesheet))
        .route("/client/app.js", get(clientDownloadScript))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/session", get(session))
        .route(
            "/api/v1/clientPackages/download",
            post(proxyClientPackageDownload),
        )
        .route("/api/v1/client/routing.txt", get(downloadClientRuleSet))
        .route("/api/v1/client/ca.cer", get(downloadClientRootCertificate))
        .route("/api/v1/{*controlPath}", any(proxyControlRequest))
        .nest("/account-management", accountManagementRouter());
    match webAssetsDirectory {
        Some(directory) => {
            let indexFile = directory.join("index.html");
            router
                .fallback_service(ServeDir::new(directory).fallback(ServeFile::new(indexFile)))
                .with_state(state)
        }
        None => router
            .fallback(get(remoteWebAssetsUnavailable))
            .with_state(state),
    }
}

/// 在仅领域测试或未安装 Web 资源的运行环境返回明确错误，账号管理子路径仍可独立验证。
///
/// 运行上下文：仅当父进程未提供资源目录且请求没有命中 API/账号管理路由时执行。
/// 失败语义：固定返回 503，不回退到账号页面或伪造主工作台可用。
async fn remoteWebAssetsUnavailable() -> (StatusCode, &'static str) {
    (StatusCode::SERVICE_UNAVAILABLE, "远程 Web 资源未安装。")
}

/// 构造账号管理子路由；所有业务路径固定在 `/account-management` 下，避免与主控制 API 形成重复入口。
///
/// 运行上下文：父路由通过 `nest` 去除前缀后进入本路由，静态资源和 API 均使用相对路径。
/// 失败语义：未授权请求由现有 Cookie/Bearer 校验精确拒绝，不会转发到主控制服务。
fn accountManagementRouter() -> Router<HttpState> {
    Router::new()
        .route("/", get(indexPage))
        .route("/styles.css", get(stylesheet))
        .route("/app.js", get(applicationScript))
        .route("/api/v1/health", get(publicHealth))
        .route("/api/v1/openapi.json", get(openApi))
        // 账号页面使用自身子路径完成直接登录和持久会话探测；只保留主站认证路由会让
        // 相对 API 落入 SPA fallback，页面刷新后看似凭证失效。
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/session", get(session))
        .route("/api/v1/auth/local", get(localLogin))
        .route(
            "/api/v1/management/identity",
            get(getManagementIdentity).put(updateManagementIdentity),
        )
        .route("/api/v1/management/apiKey", get(getApiKey))
        .route("/api/v1/accounts", get(listAccounts).post(createAccount))
        .route("/api/v1/ruleSets", get(listRuleSets).post(createRuleSet))
        .route(
            "/api/v1/ruleSets/batch",
            axum::routing::delete(deleteRuleSetsBatch),
        )
        .route(
            "/api/v1/ruleSets/{ruleSetId}",
            get(getRuleSet).put(updateRuleSet).delete(deleteRuleSet),
        )
        .route(
            "/api/v1/ruleSets/{ruleSetId}/enabled",
            put(setRuleSetEnabled),
        )
        .route(
            "/api/v1/accounts/batch",
            axum::routing::patch(updateAccountsBatch).delete(deleteAccountsBatch),
        )
        .route(
            "/api/v1/accounts/{accountId}",
            get(getAccount).patch(updateAccount).delete(deleteAccount),
        )
        .route(
            "/api/v1/accounts/{accountId}/password",
            put(setAccountPassword).delete(clearAccountPassword),
        )
        .route(
            "/api/v1/accounts/{accountId}/connections",
            get(accountConnections),
        )
        .route("/api/v1/accounts/{accountId}/usage", get(accountUsage))
        .route(
            "/api/v1/accounts/{accountId}/disconnect",
            post(disconnectAccount),
        )
        .route("/api/v1/connections", get(allConnections))
        .route("/api/v1/statistics", get(statistics))
        .route("/api/v1/auditLogs", get(listAuditLogs))
}

/// 校验主控制地址只指向回环 HTTP 服务，避免远程入口被配置成任意上游代理。
///
/// 运行上下文：账号服务绑定监听前调用一次；`configuredUrl` 来自受信父进程握手。
/// 失败语义：非 HTTP、带凭据/查询/片段、非回环主机或非根路径均返回配置校验错误并终止启动。
fn validateControlBaseUrl(configuredUrl: &str) -> Result<String> {
    let parsedUrl = reqwest::Url::parse(configuredUrl)
        .map_err(|error| AccountServiceError::Validation(format!("控制服务地址无效：{error}")))?;
    let loopbackHost = parsedUrl
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|host| host.is_loopback());
    if parsedUrl.scheme() != "http"
        || !loopbackHost
        || parsedUrl.port().is_none()
        || !parsedUrl.username().is_empty()
        || parsedUrl.password().is_some()
        || parsedUrl.path() != "/"
        || parsedUrl.query().is_some()
        || parsedUrl.fragment().is_some()
    {
        return Err(AccountServiceError::Validation(
            "控制服务地址必须是带端口的回环 HTTP 根地址".to_owned(),
        ));
    }
    Ok(parsedUrl.as_str().trim_end_matches('/').to_owned())
}

/// 校验远程 Web 资源目录包含入口文件；缺失构建产物时拒绝启动而不是返回空白页面。
///
/// 运行上下文：公共监听绑定前调用，目录来自桌面资源目录或开发环境显式覆盖。
/// 失败语义：路径不是目录或缺少 `index.html` 时返回校验错误，监督器会把原因投影到设置页。
fn validateWebAssetsDirectory(webAssetsDirectory: &std::path::Path) -> Result<()> {
    if webAssetsDirectory.is_dir() && webAssetsDirectory.join("index.html").is_file() {
        return Ok(());
    }
    Err(AccountServiceError::Validation(format!(
        "远程 Web 资源目录无效：{}",
        webAssetsDirectory.display()
    )))
}

/// 在统一管理员会话校验后把主控制 API 转发到本机控制监听，响应体保持流式传输以兼容 SSE 和大文件。
///
/// 运行上下文：仅匹配远程入口的 `/api/v1/*`，登录/退出/会话路由由更精确的本地处理器优先处理。
/// 参数：`originalUri` 保留查询字符串，`request` 携带原方法、头和正文；控制地址来自启动时已校验状态。
/// 失败语义：认证失败直接返回 401；上游连接、响应头或流错误返回明确服务错误，不伪造控制响应。
async fn proxyControlRequest(
    State(state): State<HttpState>,
    OriginalUri(originalUri): OriginalUri,
    request: Request<Body>,
) -> Result<Response> {
    authorizePublic(&state, request.headers())?;
    forwardControlRequest(&state, originalUri, request, maximumProxiedControlBodyBytes).await
}

/// 转发公开客户端下载请求；权威 SOCKS5 凭据校验由控制服务通过内部账号接口完成。
///
/// 运行上下文：路由只绑定精确 POST `/api/v1/clientPackages/download`，因此这里不接受任意免登录
/// 控制路径。请求正文中的密码不写日志、不缓存，响应保持流式以避免 APK 常驻账号服务内存。
async fn proxyClientPackageDownload(
    State(state): State<HttpState>,
    OriginalUri(originalUri): OriginalUri,
    request: Request<Body>,
) -> Result<Response> {
    forwardControlRequest(
        &state,
        originalUri,
        request,
        maximumPublicPackageRequestBodyBytes,
    )
    .await
}

/// 把已完成入口级授权的请求转发到回环控制服务；调用方必须先限定路径、认证策略和正文上限。
/// 管理控制面保留大文件边界，公开打包入口使用 2 MiB 上限容纳有界 Base64 图标并阻止无界请求。
async fn forwardControlRequest(
    state: &HttpState,
    originalUri: axum::http::Uri,
    request: Request<Body>,
    maximumRequestBodyBytes: usize,
) -> Result<Response> {
    let upstreamUrl = format!("{}{}", state.controlBaseUrl, originalUri);
    let (requestParts, requestBody) = request.into_parts();
    let requestBytes = axum::body::to_bytes(requestBody, maximumRequestBodyBytes)
        .await
        .map_err(|error| {
            AccountServiceError::Validation(format!("读取远程控制请求正文失败：{error}"))
        })?;
    let mut upstreamRequest = state
        .controlClient
        .request(requestParts.method, upstreamUrl)
        .body(requestBytes);
    for (headerName, headerValue) in &requestParts.headers {
        if shouldForwardHeader(headerName) {
            upstreamRequest = upstreamRequest.header(headerName, headerValue);
        }
    }
    let upstreamResponse = upstreamRequest.send().await.map_err(|error| {
        AccountServiceError::Io(std::io::Error::other(format!(
            "连接本机控制服务失败：{error}"
        )))
    })?;
    let status = upstreamResponse.status();
    let upstreamHeaders = upstreamResponse.headers().clone();
    let responseBody = Body::from_stream(upstreamResponse.bytes_stream());
    let mut response = Response::builder().status(status);
    for (headerName, headerValue) in &upstreamHeaders {
        if shouldForwardHeader(headerName) {
            response = response.header(headerName, headerValue);
        }
    }
    response.body(responseBody).map_err(|error| {
        AccountServiceError::Io(std::io::Error::other(format!(
            "构造远程控制响应失败：{error}"
        )))
    })
}

/// 判定端到端头是否可跨越内部代理边界；连接级头由各自 HTTP 栈重新生成。
///
/// 运行上下文：请求和响应转发共用，参数为待复制头名。
/// 失败语义：返回 false 的头被明确丢弃，不影响业务正文与端到端缓存/范围语义。
fn shouldForwardHeader(headerName: &header::HeaderName) -> bool {
    !matches!(
        headerName.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "cookie"
            | "origin"
            | "referer"
    )
}

/// 内部路由只绑定回环端点，并在每个处理器进入领域层前校验进程级令牌。
fn internalRouter(state: HttpState) -> Router {
    Router::new()
        .route("/internal/v1/health", get(internalHealth))
        .route("/internal/v1/management/bootstrap", post(internalBootstrap))
        .route(
            "/internal/v1/management/identity",
            get(internalManagementIdentity).put(internalUpdateManagementIdentity),
        )
        .route("/internal/v1/management/apiKey", get(internalGetApiKey))
        .route(
            "/internal/v1/management/session",
            post(internalCreateManagementSession),
        )
        .route("/internal/v1/statistics", get(internalStatistics))
        .route(
            "/internal/v1/leases/authenticate",
            post(internalAuthenticateLease),
        )
        .route(
            "/internal/v1/leases/synchronize",
            post(internalSynchronizeLeases),
        )
        .route(
            "/internal/v1/leases/release",
            post(internalSynchronizeLeases),
        )
        .route(
            "/internal/v1/accounts/verify",
            post(internalVerifyAccountCredentials),
        )
        .route("/internal/v1/ruleSets/active", get(internalActiveRuleSet))
        .route("/internal/v1/shutdown", post(internalShutdown))
        .with_state(state)
}

/// 返回嵌入式管理页面；HTML 只保留结构，样式和交互由同版本独立资源加载。
async fn indexPage() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

/// 返回独立样式表并固定 CSS 媒体类型；资源随二进制嵌入，不依赖运行目录。
async fn stylesheet() -> Response {
    staticAsset("text/css; charset=utf-8", include_str!("../web/styles.css"))
}

/// 返回独立应用脚本并固定 JavaScript 媒体类型；加载失败由浏览器明确报告而不是混入 HTML。
async fn applicationScript() -> Response {
    staticAsset(
        "text/javascript; charset=utf-8",
        include_str!("../web/app.js"),
    )
}

/// 返回无需管理登录的客户端下载页；页面只收集本次 SOCKS5 凭据并立即发起流式打包下载。
async fn clientDownloadPage() -> Html<&'static str> {
    Html(include_str!("../clientWeb/index.html"))
}

/// 返回客户端下载页独立样式；静态嵌入确保安装环境不依赖额外前端构建工具。
async fn clientDownloadStylesheet() -> Response {
    staticAsset(
        "text/css; charset=utf-8",
        include_str!("../clientWeb/styles.css"),
    )
}

/// 返回客户端下载页独立脚本；账号密码只停留在表单与当次请求正文，不进入 URL 或存储。
async fn clientDownloadScript() -> Response {
    staticAsset(
        "text/javascript; charset=utf-8",
        include_str!("../clientWeb/app.js"),
    )
}

/// 构造不可嗅探的嵌入式文本资源响应；非法固定媒体类型属于构建期缺陷。
fn staticAsset(contentType: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(contentType)),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        body,
    )
        .into_response()
}

/// 公共健康检查不暴露数据库路径和内部端点。
async fn publicHealth(State(state): State<HttpState>) -> Result<Json<HealthResponse>> {
    Ok(Json(healthResponse(&state)?))
}

/// 内部健康检查要求进程级令牌并返回实例标识。
async fn internalHealth(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<HealthResponse>> {
    authorizeInternal(&state, &headers)?;
    Ok(Json(healthResponse(&state)?))
}

/// 管理员登录成功后生成随机 HttpOnly 会话 Cookie；失败不区分账号和密码字段。
async fn login(
    State(state): State<HttpState>,
    Json(request): Json<ManagementLoginRequest>,
) -> Result<Response> {
    enforceLoginRateLimit(&state, &request.username)?;
    let credentialRevision = state
        .service
        .authenticateManagement(&request.username, &request.password)?;
    state.loginAttempts.write().remove(&request.username);
    let sessionId = createBrowserSession(&state, credentialRevision)?;
    let mut response = Json(LoginResponse {
        authenticated: true,
    })
    .into_response();
    setSessionCookie(&mut response, &sessionId)?;
    Ok(response)
}

/// 消费控制面签发的一次性票据并建立浏览器会话；票据单次有效，成功后立即从 URL 跳回首页。
async fn localLogin(
    State(state): State<HttpState>,
    Query(query): Query<LocalLoginQuery>,
) -> Result<Response> {
    let now = currentTimeMilliseconds();
    let mut tickets = state.localLoginTickets.write();
    tickets.retain(|_, expiresAt| *expiresAt >= now);
    let expiresAt = tickets
        .remove(&query.ticket)
        .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
    if expiresAt < now {
        return Err(AccountServiceError::ManagementAuthenticationFailed);
    }
    drop(tickets);
    let credentialRevision = state.service.managementIdentity()?.credentialRevision;
    let sessionId = createBrowserSession(&state, credentialRevision)?;
    // Axum 的嵌套路由根精确匹配无尾斜杠路径；跳到带斜杠地址会落入主站 SPA fallback，
    // 从而在账号 iframe 中递归加载 Sprak Web。账号文档通过 base 元素固定后续资源前缀。
    let mut response = Redirect::to("/account-management").into_response();
    setSessionCookie(&mut response, &sessionId)?;
    Ok(response)
}

/// 验证当前 Cookie 后让浏览器立即清除它；无状态签名会话无需维护服务端删除表。
async fn logout(State(state): State<HttpState>, headers: HeaderMap) -> Result<Response> {
    authorizePublic(&state, &headers)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("account_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    Ok(response)
}

/// 验证当前远程管理会话。
async fn session(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<LoginResponse>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(LoginResponse {
        authenticated: true,
    }))
}

/// 返回脱敏管理身份。
async fn getManagementIdentity(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<crate::ManagementIdentityView>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.managementIdentity()?))
}

/// 修改管理身份后清空全部浏览器会话，当前响应仍返回新 Key，后续请求必须重新登录。
async fn updateManagementIdentity(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<UpdateManagementIdentityRequest>,
) -> Result<Json<crate::ApiKeyResponse>> {
    authorizePublic(&state, &headers)?;
    let response = state
        .service
        .updateManagementIdentity(&request.username, &request.password)?;
    // 凭据修订号进入签名 Cookie；更新后旧 Cookie 会自然校验失败，无需保存易失撤销表。
    // 尚未消费的控制面入口必须显式撤销，避免旧授权动作在新身份生效后补建新会话。
    state.localLoginTickets.write().clear();
    Ok(Json(response))
}

/// 当前浏览器会话已经完成授权，直接恢复确定性 Key；请求不再接收或重复处理密码。
async fn getApiKey(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<crate::ApiKeyResponse>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.managementApiKey()?))
}

/// 返回账号页并组合实时连接数和持久化流量。
async fn listAccounts(
    State(state): State<HttpState>,
    headers: HeaderMap,
    query: std::result::Result<Query<AccountQuery>, QueryRejection>,
) -> Result<Json<Vec<crate::AccountView>>> {
    authorizePublic(&state, &headers)?;
    let Query(query) = query.map_err(|error| {
        AccountServiceError::Validation(format!("账号查询参数无效：{}", error.body_text()))
    })?;
    Ok(Json(state.service.queryAccounts(&query)?))
}

/// 创建账号；password=null 与固定密码语义在领域层严格区分。
async fn createAccount(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<CreateAccountRequest>,
) -> Result<(StatusCode, Json<crate::AccountView>)> {
    authorizePublic(&state, &headers)?;
    Ok((
        StatusCode::CREATED,
        Json(state.service.createAccount(&request).await?),
    ))
}

/// 返回单账号详情。
async fn getAccount(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(accountId): Path<String>,
) -> Result<Json<crate::AccountView>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.account(&accountId)?))
}

/// 使用 policyRevision 防止远程管理页面覆盖并发修改。
async fn updateAccount(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(accountId): Path<String>,
    Json(request): Json<UpdateAccountRequest>,
) -> Result<Json<crate::AccountView>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(
        state.service.updateAccount(&accountId, &request).await?,
    ))
}

/// 原子批量更新选中账号；加时和策略字段由服务端按各账号当前修订统一提交。
async fn updateAccountsBatch(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<BatchUpdateAccountsRequest>,
) -> Result<Json<crate::BatchUpdateAccountsResponse>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.updateAccountsBatch(&request).await?))
}

/// 原子删除选中账号；任一账号不存在或修订已变化时整批拒绝。
async fn deleteAccountsBatch(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<BatchDeleteAccountsRequest>,
) -> Result<Json<crate::BatchDeleteAccountsResponse>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.deleteAccountsBatch(&request).await?))
}

/// 删除账号并撤销现有租约。
async fn deleteAccount(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(accountId): Path<String>,
) -> Result<StatusCode> {
    authorizePublic(&state, &headers)?;
    state.service.deleteAccount(&accountId).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 返回全部规则集管理视图；正文仅对已授权管理会话或自动化 Key 可见。
async fn listRuleSets(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::RuleSetView>>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.listRuleSets()?))
}

/// 创建并可选启用规则集；routing.txt 语法和单启用不变量由存储事务统一校验。
async fn createRuleSet(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<CreateRuleSetRequest>,
) -> Result<(StatusCode, Json<crate::RuleSetView>)> {
    authorizePublic(&state, &headers)?;
    Ok((
        StatusCode::CREATED,
        Json(state.service.createRuleSet(&request)?),
    ))
}

/// 返回单个规则集最新正文与修订号，供编辑冲突后重新加载。
async fn getRuleSet(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(ruleSetId): Path<String>,
) -> Result<Json<crate::RuleSetView>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.ruleSet(&ruleSetId)?))
}

/// 保存规则集名称和完整正文；revision 不匹配时返回 409 并保持现有配置。
async fn updateRuleSet(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(ruleSetId): Path<String>,
    Json(request): Json<UpdateRuleSetRequest>,
) -> Result<Json<crate::RuleSetView>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.updateRuleSet(&ruleSetId, &request)?))
}

/// 设置规则集互斥开关；开启新项时旧启用项在同一 SQLite 事务内关闭。
async fn setRuleSetEnabled(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(ruleSetId): Path<String>,
    Json(request): Json<SetRuleSetEnabledRequest>,
) -> Result<Json<crate::RuleSetView>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.setRuleSetEnabled(&ruleSetId, &request)?))
}

/// 删除单个规则集；当前启用项被删后客户端会收到明确 404，禁止使用过期服务端配置。
async fn deleteRuleSet(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(ruleSetId): Path<String>,
) -> Result<StatusCode> {
    authorizePublic(&state, &headers)?;
    state.service.deleteRuleSet(&ruleSetId)?;
    Ok(StatusCode::NO_CONTENT)
}

/// 原子删除管理页多选规则集；请求 ID 重复、缺失或不存在都会在写入前拒绝。
async fn deleteRuleSetsBatch(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<BatchDeleteRuleSetsRequest>,
) -> Result<Json<crate::BatchDeleteRuleSetsResponse>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.deleteRuleSetsBatch(&request)?))
}

/// 校验 HTTP Basic SOCKS5 凭据并下载当前唯一启用规则；支持 ETag 条件请求和原子客户端缓存。
async fn downloadClientRuleSet(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Response> {
    let (username, password) = basicAccountCredentials(&headers)?;
    state
        .service
        .verifyAccountCredentials(&username, &password)
        .await?;
    let ruleSet = state.service.activeRuleSet()?;
    let etag = format!("\"{}-{}\"", ruleSet.ruleSetId, ruleSet.revision);
    let notModified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|provided| provided.split(',').any(|value| value.trim() == etag));
    let mut response = Response::builder()
        .status(if notModified {
            StatusCode::NOT_MODIFIED
        } else {
            StatusCode::OK
        })
        .header(header::ETAG, &etag)
        .header("x-rule-set-id", &ruleSet.ruleSetId)
        .header("x-rule-set-revision", ruleSet.revision.to_string())
        .header(header::CACHE_CONTROL, "private, no-cache");
    if !notModified {
        response = response
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    }
    response
        .body(if notModified {
            Body::empty()
        } else {
            Body::from(ruleSet.content)
        })
        .map_err(|error| {
            AccountServiceError::Io(std::io::Error::other(format!(
                "构造规则集下载响应失败：{error}"
            )))
        })
}

/// 校验与 SOCKS5 相同的账号凭据并返回当前代理根证书。
///
/// 运行上下文：Android 客户端只在用户开启“证书信任”且 Root 可用时，经自身 SOCKS5 节点访问本端点；
/// 控制服务仍只监听回环，证书私钥和本机路径不会跨越账号服务边界。
/// 失败语义：无效、停用或过期账号返回 401；控制服务异常、非 200 或超限证书返回稳定内部错误，
/// 客户端不得把旧证书当成本次安装成功。
async fn downloadClientRootCertificate(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Response> {
    let (username, password) = basicAccountCredentials(&headers)?;
    state
        .service
        .verifyAccountCredentials(&username, &password)
        .await?;
    let upstreamUrl = format!("{}{}", state.controlBaseUrl, rootCertificateControlPath);
    let upstream = state
        .controlClient
        .get(upstreamUrl)
        .send()
        .await
        .map_err(controlCertificateError)?;
    if upstream.status() != reqwest::StatusCode::OK {
        return Err(controlCertificateError("控制服务未返回根证书"));
    }
    if upstream
        .content_length()
        .is_some_and(|length| length > maximumRootCertificateBytes as u64)
    {
        return Err(controlCertificateError("根证书超过大小上限"));
    }
    let certificate = upstream.bytes().await.map_err(controlCertificateError)?;
    if certificate.is_empty() || certificate.len() > maximumRootCertificateBytes {
        return Err(controlCertificateError("根证书长度无效"));
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/pkix-cert")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(certificate))
        .map_err(|error| controlCertificateError(error.to_string()))
}

/// 把证书转发的底层诊断收敛为账号服务内部错误；公开响应不包含控制地址或本机路径。
fn controlCertificateError(error: impl std::fmt::Display) -> AccountServiceError {
    AccountServiceError::Io(std::io::Error::other(format!(
        "读取控制服务根证书失败：{error}"
    )))
}

/// 设置固定密码并撤销用旧密码建立的连接。
async fn setAccountPassword(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(accountId): Path<String>,
    Json(request): Json<SetPasswordRequest>,
) -> Result<StatusCode> {
    authorizePublic(&state, &headers)?;
    state
        .service
        .setAccountPassword(&accountId, &request)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 清除固定密码后只接受任意非空密码。
async fn clearAccountPassword(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(accountId): Path<String>,
) -> Result<StatusCode> {
    authorizePublic(&state, &headers)?;
    state.service.clearAccountPassword(&accountId).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 返回指定账号的活动连接。
async fn accountConnections(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(accountId): Path<String>,
) -> Result<Json<Vec<crate::ConnectionView>>> {
    authorizePublic(&state, &headers)?;
    state.service.account(&accountId)?;
    Ok(Json(state.service.connections(Some(&accountId))))
}

/// 返回账号累计与每日流量；账号服务只包含最近一次已确认的租约增量。
async fn accountUsage(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(accountId): Path<String>,
) -> Result<Json<crate::AccountUsageView>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.accountUsage(&accountId)?))
}

/// 强制下线全部连接但不禁用账号。
async fn disconnectAccount(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(accountId): Path<String>,
) -> Result<Json<DisconnectResponse>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(DisconnectResponse {
        revokedConnections: state.service.disconnectAccount(&accountId)?,
    }))
}

/// 返回全部活动连接，目标地址仍由 SprakCapture 事务工作台展示。
async fn allConnections(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::ConnectionView>>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.connections(None)))
}

/// 返回管理首页聚合统计。
async fn statistics(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<crate::AccountStatistics>> {
    authorizePublic(&state, &headers)?;
    Ok(Json(state.service.statistics()?))
}

/// 返回脱敏审计日志，使用与账号列表一致的有界分页契约。
async fn listAuditLogs(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<Vec<crate::AuditLogView>>> {
    authorizePublic(&state, &headers)?;
    validatePage(&query)?;
    Ok(Json(
        state.service.listAuditLogs(query.offset, query.limit)?,
    ))
}

/// 内部 bootstrap 幂等创建默认管理身份并返回可重新派生的当前 Key。
async fn internalBootstrap(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<crate::ApiKeyResponse>> {
    authorizeInternal(&state, &headers)?;
    Ok(Json(state.service.bootstrapManagement(
        defaultAdministratorUsername,
        defaultAdministratorPassword,
    )?))
}

/// SprakCapture 监督器读取脱敏身份以恢复 Key 指纹和生成时间，不需要管理密码且不返回完整 Key。
async fn internalManagementIdentity(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<crate::ManagementIdentityView>> {
    authorizeInternal(&state, &headers)?;
    Ok(Json(state.service.managementIdentity()?))
}

/// SprakCapture 设置页通过内部接口更新身份，成功后同时撤销远程浏览器会话。
async fn internalUpdateManagementIdentity(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<UpdateManagementIdentityRequest>,
) -> Result<Json<crate::ApiKeyResponse>> {
    authorizeInternal(&state, &headers)?;
    let response = state
        .service
        .updateManagementIdentity(&request.username, &request.password)?;
    // 内部更新与公共更新保持同一修订边界，旧签名 Cookie 和未消费票据均不能跨越管理身份变更。
    state.localLoginTickets.write().clear();
    Ok(Json(response))
}

/// SprakCapture 控制面凭内部令牌恢复当前完整 Key；请求正文为空，响应不得进入状态快照。
async fn internalGetApiKey(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<crate::ApiKeyResponse>> {
    authorizeInternal(&state, &headers)?;
    Ok(Json(state.service.managementApiKey()?))
}

/// 为 SprakCapture 本机概览签发短期一次性入口；票据只建立浏览器 Cookie，不授予 API Bearer 权限。
async fn internalCreateManagementSession(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<LocalLoginTicketResponse>> {
    authorizeInternal(&state, &headers)?;
    let now = currentTimeMilliseconds();
    let ticket = randomToken(32);
    let mut tickets = state.localLoginTickets.write();
    tickets.retain(|_, expiresAt| *expiresAt >= now);
    if tickets.len() >= maximumLocalLoginTickets {
        return Err(AccountServiceError::RateLimited);
    }
    tickets.insert(
        ticket.clone(),
        now.saturating_add(localLoginTicketLifetimeMilliseconds),
    );
    Ok(Json(LocalLoginTicketResponse {
        path: format!("/api/v1/auth/local?ticket={ticket}"),
    }))
}

/// 向主控制面提供严格脱敏的在线与实时速率快照；该接口不返回账号、IP、连接标识或累计流量。
async fn internalStatistics(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<InternalStatisticsResponse>> {
    authorizeInternal(&state, &headers)?;
    let statistics = state.service.statistics()?;
    Ok(Json(InternalStatisticsResponse {
        onlineAccounts: statistics.onlineAccounts,
        activeConnections: statistics.activeConnections,
        uploadBytesPerSecond: statistics.uploadBytesPerSecond,
        downloadBytesPerSecond: statistics.downloadBytesPerSecond,
    }))
}

/// SOCKS5 数据面认证并原子申请连接租约。
async fn internalAuthenticateLease(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<LeaseAuthenticationRequest>,
) -> Result<Json<crate::LeaseAuthenticationResponse>> {
    authorizeInternal(&state, &headers)?;
    Ok(Json(state.service.authenticateLease(&request).await?))
}

/// 同步与 release 共用 final 字段协议，重复批次不会重复累计流量。
async fn internalSynchronizeLeases(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<LeaseSynchronizationRequest>,
) -> Result<Json<crate::LeaseSynchronizationResponse>> {
    authorizeInternal(&state, &headers)?;
    Ok(Json(state.service.synchronizeLeases(&request).await?))
}

/// 使用内部令牌保护的无租约账号校验；打包器授权不得制造在线连接或消耗连接/IP 配额。
async fn internalVerifyAccountCredentials(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(request): Json<VerifyAccountCredentialsRequest>,
) -> Result<StatusCode> {
    authorizeInternal(&state, &headers)?;
    state
        .service
        .verifyAccountCredentials(&request.username, &request.password)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 返回打包前置检查所需的当前启用规则元数据；正文只通过客户端认证下载端点传输。
async fn internalActiveRuleSet(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<Json<crate::ActiveRuleSetMetadata>> {
    authorizeInternal(&state, &headers)?;
    Ok(Json(state.service.activeRuleSetMetadata()?))
}

/// 通过受保护内部接口通知两个监听器有序停止；父进程仍负责等待子进程退出并确认状态码。
async fn internalShutdown(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    authorizeInternal(&state, &headers)?;
    state.shutdownSender.send_replace(true);
    Ok(StatusCode::NO_CONTENT)
}

/// 管理页面支持持久签名 Cookie，自动化调用支持 Bearer Key；两种身份不共享凭据材料。
fn authorizePublic(state: &HttpState, headers: &HeaderMap) -> Result<()> {
    if let Some(apiKey) = bearerToken(headers) {
        return state.service.authenticateApiKey(apiKey);
    }
    let sessionId =
        sessionIdFromHeaders(headers).ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
    verifyBrowserSession(state, &sessionId)
}

/// 内部接口只接受匿名管道传入的进程级令牌，使用恒定时间比较避免短路差异。
fn authorizeInternal(state: &HttpState, headers: &HeaderMap) -> Result<()> {
    let provided = headers
        .get("x-account-service-token")
        .and_then(|value| value.to_str().ok())
        .ok_or(AccountServiceError::InternalAuthenticationFailed)?;
    if provided.len() != state.internalToken.len()
        || !bool::from(provided.as_bytes().ct_eq(state.internalToken.as_bytes()))
    {
        return Err(AccountServiceError::InternalAuthenticationFailed);
    }
    Ok(())
}

/// 从 Authorization 读取 Bearer Token，不接受 URL 或 Cookie 中的自动化 Key。
fn bearerToken(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// 解析规则下载使用的 HTTP Basic 凭据；格式或 UTF-8 无效统一折叠为 SOCKS5 认证失败。
fn basicAccountCredentials(headers: &HeaderMap) -> Result<(String, String)> {
    let encoded = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .ok_or(AccountServiceError::SocksAuthenticationFailed)?;
    if encoded.len() > maximumBasicAuthorizationBytes {
        return Err(AccountServiceError::SocksAuthenticationFailed);
    }
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| AccountServiceError::SocksAuthenticationFailed)?;
    let credentials =
        String::from_utf8(decoded).map_err(|_| AccountServiceError::SocksAuthenticationFailed)?;
    let (username, password) = credentials
        .split_once(':')
        .ok_or(AccountServiceError::SocksAuthenticationFailed)?;
    if username.is_empty() || password.is_empty() {
        return Err(AccountServiceError::SocksAuthenticationFailed);
    }
    Ok((username.to_owned(), password.to_owned()))
}

/// 只解析本服务的同源 Cookie，不接受其它同名字段的部分匹配。
fn sessionIdFromHeaders(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.strip_prefix("account_session="))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 创建不进入日志的随机会话标识。
fn randomToken(byteCount: usize) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let mut bytes = vec![0_u8; byteCount];
    rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 创建带管理身份修订号的签名会话；服务重启后仍能验证，修改管理员凭据会使旧会话立即失效。
fn createBrowserSession(state: &HttpState, credentialRevision: i64) -> Result<String> {
    let nonce = randomToken(24);
    let payload = format!("{credentialRevision}.{nonce}");
    let (signingMaterial, currentRevision) = state.service.browserSessionMaterial()?;
    if credentialRevision != currentRevision {
        return Err(AccountServiceError::ManagementAuthenticationFailed);
    }
    let signature = signBrowserSession(signingMaterial.as_bytes(), payload.as_bytes())?;
    Ok(format!("{payload}.{signature}"))
}

/// 校验签名和当前身份修订号；格式、签名或修订不匹配均返回统一认证失败。
fn verifyBrowserSession(state: &HttpState, sessionId: &str) -> Result<()> {
    let (payload, providedSignature) = sessionId
        .rsplit_once('.')
        .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
    let (revision, nonce) = payload
        .split_once('.')
        .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
    let credentialRevision = revision
        .parse::<i64>()
        .map_err(|_| AccountServiceError::ManagementAuthenticationFailed)?;
    let (signingMaterial, currentRevision) = state.service.browserSessionMaterial()?;
    if nonce.is_empty() || credentialRevision != currentRevision {
        return Err(AccountServiceError::ManagementAuthenticationFailed);
    }
    let expectedSignature = signBrowserSession(signingMaterial.as_bytes(), payload.as_bytes())?;
    if providedSignature.len() != expectedSignature.len()
        || !bool::from(
            providedSignature
                .as_bytes()
                .ct_eq(expectedSignature.as_bytes()),
        )
    {
        return Err(AccountServiceError::ManagementAuthenticationFailed);
    }
    Ok(())
}

/// 以管理凭据派生材料签名浏览器会话；材料只存在于当前调用栈，构造失败返回凭据错误。
fn signBrowserSession(signingKey: &[u8], payload: &[u8]) -> Result<String> {
    let mut mac = BrowserSessionMac::new_from_slice(signingKey)
        .map_err(|_| AccountServiceError::Credential)?;
    mac.update(browserSessionContext);
    mac.update(payload);
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// 把会话写入十年期严格同站 HttpOnly Cookie；用户主动退出或凭据修订变化才结束授权。
fn setSessionCookie(response: &mut Response, sessionId: &str) -> Result<()> {
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "account_session={sessionId}; HttpOnly; SameSite=Strict; Path=/; Max-Age={persistentSessionCookieSeconds}"
        ))
        .map_err(|_| AccountServiceError::Credential)?,
    );
    Ok(())
}

/// 对同一管理用户名实施固定窗口限速，避免远程页面把昂贵密码校验变成无界 CPU 工作负载。
fn enforceLoginRateLimit(state: &HttpState, username: &str) -> Result<()> {
    let now = currentTimeMilliseconds();
    let mut attemptsByUsername = state.loginAttempts.write();
    attemptsByUsername.retain(|_, attempts| {
        attempts
            .back()
            .is_some_and(|attempt| now.saturating_sub(*attempt) <= loginAttemptWindowMilliseconds)
    });
    if !attemptsByUsername.contains_key(username)
        && attemptsByUsername.len() >= maximumTrackedLoginIdentities
    {
        return Err(AccountServiceError::RateLimited);
    }
    let attempts = attemptsByUsername.entry(username.to_owned()).or_default();
    while attempts
        .front()
        .is_some_and(|attempt| now.saturating_sub(*attempt) > loginAttemptWindowMilliseconds)
    {
        attempts.pop_front();
    }
    if attempts.len() >= maximumLoginAttemptsPerWindow {
        return Err(AccountServiceError::RateLimited);
    }
    attempts.push_back(now);
    Ok(())
}

/// 构造公共和内部共用的健康响应。
fn healthResponse(state: &HttpState) -> Result<HealthResponse> {
    Ok(HealthResponse {
        status: "ok",
        serviceInstanceId: state.service.serviceInstanceId().to_owned(),
        schemaVersion: state.service.schemaVersion()?,
    })
}

/// 返回账号管理子路径的 OpenAPI 3.1 操作和规则正文协议清单。
///
/// 运行上下文：管理页和自动化客户端从 `/account-management` 读取本文档。
/// 修复理由：规则正文新增必需 DNS 协议，因此在单一 Schema 中明确 IP 字面量、唯一键和
/// 四个必需段，避免 API 调用方继续提交旧格式。公共规则和 APK 下载不混入管理会话路径。
/// 失败语义：该处只构造静态 JSON，不读取状态也不产生运行时错误。
async fn openApi() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "SprakCapture SOCKS5 Account Service",
            "version": "1.0.0",
            "description": "管理员 API 位于 /account-management。客户端规则与根证书下载分别使用根级 GET /api/v1/client/routing.txt 和 GET /api/v1/client/ca.cer，安装包下载页面使用根级 GET /client；这些入口具有独立认证语义，不属于本管理 OpenAPI。"
        },
        "servers": [
            { "url": "/account-management", "description": "SOCKS5 管理子路径" }
        ],
        "paths": {
            "/api/v1/auth/login": { "post": { "summary": "管理员登录" } },
            "/api/v1/auth/local": { "get": { "summary": "消费本机一次性登录票据" } },
            "/api/v1/auth/logout": { "post": { "summary": "退出当前会话" } },
            "/api/v1/auth/session": { "get": { "summary": "查询当前会话" } },
            "/api/v1/management/identity": {
                "get": { "summary": "查询脱敏管理身份" },
                "put": { "summary": "修改管理身份" }
            },
            "/api/v1/management/apiKey": { "get": { "summary": "获取当前完整 Key" } },
            "/api/v1/accounts": {
                "get": {
                    "summary": "查询账号",
                    "description": "先按 search/status/expiration 过滤并按 sort/order 排序，再应用 offset/limit。未知参数和枚举值返回 400。",
                    "parameters": [
                        { "name": "offset", "in": "query", "schema": { "type": "integer", "minimum": 0, "default": 0 } },
                        { "name": "limit", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 200, "default": 100 } },
                        { "name": "search", "in": "query", "schema": { "type": "string" }, "description": "按账号或备注包含匹配" },
                        { "name": "status", "in": "query", "schema": { "type": "string", "enum": ["available", "disabled", "expired"] } },
                        { "name": "expiration", "in": "query", "schema": { "type": "string", "enum": ["never", "scheduled", "expired"] } },
                        { "name": "sort", "in": "query", "schema": { "type": "string", "enum": ["createdAt", "username", "expiresAt", "uploadedBytes", "downloadedBytes", "totalBytes", "activeConnections", "onlineIps"], "default": "createdAt" } },
                        { "name": "order", "in": "query", "schema": { "type": "string", "enum": ["asc", "desc"], "default": "desc" } }
                    ]
                },
                "post": { "summary": "创建账号" }
            },
            "/api/v1/accounts/batch": {
                "patch": { "summary": "批量修改账号策略与按原到期时间加时" },
                "delete": { "summary": "批量删除账号" }
            },
            "/api/v1/accounts/{accountId}": {
                "get": { "summary": "账号详情" },
                "patch": { "summary": "修改账号" },
                "delete": { "summary": "删除账号" }
            },
            "/api/v1/accounts/{accountId}/password": {
                "put": { "summary": "设置固定密码" },
                "delete": { "summary": "切换任意密码模式" }
            },
            "/api/v1/accounts/{accountId}/connections": { "get": { "summary": "查询账号连接" } },
            "/api/v1/accounts/{accountId}/usage": { "get": { "summary": "查询账号用量" } },
            "/api/v1/accounts/{accountId}/disconnect": { "post": { "summary": "强制下线账号" } },
            "/api/v1/ruleSets": {
                "get": { "summary": "查询规则集" },
                "post": {
                    "summary": "创建规则集",
                    "description": "content 必须符合 RoutingText，保存时拒绝缺失或模糊 DNS 上游。"
                }
            },
            "/api/v1/ruleSets/batch": {
                "delete": { "summary": "批量删除规则集" }
            },
            "/api/v1/ruleSets/{ruleSetId}": {
                "get": { "summary": "查询规则集详情" },
                "put": {
                    "summary": "保存规则集正文",
                    "description": "content 必须符合 RoutingText，PRIMARY 必需，SECONDARY 可选。"
                },
                "delete": { "summary": "删除规则集" }
            },
            "/api/v1/ruleSets/{ruleSetId}/enabled": {
                "put": { "summary": "互斥切换规则集启用状态" }
            },
            "/api/v1/connections": { "get": { "summary": "查询全部连接" } },
            "/api/v1/statistics": { "get": { "summary": "账号服务统计" } },
            "/api/v1/auditLogs": { "get": { "summary": "查询审计日志" } },
            "/api/v1/health": { "get": { "summary": "服务健康状态" } },
            "/api/v1/openapi.json": { "get": { "summary": "OpenAPI 文档" } }
        },
        "components": {
            "schemas": {
                "RoutingText": {
                    "type": "string",
                    "description": "routing.txt 必须包含且不得重复 [DNS]、[RoutingRule]、[GRoutingRule]、[proxy_app]。[DNS] 必须有唯一 PRIMARY,<IPv4/IPv6>，可有唯一 SECONDARY,<IPv4/IPv6>，拒绝主机名、未知键和重复键。[RoutingRule] 只作用于 proxy_app 中的应用，[GRoutingRule] 只作用于其他应用，两种范围允许混合。",
                    "examples": ["[DNS]\nPRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\n\n[RoutingRule]\n\n[GRoutingRule]\nFINAL,PROXY\n\n[proxy_app]\n"]
                }
            }
        }
    }))
}

/// 监听关闭标记；晚订阅者也能立即观察已经发生的关闭。
async fn waitForShutdown(mut receiver: watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
}

const fn defaultAccountPageLimit() -> i64 {
    50
}

/// 统一约束列表分页，避免负偏移和无界查询占用独立管理服务的 SQLite 连接。
fn validatePage(query: &PageQuery) -> Result<()> {
    if query.offset < 0 || !(1..=200).contains(&query.limit) {
        return Err(AccountServiceError::Validation(
            "分页参数要求 offset 大于等于 0，limit 位于 1 至 200".to_owned(),
        ));
    }
    Ok(())
}
