# Sprak Capture Go Plugin SDK

Go SDK 同时支持：

- `c-shared` Native ABI：作者只注册普通工厂/闭包，生成器封装固定导出入口与 C 内存释放；
- JSONL worker：`RunJSONL` 完整处理 initialize、invoke、result/error、stop 生命周期。

## Native 最小流程

```go
func init() {
    pluginsdk.Register(func(init pluginsdk.InitContext) (pluginsdk.Plugin, error) {
        return pluginsdk.Plugin{Handle: myHandler}, nil
    })
}
```

```powershell
go run sprakcapture/plugin-sdk-go/cmd/sprak-plugin-gen -output .
go build -buildmode=c-shared -o plugin.dll .
```

生成桥负责 `capture_extension_init`、并发 invoke、stop/destroy、动作 JSON、`C.malloc/free`，作者
不接触 C 指针。桥限制单个 ABI 切片不超过 `MaxInt32`，超过时在进入 Go 内存前明确失败。

## 二进制与分帧

- `ParseBinaryEvent` / `ModifyBytes`：读取并完整替换 TCP、UDP、WebSocket 或正文块。
- `SplitPackets` / `JoinPackets`：按上限分包与按序合包。
- `LengthPrefixedFrames`：跨 TCP chunk 解析并重封装 4 字节大端长度前缀帧。

UDP 调用对应完整数据报。应用层拆包必须携带自身顺序字段；SDK 不伪造 IP 分片，也不截断正文。

## 动作构造器

SDK 提供 `Continue`、`ModifyPayload`、`ModifyBytes`、`Hold`、`Drop`、`Reject`、`Close`、
`Annotate`、`Redirect` 和 `Respond`。构造器自动绑定当前 `eventId`，并在进入宿主前校验原因、
重定向目标和 JSON 可编码性。

## JSONL worker

```go
err := pluginsdk.RunJSONL(context.Background(), os.Stdin, os.Stdout)
```

协议与宿主一致：initialize→ready、invoke→result/error、stop。实现使用流式 `json.Decoder`，不受
`bufio.Scanner` 固定行长影响。`requestId` 按宿主契约使用 `0..9007199254740991` 的 JSON 安全整数，
包括零值在内都会原样返回；`deadlineUnixMs` 仅作为作者可读取的调度参考，SDK 不会据此自动取消处理器。

默认 `RunJSONL` 串行调用作者处理器，适合最简单的有状态插件。需要并行处理多个连接时，调用
`RunJSONLWithOptions` 并设置 `WorkerOptions{MaxConcurrentInvocations: N}`；SDK 会限制并发数、
串行写出完整 JSONL 响应，并在 stop 前等待所有在途调用结束。

## 验证

```powershell
go test ./...
go vet ./...
```
