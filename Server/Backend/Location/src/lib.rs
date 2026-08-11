#![allow(non_snake_case)]

//! 提供代理、录制与后续工具共用的 Location 校验和匹配语义。

mod error;
mod matcher;
mod model;

pub use error::LocationError;
pub use matcher::{locationMatches, validateLocationPattern};
pub use model::{LocationMatchOptions, LocationPattern, ResolvedLocation};
