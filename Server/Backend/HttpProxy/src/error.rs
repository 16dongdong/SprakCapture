use std::{collections::BTreeMap, io};

use capture_core::TransactionError;
use http::StatusCode;
use thiserror::Error;

/// 描述 HTTP 代理启动和生命周期的稳定失败类型；请求级错误记录在 TransactionError。
#[derive(Debug, Error)]
pub enum HttpProxyError {
    #[error("error.httpProxy.invalidConnectionLimit")]
    InvalidConnectionLimit,
    #[error("error.httpProxy.invalidHeaderLimit")]
    InvalidHeaderLimit,
    #[error("error.httpProxy.headerBudgetExceeded")]
    HeaderBudgetExceeded,
    #[error("error.httpProxy.invalidBodyLimit")]
    InvalidBodyLimit,
    #[error("error.httpProxy.captureBudgetExceeded")]
    CaptureBudgetExceeded,
    #[error("error.httpProxy.invalidTimeout")]
    InvalidTimeout,
    #[error("error.httpProxy.invalidUpstreamProxy")]
    InvalidUpstreamProxy,
    #[error("error.ssl.tlsConfiguration")]
    TlsConfigurationFailed,
    #[error("error.httpProxy.bindFailed")]
    BindFailed {
        #[source]
        source: io::Error,
    },
    #[error("error.httpProxy.acceptFailed")]
    AcceptFailed {
        #[source]
        source: io::Error,
    },
    #[error("error.httpProxy.runtimeJoinFailed")]
    RuntimeJoinFailed,
    #[error("error.httpProxy.shutdownTimeout")]
    ShutdownTimeout,
}

/// 描述单个代理事务可序列化的失败原因；不会直接产生任何单语言响应正文。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestFailure {
    InvalidRequest,
    LoopDetected,
    UnsupportedScheme,
    UpstreamUnavailable,
    UpstreamTimeout,
    ClientDisconnected,
    UpgradeFailed,
    Cancelled,
    CaptureFailed,
    UpstreamProtocol,
    DownstreamTlsHandshake,
    UpstreamTlsHandshake,
    PipelineBodyLimitExceeded,
}

impl RequestFailure {
    /// 返回客户端尚可接收响应头时使用的 HTTP 状态。
    pub(crate) const fn statusCode(self) -> StatusCode {
        match self {
            Self::InvalidRequest | Self::UnsupportedScheme => StatusCode::BAD_REQUEST,
            Self::LoopDetected => StatusCode::LOOP_DETECTED,
            Self::PipelineBodyLimitExceeded => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UpstreamTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::UpstreamUnavailable
            | Self::ClientDisconnected
            | Self::UpgradeFailed
            | Self::Cancelled
            | Self::CaptureFailed
            | Self::UpstreamProtocol
            | Self::DownstreamTlsHandshake
            | Self::UpstreamTlsHandshake => StatusCode::BAD_GATEWAY,
        }
    }

