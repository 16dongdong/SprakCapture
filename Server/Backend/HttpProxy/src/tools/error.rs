use location_core::LocationError;
use thiserror::Error;

/// 描述内建工具配置和匹配阶段可稳定判别的失败类型；控制面使用 code 与 messageKey 完成本地化。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ToolError {
    #[error("error.tools.invalidLocationPattern")]
    InvalidLocationPattern {
        index: usize,
        #[source]
        source: LocationError,
    },
    #[error("error.tools.invalidLocation")]
    InvalidLocation {
        #[source]
        source: LocationError,
    },
    #[error("error.tools.invalidBlockStatusCode")]
    InvalidBlockStatusCode,
    #[error("error.tools.blockResponseBodyTooLarge")]
    BlockResponseBodyTooLarge,
}

impl ToolError {
    /// 返回供控制 API、MCP 和结构化日志共享的机器错误码，不暴露规则内容或请求位置。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidLocationPattern { .. } => "toolInvalidLocationPattern",
            Self::InvalidLocation { .. } => "toolInvalidLocation",
            Self::InvalidBlockStatusCode => "toolInvalidBlockStatusCode",
            Self::BlockResponseBodyTooLarge => "toolBlockResponseBodyTooLarge",
        }
    }

    /// 返回由语言包渲染的稳定消息键；工具层不直接构造用户可见文本。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::InvalidLocationPattern { .. } => "error.tools.invalidLocationPattern",
            Self::InvalidLocation { .. } => "error.tools.invalidLocation",
            Self::InvalidBlockStatusCode => "error.tools.invalidBlockStatusCode",
            Self::BlockResponseBodyTooLarge => "error.tools.blockResponseBodyTooLarge",
        }
    }
}
