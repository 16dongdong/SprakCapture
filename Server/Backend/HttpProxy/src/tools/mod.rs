//! 提供可由工具流水线注册的内建 HTTP 头部与访问控制工具。
mod autoSave;
mod blockCookies;
mod blockList;
mod breakpoints;
mod dnsSpoofing;
mod error;
mod locationScope;
mod mapLocal;
mod mapRemote;
mod mapSupport;
mod messageDraft;
mod mirror;
mod noCaching;
mod recordingRules;
mod rewrite;
mod throttling;

pub use autoSave::{
    AutoSaveConfiguration, AutoSaveError, AutoSaveFormat, AutoSavePublicState, AutoSaveTool,
};
pub use blockCookies::{BlockCookiesConfiguration, BlockCookiesTool};
pub use blockList::{
    BlockListConfiguration, BlockListDecision, BlockListTool, BlockMode, SyntheticBlockResponse,
};
pub use breakpoints::{
    BreakpointError, BreakpointPhase, BreakpointRule, BreakpointTimeoutAction,
    BreakpointsConfiguration, BreakpointsTool, EditableHttpMessage, MessageDraftError,
    SuspendedBreakpoint,
};
pub use dnsSpoofing::{
    DnsSpoofingConfiguration, DnsSpoofingError, DnsSpoofingRule, DnsSpoofingTool,
};
pub use error::ToolError;
pub use mapLocal::{
    MapLocalConfiguration, MapLocalResolution, MapLocalResponse, MapLocalResponseSource,
    MapLocalRule, MapLocalTool, MapResponseHeader,
};
pub use mapRemote::{
    MapRemoteApplication, MapRemoteConfiguration, MapRemoteRule, MapRemoteTarget, MapRemoteTool,
};
pub use mapSupport::MapToolError;
pub use messageDraft::{applyRequestDraft, applyResponseDraft, editableRequest, editableResponse};
pub use mirror::{
    MirrorConfiguration, MirrorError, MirrorLayout, MirrorOverflowPolicy, MirrorPublicState,
    MirrorTool,
};
pub use noCaching::{HeaderMutation, NoCachingConfiguration, NoCachingTool};
pub use recordingRules::RecordingRulesTool;
pub use rewrite::{
    HeaderAction, RewriteConfiguration, RewriteError, RewriteRule, RewriteRuleType, RewriteSet,
    RewriteTool,
};
pub use throttling::{
    ThrottleChunk, ThrottleChunkAction, ThrottleDirection, ThrottlePacer, ThrottlePlan,
    ThrottlePreset, ThrottleProfile, ThrottlingConfiguration, ThrottlingError,
    ThrottlingPublicState, ThrottlingTool, TokenBucket, builtInThrottlePresets,
};

use crate::{PipelineError, ToolId};

/// 将工具层的结构化错误转换为流水线错误，保留 ToolId 与机器码以便控制面精确定位失败源。
pub(crate) fn pipelineError(toolId: ToolId, error: ToolError) -> PipelineError {
    PipelineError::ToolFailed {
        toolId,
        code: error.code().to_owned(),
    }
}
