#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

//! 提供与传输连接解耦的录制会话、事务元数据和完整正文存储。

mod bodyStore;
mod error;
mod harExport;
mod metadataBudget;
mod model;
mod recordingRules;
mod recordingSession;
mod responseEntity;

pub use bodyStore::{BodyReadLease, BodyRef, BodySpool, BodyStorageKind};
pub use error::CaptureError;
pub use harExport::{
    HarArchive, HarBodyMetadata, HarCache, HarCaptureExtension, HarContent, HarCookie, HarCreator,
    HarEntry, HarExportError, HarExportRequest, HarLog, HarNameValue, HarPostData, HarRequest,
    HarResponse, HarTimings, buildHarExport,
};
pub use model::{
    BeginTransaction, BodyHandleMeta, BodyResponse, BodyWrite, HeaderField, MessageSide,
    RecordingConfiguration, RecordingLimits, RecordingLimitsUpdate, RecordingPageView,
    RecordingSettingsUpdate, RecordingSnapshot, RecordingState, ResponseRangeCandidate,
    StreamPacket, TransactionCompletion, TransactionDetailRecord, TransactionError,
    TransactionFlags, TransactionProgressUpdate, TransactionProtocol, TransactionSizes,
    TransactionStatus, TransactionSummary, TransactionTimings, TransactionUpdate,
    TransactionUserUpdate, currentTimeMilliseconds,
};
pub use recordingRules::{
    RecordingRule, RecordingRuleAction, RecordingRuleConfiguration, RecordingRuleError,
    RecordingRuleKind, RecordingRuleRuntime, RecordingRuleSet,
};
pub use recordingSession::RecordingSession;
pub use responseEntity::{responseContentRange, strongResponseEntityTag};
