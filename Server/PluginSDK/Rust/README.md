# Sprak Capture Rust Plugin SDK

该 crate 对接宿主现有 Native ABI v2。插件作者只需要：

1. 用普通函数或闭包构造 `Plugin`；
2. 对 `Invocation` 返回 `Action`；
3. 调用一次 `exportPlugin!(factory)`。

SDK 统一处理 `capture_extension_init`、并发 invoke、stop/destroy、panic 边界、JSON 输出所有权
和释放回调。事件正文不会被 SDK 截断或合并。

## 最小插件

```rust
use sprak_plugin_sdk::{Action, InitContext, Plugin, PluginError};

fn create(_: InitContext) -> Result<Plugin, PluginError> {
    Ok(Plugin::new(|event| Ok(Action::continueEvent(event))))
}

sprak_plugin_sdk::exportPlugin!(create);
```

## 二进制流

- `BinaryEvent::fromInvocation`：读取 TCP、UDP、WebSocket 或正文块的完整 `bytes`。
- `BinaryEvent::modify`：用普通闭包生成 `modify` 动作。
- `splitPackets` / `joinPackets`：按上限分包并按序合包。
- `LengthPrefixedFrames`：跨 TCP chunk 解析并重封装 4 字节大端长度前缀帧。

UDP 的一次 `udpDatagram` 调用对应一个完整数据报。若应用层协议需要拆分，插件必须用自身协议字段
标记顺序并在对端重组；SDK 不会伪造 IP 分片。

## 动作构造器

`Action` 提供 `continueEvent`、`modifyPayload`、`modifyBytes`、`hold`、`dropEvent`、
`reject`、`close`、`annotate`、`redirect` 和 `respond`。这些构造器自动绑定当前 `eventId`，
并在 SDK 边界校验终止原因、重定向目标以及二进制 payload 结构。

## 验证

```powershell
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```
