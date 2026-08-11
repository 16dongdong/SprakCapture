use std::io;

use location_core::LocationError;
use thiserror::Error;

/// 描述 capture-core 的稳定失败边界；外层根据 code/messageKey 渲染请求语言。
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("error.capture.invalidLimits")]
    InvalidLimits,
    #[error("error.capture.invalidMemoryThreshold")]
    InvalidMemoryThreshold,
    #[error("error.capture.invalidMetadataMemoryBudget")]
    InvalidMetadataMemoryBudget,
    #[error("error.capture.invalidRecordingRules")]
    InvalidRecordingRules,
    #[error("error.capture.metadataMemoryBudgetExceeded")]
    MetadataMemoryBudgetExceeded,
    #[error("error.capture.sessionClosed")]
    SessionClosed,
    #[error("error.capture.transactionNotFound")]
    TransactionNotFound,
    #[error("error.capture.transactionFinished")]
    TransactionFinished,
    #[error("error.capture.collectionChanged")]
    CollectionChanged,
    #[error("error.capture.bodyNotFound")]
    BodyNotFound,
    #[error("error.capture.invalidBodyLength")]
    InvalidBodyLength,
    #[error("error.capture.io")]
    Io {
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Location(#[from] LocationError),
}

impl CaptureError {
    /// 将底层文件系统错误包装为不暴露本机路径的稳定库错误。
    pub(crate) fn io(source: io::Error) -> Self {
        Self::Io { source }
    }

    /// 返回跨版本稳定的机器错误码，供 HTTP、MCP 与日志结构化记录。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidLimits => "captureInvalidLimits",
            Self::InvalidMemoryThreshold => "captureInvalidMemoryThreshold",
            Self::InvalidMetadataMemoryBudget => "captureInvalidMetadataMemoryBudget",
            Self::InvalidRecordingRules => "captureInvalidRecordingRules",
            Self::MetadataMemoryBudgetExceeded => "captureMetadataMemoryBudgetExceeded",
            Self::SessionClosed => "captureSessionClosed",
            Self::TransactionNotFound => "captureTransactionNotFound",
            Self::TransactionFinished => "captureTransactionFinished",
            Self::CollectionChanged => "captureCollectionChanged",
            Self::BodyNotFound => "captureBodyNotFound",
            Self::InvalidBodyLength => "captureInvalidBodyLength",
            Self::Io { .. } => "captureIo",
            Self::Location(error) => error.code(),
        }
    }

    /// 返回后续错误目录使用的稳定键；库本身不持有任何单语言 catalog。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::InvalidLimits => "error.capture.invalidLimits",
            Self::InvalidMemoryThreshold => "error.capture.invalidMemoryThreshold",
            Self::InvalidMetadataMemoryBudget => "error.capture.invalidMetadataMemoryBudget",
            Self::InvalidRecordingRules => "error.capture.invalidRecordingRules",
            Self::MetadataMemoryBudgetExceeded => "error.capture.metadataMemoryBudgetExceeded",
            Self::SessionClosed => "error.capture.sessionClosed",
            Self::TransactionNotFound => "error.capture.transactionNotFound",
            Self::TransactionFinished => "error.capture.transactionFinished",
            Self::CollectionChanged => "error.capture.collectionChanged",
            Self::BodyNotFound => "error.capture.bodyNotFound",
            Self::InvalidBodyLength => "error.capture.invalidBodyLength",
            Self::Io { .. } => "error.capture.io",
            Self::Location(error) => error.messageKey(),
        }
    }
}
