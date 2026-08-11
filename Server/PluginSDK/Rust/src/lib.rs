#![allow(non_snake_case)]

//! 为 Rust 插件作者封装 Sprak Capture Native ABI v2、阶段事件、动作和二进制分帧。
//!
//! 作者只需构造 [`Plugin`] 并用 [`exportPlugin!`] 导出工厂。SDK 负责校验 ABI、解析 JSON、
//! 捕获跨 FFI panic、管理停止/销毁生命周期以及恰好一次释放宿主读取后的输出缓冲区。

#[doc(hidden)]
pub mod nativeAbi;
mod packet;
mod protocol;

pub use nativeAbi::{InitContext, Plugin, PluginBuilder, PluginError};
pub use packet::{LengthPrefixedFrames, PacketError, joinPackets, splitPackets};
pub use protocol::{
    Action, ActionKind, BinaryEvent, EventEnvelope, Invocation, Stage, StageContext,
};

/// 为插件工厂生成固定的 `capture_extension_init` Native ABI v2 导出入口。
///
/// 工厂可以是普通函数或无捕获闭包，签名为
/// `fn(InitContext) -> Result<Plugin, PluginError>`。宏生成的入口不允许 panic 穿越 C ABI；
/// 初始化失败通过非零状态码返回，宿主不会观察到半初始化实例。
#[macro_export]
macro_rules! exportPlugin {
    ($factory:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn capture_extension_init(
            request: *const $crate::nativeAbi::NativeExtensionInitRequest,
            exports: *mut $crate::nativeAbi::NativeExtensionExports,
        ) -> i32 {
            unsafe { $crate::nativeAbi::initializePlugin($factory, request, exports) }
        }
    };
}
