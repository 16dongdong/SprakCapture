//! 把账号管理页面映射到 Sprak Capture 控制源下，隐藏独立子进程监听地址。
//!
//! 本模块只做字节与端到端头转发，不复制账号 CRUD、认证或页面逻辑；浏览器始终看到
//! `/account-management/*`，账号服务重启后的端点由监督器逐请求重新解析。

use axum::{
    Router,
    body::Body,
    extract::{OriginalUri, State},
    http::{Request, Response, StatusCode, header},
    response::IntoResponse,
    routing::any,
};
use reqwest::redirect::Policy;

use super::ControlState;

const maximumMappedRequestBodyBytes: usize = 16 * 1024 * 1024;

/// 构造唯一账号管理映射入口；路径前缀完整转发给账号服务，使其进入独立账号管理子路由。
///
/// 运行上下文：本机控制 Router 组装时调用，远程入口则由账号服务直接挂载相同页面。
/// 参数：`router` 为尚未绑定状态的控制路由。
/// 失败语义：路由注册本身不执行 I/O，运行期错误由映射处理器以 502/503 返回。
pub(super) fn routes() -> Router<ControlState> {
    Router::new()
        .route("/account-management", any(proxyAccountRequest))
        .route(
            "/account-management/{*mappedPath}",
            any(proxyAccountRequest),
        )
        .route("/client", any(proxyClientRequest))
        .route("/client/{*mappedPath}", any(proxyClientRequest))
}

/// 把同源账号管理请求转发到当前账号服务公共回环端点，并保持流式响应。
///
/// 运行上下文：桌面 WebView 和开发 Web 的内嵌账号页面使用；`originalUri` 含查询字符串。
/// 失败语义：服务未运行返回 503，上游网络或响应构造失败返回 502，错误正文不包含内部令牌。
async fn proxyAccountRequest(
    State(state): State<ControlState>,
    OriginalUri(originalUri): OriginalUri,
    request: Request<Body>,
) -> Response<Body> {
    match forwardMappedRequest(&state, originalUri, request, "/account-management").await {
        Ok(response) => response,
        Err((status, message)) => (status, message).into_response(),
    }
}

/// 将客户端下载安装页映射到账号服务，保持控制端口下的同源入口。
/// 该入口只转发公开 `/client` 页面及其下载请求，账号服务继续负责凭据校验和单次打包。
/// 映射失败返回明确的 4xx/5xx 状态，不伪造下载成功。
async fn proxyClientRequest(
    State(state): State<ControlState>,
    OriginalUri(originalUri): OriginalUri,
    request: Request<Body>,
) -> Response<Body> {
    match forwardMappedRequest(&state, originalUri, request, "/client").await {
        Ok(response) => response,
        Err((status, message)) => (status, message).into_response(),
    }
}

/// 执行账号映射的可失败部分；独立返回 `Result` 让 HTTP 处理器只负责稳定错误投影。
///
/// 运行上下文：每次请求重新读取监督器端点，参数包含共享状态、原始 URI 与完整请求。
/// 失败语义：正文超限、服务不可用或上游 I/O 失败均返回明确状态和中文摘要。
async fn forwardMappedRequest(
    state: &ControlState,
    originalUri: axum::http::Uri,
    request: Request<Body>,
    mappedPrefix: &'static str,
) -> Result<Response<Body>, (StatusCode, String)> {
    let endpoint = state
        .accountService
        .mappedPublicEndpoint()
        .await
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error))?;
    // 账号服务本身以 `/account-management` 区分账号页面和 Sprak Web。剥掉前缀会让
    // 一次性票据误入主站 SPA 回退并在 iframe 中递归加载工作台，因此必须保留完整路径。
    let mappedPath = originalUri
        .path_and_query()
        .map(|path| path.as_str())
        .unwrap_or(mappedPrefix);
    if !mappedPath.starts_with(mappedPrefix) {
        return Err((StatusCode::BAD_REQUEST, "账号管理映射路径无效".to_owned()));
    }
    let upstreamUrl = format!("{endpoint}{mappedPath}");
    let (requestParts, requestBody) = request.into_parts();
    let requestBytes = axum::body::to_bytes(requestBody, maximumMappedRequestBodyBytes)
        .await
        .map_err(|error| {
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("读取账号管理请求正文失败：{error}"),
            )
        })?;
    let mut upstreamRequest = accountMappingClient()?
        .request(requestParts.method, upstreamUrl)
        .body(requestBytes);
    for (headerName, headerValue) in &requestParts.headers {
        if shouldForwardHeader(headerName) {
            upstreamRequest = upstreamRequest.header(headerName, headerValue);
        }
    }
    let upstreamResponse = upstreamRequest.send().await.map_err(|error| {
        (
            StatusCode::BAD_GATEWAY,
            format!("连接账号管理服务失败：{error}"),
        )
    })?;
    let status = upstreamResponse.status();
    let upstreamHeaders = upstreamResponse.headers().clone();
    let responseBytes = upstreamResponse.bytes().await.map_err(|error| {
        (
            StatusCode::BAD_GATEWAY,
            format!("读取账号管理响应失败：{error}"),
        )
    })?;
    let mut response = Response::builder().status(status);
    for (headerName, headerValue) in &upstreamHeaders {
        if shouldForwardHeader(headerName) {
            response = response.header(headerName, headerValue);
        }
    }
    response.body(Body::from(responseBytes)).map_err(|error| {
        (
            StatusCode::BAD_GATEWAY,
            format!("构造账号管理响应失败：{error}"),
        )
    })
}

/// 创建禁止自动跳转的账号管理映射客户端，确保一次性登录响应中的 Cookie 能原样返回浏览器。
///
/// 运行上下文：映射处理器逐请求创建隔离客户端；上游的 3xx、Location 与 Set-Cookie 必须由桌面
/// WebView 自己消费。若在代理内部跟随跳转，首次响应携带的登录 Cookie 会被吞掉，页面最终只能
/// 回到登录表单。客户端配置失败返回 502，调用方不会继续发送未受控请求。
fn accountMappingClient() -> Result<reqwest::Client, (StatusCode, String)> {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|error| {
            (
                StatusCode::BAD_GATEWAY,
                format!("创建账号管理映射客户端失败：{error}"),
            )
        })
}

/// 过滤逐跳头并保留 Cookie；账号页面的持久会话必须在同源映射与直接远程入口之间一致。
///
/// 运行上下文：请求和响应复制共用，参数是待判断的头名。
/// 失败语义：false 表示由两侧 HTTP 栈重新生成，不影响业务端到端语义。
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
            | "origin"
            | "referer"
    )
}

#[cfg(test)]
#[path = "../../tests/unit/accountServiceMappingTests.rs"]
mod tests;