    /// 返回事务与响应头共用的稳定机器错误码。
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "httpProxyInvalidRequest",
            Self::LoopDetected => "httpProxyLoopDetected",
            Self::UnsupportedScheme => "httpProxyUnsupportedScheme",
            Self::UpstreamUnavailable => "httpProxyUpstreamUnavailable",
            Self::UpstreamTimeout => "httpProxyUpstreamTimeout",
            Self::ClientDisconnected => "httpProxyClientDisconnected",
            Self::UpgradeFailed => "httpProxyUpgradeFailed",
            Self::Cancelled => "httpProxyCancelled",
            Self::CaptureFailed => "httpProxyCaptureFailed",
            Self::UpstreamProtocol => "httpProxyUpstreamProtocol",
            Self::DownstreamTlsHandshake => "sslDownstreamHandshakeFailed",
            Self::UpstreamTlsHandshake => "sslUpstreamHandshakeFailed",
            Self::PipelineBodyLimitExceeded => "httpProxyPipelineBodyLimitExceeded",
        }
    }

    /// 返回 I18N catalog 键，供后续控制面按请求语言渲染。
    pub(crate) const fn messageKey(self) -> &'static str {
        match self {
            Self::InvalidRequest => "error.httpProxy.invalidRequest",
            Self::LoopDetected => "error.httpProxy.loopDetected",
            Self::UnsupportedScheme => "error.httpProxy.unsupportedScheme",
            Self::UpstreamUnavailable => "error.httpProxy.upstreamUnavailable",
            Self::UpstreamTimeout => "error.httpProxy.upstreamTimeout",
            Self::ClientDisconnected => "error.httpProxy.clientDisconnected",
            Self::UpgradeFailed => "error.httpProxy.upgradeFailed",
            Self::Cancelled => "error.httpProxy.cancelled",
            Self::CaptureFailed => "error.httpProxy.captureFailed",
            Self::UpstreamProtocol => "error.httpProxy.upstreamProtocol",
            Self::DownstreamTlsHandshake => "error.ssl.downstreamHandshakeFailed",
            Self::UpstreamTlsHandshake => "error.ssl.upstreamHandshakeFailed",
            Self::PipelineBodyLimitExceeded => "error.httpProxy.pipelineBodyLimitExceeded",
        }
    }

    /// 构造 capture-core 使用的未本地化错误，并携带可选目标 host 参数。
    pub(crate) fn transactionError(self, host: Option<&str>) -> TransactionError {
        let params = host.map_or_else(BTreeMap::new, |host| {
            BTreeMap::from([("host".to_owned(), host.to_owned())])
        });
        TransactionError {
            code: self.code().to_owned(),
            messageKey: self.messageKey().to_owned(),
            params,
        }
    }
}

impl HttpProxyError {
    /// 包装监听绑定错误，不把本机路径或语言文案带入公共错误。
    pub(crate) fn bind(source: io::Error) -> Self {
        Self::BindFailed { source }
    }

    /// 包装运行期 accept 错误，供生命周期拥有者决定重启。
    pub(crate) fn accept(source: io::Error) -> Self {
        Self::AcceptFailed { source }
    }

    /// 返回跨 API、MCP 和日志保持稳定的机器错误码。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidConnectionLimit => "httpProxyInvalidConnectionLimit",
            Self::InvalidHeaderLimit => "httpProxyInvalidHeaderLimit",
            Self::HeaderBudgetExceeded => "httpProxyHeaderBudgetExceeded",
            Self::InvalidBodyLimit => "httpProxyInvalidBodyLimit",
            Self::CaptureBudgetExceeded => "httpProxyCaptureBudgetExceeded",
            Self::InvalidTimeout => "httpProxyInvalidTimeout",
            Self::InvalidUpstreamProxy => "httpProxyInvalidUpstreamProxy",
            Self::TlsConfigurationFailed => "sslTlsConfiguration",
            Self::BindFailed { .. } => "httpProxyBindFailed",
            Self::AcceptFailed { .. } => "httpProxyAcceptFailed",
            Self::RuntimeJoinFailed => "httpProxyRuntimeJoinFailed",
            Self::ShutdownTimeout => "httpProxyShutdownTimeout",
        }
    }

    /// 返回 I18N catalog 的稳定键；库本身不直接生成用户语言 message。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::InvalidConnectionLimit => "error.httpProxy.invalidConnectionLimit",
            Self::InvalidHeaderLimit => "error.httpProxy.invalidHeaderLimit",
            Self::HeaderBudgetExceeded => "error.httpProxy.headerBudgetExceeded",
            Self::InvalidBodyLimit => "error.httpProxy.invalidBodyLimit",
            Self::CaptureBudgetExceeded => "error.httpProxy.captureBudgetExceeded",
            Self::InvalidTimeout => "error.httpProxy.invalidTimeout",
            Self::InvalidUpstreamProxy => "error.httpProxy.invalidUpstreamProxy",
            Self::TlsConfigurationFailed => "error.ssl.tlsConfiguration",
            Self::BindFailed { .. } => "error.httpProxy.bindFailed",
            Self::AcceptFailed { .. } => "error.httpProxy.acceptFailed",
            Self::RuntimeJoinFailed => "error.httpProxy.runtimeJoinFailed",
            Self::ShutdownTimeout => "error.httpProxy.shutdownTimeout",
        }
    }
}
