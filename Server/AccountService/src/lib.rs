#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

//! 提供 SprakCapture 多账号模式独占使用的账号存储、认证租约和管理 HTTP 边界。

mod credential;
mod error;
mod http;
mod lease;
mod model;
mod ruleSetStore;
mod service;
mod store;

pub use error::{AccountServiceError, Result};
pub use http::{AccountServerConfig, RunningAccountService, startAccountService};
pub use model::*;
pub use service::AccountDomainService;
pub use store::AccountStore;
