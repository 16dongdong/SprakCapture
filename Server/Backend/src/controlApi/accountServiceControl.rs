//! 暴露 SprakCapture 设置页使用的账号服务生命周期与管理身份转发接口。

use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    routing::{get, post},
};
use serde::Deserialize;

use super::{ApiError, ControlState, LocalizedApiError, MultiAccountPublicState};
use crate::localization::{ErrorCode, RequestLocale};

/// 修改管理身份只转发新账号和新密码；本机控制面通过内部令牌完成授权且不记录请求正文。
#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentityUpdate {
    username: String,
    password: String,
}

/// 注册账号服务设置接口；账号 CRUD 继续由独立公共管理端提供，不复制进主控制面。
pub(super) fn addRoutes(router: Router<ControlState>) -> Router<ControlState> {
    router
        .route("/api/v1/multiAccount", get(getMultiAccountState))
        .route(
            "/api/v1/multiAccount/identity",
            get(getManagementIdentity).put(updateManagementIdentity),
        )
        .route("/api/v1/multiAccount/apiKey", get(getApiKey))
        .route(
            "/api/v1/multiAccount/managementSession",
            post(createManagementSession),
        )
}

/// 转发脱敏的管理员身份；响应只包含账号、修订号和 Key 指纹，不包含密码、完整 Key 或内部令牌。
///
/// 运行上下文：设置页进入多账号区域时按需调用；账号服务未运行或内部请求失败时返回本地化控制错误。
async fn getManagementIdentity(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<serde_json::Value>, LocalizedApiError> {
    state
        .accountService
        .requestWithoutBody(reqwest::Method::GET, "/internal/v1/management/identity")
        .await
        .map(Json)
        .map_err(|detail| {
            ApiError::internal(ErrorCode::ServiceStartFailed)
                .withParam("detail", detail)
                .withLocale(locale)
        })
}

/// 返回管理服务实时状态；该读取会保留完整 Key 和内部令牌的保密边界。
async fn getMultiAccountState(State(state): State<ControlState>) -> Json<MultiAccountPublicState> {
    let configuration = state.multiAccountConfiguration.read().await.clone();
    Json(state.accountService.publicState(&configuration).await)
}

/// 把管理身份修改转发到内部回环端点；账号服务负责密码校验、事务更新和会话失效。
async fn updateManagementIdentity(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
    request: Result<Json<IdentityUpdate>, JsonRejection>,
) -> Result<Json<serde_json::Value>, LocalizedApiError> {
    let Json(request) = request.map_err(|error| invalidRequest(error, locale))?;
    let response = forwardInternal(
        &state,
        reqwest::Method::PUT,
        "/internal/v1/management/identity",
        &request,
        locale,
    )
    .await?;
    state.publishCurrentConfiguration().await;
    Ok(response)
}

/// 当前 SprakCapture 会话已经完成控制面授权，直接返回可复制的确定性 Key，不重复接收密码。
async fn getApiKey(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<serde_json::Value>, LocalizedApiError> {
    let response = state
        .accountService
        .requestWithoutBody(reqwest::Method::GET, "/internal/v1/management/apiKey")
        .await
        .map(Json)
        .map_err(|detail| {
            ApiError::internal(ErrorCode::ServiceStartFailed)
                .withParam("detail", detail)
                .withLocale(locale)
        })?;
    state.publishCurrentConfiguration().await;
    Ok(response)
}

/// 签发主工作台内部账号路由使用的一次性会话路径；响应不包含独立主机、端口或外部 URL。
///
/// 运行上下文：账号管理页面挂载 iframe 前调用；本机控制端无需再次输入密码，远程端已由统一登录门禁授权。
/// 失败语义：账号服务未运行、实例切换或票据签发失败时返回本地化服务错误，页面不会加载未授权 iframe。
async fn createManagementSession(
    RequestLocale(locale): RequestLocale,
    State(state): State<ControlState>,
) -> Result<Json<serde_json::Value>, LocalizedApiError> {
    state
        .accountService
        .managementSessionPath()
        .await
        .map(|path| Json(serde_json::json!({ "path": path })))
        .map_err(|detail| {
            ApiError::internal(ErrorCode::ServiceStartFailed)
                .withParam("detail", detail)
                .withLocale(locale)
        })
}

/// 统一转发内部管理请求并擦除底层响应正文；错误响应不得把密码或内部令牌带入控制错误参数。
async fn forwardInternal<T: serde::Serialize + ?Sized>(
    state: &ControlState,
    method: reqwest::Method,
    path: &str,
    request: &T,
    locale: crate::localization::Locale,
) -> Result<Json<serde_json::Value>, LocalizedApiError> {
    state
        .accountService
        .request(method, path, request)
        .await
        .map(Json)
        .map_err(|detail| {
            ApiError::internal(ErrorCode::ServiceStartFailed)
                .withParam("detail", detail)
                .withLocale(locale)
        })
}

/// 把 Axum JSON 拒绝归一为现有配置请求错误，避免向设置页暴露框架默认文本协议。
fn invalidRequest(error: JsonRejection, locale: crate::localization::Locale) -> LocalizedApiError {
    ApiError::badRequest(ErrorCode::InvalidConfigurationRequest)
        .withParam("detail", error.body_text())
        .withLocale(locale)
}
