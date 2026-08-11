#![allow(non_snake_case)]

use sprak_plugin_sdk::{BinaryEvent, InitContext, Plugin, PluginError};

/// 注册一个同时处理 TCP chunk 与 UDP datagram 的普通闭包；示例把 ASCII 小写转为大写。
fn createPlugin(_: InitContext) -> Result<Plugin, PluginError> {
    Ok(Plugin::new(|invocation| {
        let binary = BinaryEvent::fromInvocation(invocation).map_err(PluginError::new)?;
        binary
            .modify(invocation, |mut bytes| {
                bytes.make_ascii_uppercase();
                bytes
            })
            .map_err(PluginError::new)
    }))
}

sprak_plugin_sdk::exportPlugin!(createPlugin);
