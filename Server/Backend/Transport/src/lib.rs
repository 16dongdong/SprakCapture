#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

//! 统一数据面的直接连接、HTTP CONNECT 与 SOCKS5 二级代理建连。

mod connector;

pub use connector::{
    OutboundConnectError, OutboundConnector, UpstreamProxyConfiguration, UpstreamProxyProtocol,
    validateUpstreamProxy,
};
