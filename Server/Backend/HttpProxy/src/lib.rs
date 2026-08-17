#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

//! 提供 HTTP/1.1 正向代理、CONNECT 裸隧道和 capture-core 录制桥。

mod bodyStream;
mod captureBridge;
mod clientCertificate;
mod config;
mod connector;
mod error;
mod forwarder;
mod listeners;
pub mod pipeline;
mod runtimeMetrics;
mod server;
mod socksTunnel;
mod ssl;
mod target;
mod taskTracker;
pub mod tools;
mod upstreamClient;

pub use clientCertificate::{
    ClientCertificateFormat, ClientCertificateImport, ClientCertificateInfo,
    ClientCertificateUpdate,
};
pub use config::HttpProxyConfig;
pub use error::HttpProxyError;
pub use listeners::{
    AuxiliaryListenerBindings, AuxiliaryListenerConfiguration, AuxiliaryListenerError,
    ListenerBinding, PortForwardEntry, ReverseProxyEntry, ReverseProxyScheme,
    RunningAuxiliaryListeners, startAuxiliaryListeners,
};
pub use pipeline::{
    PipelineContext, PipelineDirective, PipelineError, PipelineRequestOutcome, PipelineTool,
    RequestDraft, ResponseDraft, SyntheticResponse, ToolId, ToolPhase, ToolPipeline,
    ToolRegistration,
};
pub use runtimeMetrics::HttpRuntimeMetrics;
pub use server::{
    HttpConnectionHandler, HttpProxyDependencies, HttpProxyExit, RunningHttpProxy,
    buildHttpConnectionHandler, startHttpProxy, startHttpProxyWithPlugins,
    startHttpProxyWithPluginsAndDns,
};
pub use socksTunnel::{SocksHttpTarget, SocksHttpTunnelHandler};
pub use ssl::{
    CertificateAuthorityInfo, SslMitmConfiguration, SslMitmError, SslMitmManager, SslPublicState,
};
pub use target::{canonicalAuthority, canonicalHostHeader};
pub use tools::{
    AutoSaveConfiguration, AutoSaveError, AutoSaveFormat, AutoSavePublicState, AutoSaveTool,
    BlockCookiesConfiguration, BlockCookiesTool, BlockListConfiguration, BlockListTool, BlockMode,
    BreakpointError, BreakpointPhase, BreakpointRule, BreakpointTimeoutAction,
    BreakpointsConfiguration, BreakpointsTool, DnsSpoofingConfiguration, DnsSpoofingError,
    DnsSpoofingRule, DnsSpoofingTool, EditableHttpMessage, HeaderAction, MapLocalConfiguration,
    MapLocalRule, MapLocalTool, MapRemoteConfiguration, MapRemoteRule, MapRemoteTool,
    MessageDraftError, MirrorConfiguration, MirrorError, MirrorLayout, MirrorOverflowPolicy,
    MirrorPublicState, MirrorTool, NoCachingConfiguration, NoCachingTool, RewriteConfiguration,
    RewriteError, RewriteRule, RewriteRuleType, RewriteSet, RewriteTool, SuspendedBreakpoint,
    ThrottleChunk, ThrottleChunkAction, ThrottleDirection, ThrottlePacer, ThrottlePlan,
    ThrottlePreset, ThrottleProfile, ThrottlingConfiguration, ThrottlingError,
    ThrottlingPublicState, ThrottlingTool, TokenBucket, builtInThrottlePresets,
};
