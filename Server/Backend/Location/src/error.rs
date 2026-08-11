use thiserror::Error;

/// 描述 Location 库可稳定判别的失败类型；控制层应使用 code/messageKey 完成本地化渲染。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum LocationError {
    #[error("error.location.invalidProtocol")]
    InvalidProtocol,
    #[error("error.location.invalidHost")]
    InvalidHost,
    #[error("error.location.invalidPort")]
    InvalidPort,
    #[error("error.location.invalidPath")]
    InvalidPath,
    #[error("error.location.invalidCandidate")]
    InvalidCandidate,
}

impl LocationError {
    /// 返回跨控制面和语言包保持稳定的机器错误码。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidProtocol => "locationInvalidProtocol",
            Self::InvalidHost => "locationInvalidHost",
            Self::InvalidPort => "locationInvalidPort",
            Self::InvalidPath => "locationInvalidPath",
            Self::InvalidCandidate => "locationInvalidCandidate",
        }
    }

    /// 返回后续 I18N catalog 使用的稳定键；库层不直接生成任何单语言文案。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::InvalidProtocol => "error.location.invalidProtocol",
            Self::InvalidHost => "error.location.invalidHost",
            Self::InvalidPort => "error.location.invalidPort",
            Self::InvalidPath => "error.location.invalidPath",
            Self::InvalidCandidate => "error.location.invalidCandidate",
        }
    }
}
