use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use thiserror::Error;

/// 统一账号服务的存储、认证、冲突和传输失败，HTTP 层据此返回稳定错误码。
#[derive(Debug, Error)]
pub enum AccountServiceError {
    #[error("账号不存在")]
    AccountNotFound,
    #[error("账号名称已存在")]
    AccountConflict,
    #[error("账号策略已被其他管理端修改")]
    PolicyRevisionConflict { currentRevision: i64 },
    #[error("规则集不存在")]
    RuleSetNotFound,
    #[error("规则集名称已存在")]
    RuleSetConflict,
    #[error("规则集已被其他管理端修改")]
    RuleSetRevisionConflict { currentRevision: i64 },
    #[error("管理身份认证失败")]
    ManagementAuthenticationFailed,
    #[error("SOCKS5 账号认证失败")]
    SocksAuthenticationFailed,
    #[error("活动租约不存在或已失效")]
    LeaseNotFound,
    #[error("内部接口令牌无效")]
    InternalAuthenticationFailed,
    #[error("管理登录尝试过于频繁")]
    RateLimited,
    #[error("请求字段无效：{0}")]
    Validation(String),
    #[error("账号服务状态冲突：{0}")]
    StateConflict(String),
    #[error("数据库操作失败：{0}")]
    Database(#[from] rusqlite::Error),
    #[error("JSON 处理失败：{0}")]
    Json(#[from] serde_json::Error),
    #[error("网络操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("内部加密材料处理失败")]
    Credential,
}

pub type Result<T> = std::result::Result<T, AccountServiceError>;

/// 保持公共 API 错误体结构稳定，底层数据库和凭据诊断不进入远端响应。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    code: &'static str,
    message: String,
    params: serde_json::Value,
}

impl IntoResponse for AccountServiceError {
    /// 把领域错误映射为明确 HTTP 状态；认证路径统一使用 401，避免泄露账号存在性。
    fn into_response(self) -> axum::response::Response {
        let (status, code, params) = match &self {
            Self::AccountNotFound => (
                StatusCode::NOT_FOUND,
                "accountNotFound",
                serde_json::json!({}),
            ),
            Self::AccountConflict => (
                StatusCode::CONFLICT,
                "accountConflict",
                serde_json::json!({}),
            ),
            Self::PolicyRevisionConflict { currentRevision } => (
                StatusCode::CONFLICT,
                "accountPolicyRevisionConflict",
                serde_json::json!({ "currentRevision": currentRevision }),
            ),
            Self::RuleSetNotFound => (
                StatusCode::NOT_FOUND,
                "ruleSetNotFound",
                serde_json::json!({}),
            ),
            Self::RuleSetConflict => (
                StatusCode::CONFLICT,
                "ruleSetConflict",
                serde_json::json!({}),
            ),
            Self::RuleSetRevisionConflict { currentRevision } => (
                StatusCode::CONFLICT,
                "ruleSetRevisionConflict",
                serde_json::json!({ "currentRevision": currentRevision }),
            ),
            Self::ManagementAuthenticationFailed | Self::InternalAuthenticationFailed => (
                StatusCode::UNAUTHORIZED,
                "authenticationFailed",
                serde_json::json!({}),
            ),
            Self::SocksAuthenticationFailed => (
                StatusCode::UNAUTHORIZED,
                "socksAuthenticationFailed",
                serde_json::json!({}),
            ),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "rateLimited",
                serde_json::json!({}),
            ),
            Self::LeaseNotFound => (
                StatusCode::NOT_FOUND,
                "leaseNotFound",
                serde_json::json!({}),
            ),
            Self::Validation(_) => (
                StatusCode::BAD_REQUEST,
                "invalidRequest",
                serde_json::json!({}),
            ),
            Self::StateConflict(_) => {
                (StatusCode::CONFLICT, "stateConflict", serde_json::json!({}))
            }
            Self::Database(_) | Self::Json(_) | Self::Io(_) | Self::Credential => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internalError",
                serde_json::json!({}),
            ),
        };
        let message = match self {
            Self::Database(_) | Self::Json(_) | Self::Io(_) | Self::Credential => {
                "账号服务内部错误。".to_owned()
            }
            other => other.to_string(),
        };
        (
            status,
            Json(ErrorResponse {
                code,
                message,
                params,
            }),
        )
            .into_response()
    }
}
