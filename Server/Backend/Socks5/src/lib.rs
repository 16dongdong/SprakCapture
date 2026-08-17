#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

//! 提供独立于界面的 SOCKS5 v5 服务、会话快照和有序关闭接口。

pub mod accountService;
pub mod address;
pub mod config;
pub mod error;
pub mod interception;
pub mod model;
pub mod protocol;
pub mod registry;
pub mod relay;
pub mod server;
pub mod udpRelay;

pub use accountService::{AccountServiceClientConfig, AccountTrafficLease, AccountTrafficStream};
pub use address::AddressOverride;
pub use config::{AuthenticationMode, Socks5Config};
pub use error::{Result, Socks5Error};
pub use model::{
    CaptureGeneration, CapturedPacket, ServerSnapshot, ServerStopOutcome, ServiceMetrics,
    SessionApplicationProtocol, SessionEvent, SessionSnapshot, SessionState, TrafficDirection,
};
pub use server::{
    FusedProxyDependencies, FusedProxyOptions, RunningServer, startFusedProxyServer,
    startSocks5Server, startSocks5ServerWithInterception,
    startSocks5ServerWithInterceptionAndResolver, startSocks5ServerWithPlugins,
};
