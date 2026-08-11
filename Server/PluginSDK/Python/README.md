# Python 插件 SDK

该 SDK 把插件开发压缩为普通函数注册。事件、动作、TCP 双向半包、UDP 数据报、解密/加密与重封包都有稳定类型和本地模拟器。

## 最短插件

```python
from capturePluginSdk import Stage, definePlugin, modifyBytes, serve

plugin = definePlugin()

@plugin.on(Stage.UDP_DATAGRAM)
def rewrite(event):
    return modifyBytes(event, event.bytes.replace(b"old", b"new"))

if __name__ == "__main__":
    serve(plugin)
```

`plugin.json` 使用 `runtime.kind: "sidecar"`、`protocolVersion: "2.0"` 和相对 `.py` 入口。生产 Host 按扩展名启动 Python，并通过冻结的 Sidecar JSONL 与 `serve(plugin)` 通信；`.py` 不会被伪装成 Native DLL。

## JSONL

- Host → worker：`initialize`、`invoke`、`stop`
- worker → Host：`ready`、`result`、`error`
- 标准输出只允许协议 JSON；作者日志写入标准错误。

`serve(plugin)` 完成全部协议分派。作者处理函数只接收 `Event` 并返回 `Action`。

默认 `serve(plugin)` 按到达顺序串行调用，最适合有状态协议。作者确认实现可并发后可显式使用：

```python
serve(plugin, concurrentInvocations=True)
```

并发模式允许多个 `invoke` 同时在途，结果可按完成时间乱序返回，但始终以 JSON number 原样保留 Host 的
`requestId`。Process JSONL 中的数字 ID、代际和 deadline 位于 `0..9007199254740991`，SDK 对越界
`requestId` 明确拒绝，避免与 JavaScript 插件产生不同的整数解释。互斥写入保证每条 stdout JSON 帧
完整；收到 `stop` 后先等待全部作者任务，再且仅调用一次 `@plugin.onStop` 生命周期。Host 不注入线程数或并发上限。

## 二进制 TCP 协议

```python
pipeline = StreamPipeline(
    lambda connectionId, direction: LengthPrefixedCodec(prefixBytes=2),
    lambda connectionId, direction: MyCipher(connectionId, direction),
)
plugin.tcp(pipeline, lambda frame, event: rewrite(frame.payload))
```

SDK 按 `connectionId + direction` 隔离缓冲、编解码器和密码状态。半帧返回 `hold`，完整帧执行 `decode → decrypt → 作者函数 → encrypt → encode`，长度由编解码器重算。UDP 使用 `plugin.udp(function)`，每个数据报保持原子边界。

## 安装与测试

```powershell
python -m pip install -e Server/PluginSDK/Python
python -m unittest discover -s Server/PluginSDK/Python/tests -p "test*.py"
```

也可直接运行夹具：

```powershell
Set-Location Server/PluginSDK/Python
capture-plugin-python examples.binaryProtocol.plugin:plugin examples/binaryProtocol/invocation.json
```

完整示例位于 `examples/binaryProtocol`。
