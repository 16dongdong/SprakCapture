# Sprak Capture Plugin SDK

`PluginSDK` 是插件作者唯一需要依赖的开发入口。SDK 把 manifest、阶段事件、动作、内存释放、
请求关联和生命周期隐藏在语言适配器后面；作者只注册普通函数，函数参数是事件对象，返回值是动作对象。

## 选择语言

| 目录 | 生产运行方式 | 适用场景 |
|---|---|---|
| `C++/` | `native` | 最高吞吐、复用现有 C/C++ 协议库、直接控制宿主进程 |
| `Rust/` | `native` | 内存安全的高性能分包、解密、修改和重封包 |
| `Go/` | `native` 或 `nativeWorker` | 复用 Go 协议栈，按作者选择进程内或独立进程 |
| `Python/` | `sidecar` | 快速实现协议研究、自动化、算法和外部系统集成 |
| `TS/` | `sidecar` | 使用 Node.js 生态实现协议、规则和工具集成 |

所有 SDK 使用同一 [SDK 协议](PROTOCOL.md)、插件清单、阶段名称和动作语义。宿主不执行能力授权、
调用超时、并发上限、输出配额、存储配额或自动熔断；插件作者自行决定线程、队列、缓存和失败策略。
`schemas/` 是由 Host 类型直接生成的权威 JSON Schema，语言 SDK 的模型和 CI 校验必须以它为准。

## 最短开发流程

1. 进入目标语言目录，复制 `examples/` 中最接近需求的插件。
2. 在普通函数中读取 `event.payload` 或 SDK 提供的 `bytes()`。
3. 返回 `continue`、`modify`、`hold`、`drop`、`reject`、`respond`、`redirect`、`annotate` 或 `close`。
4. 构建后把 `plugin.json` 与入口文件放入 `{dataDirectory}/plugins/<pluginId>/`。
5. 在控制 API 或插件管理界面启用插件；修改配置会热替换运行实例。

一个二进制流插件的业务代码应接近以下形式：

```text
onTcpChunk(event):
    frames = decoder.push(event.bytes())
    if frames 还不完整:
        return hold()
    decoded = decrypt(frames)
    changed = authorFunction(decoded)
    return modify(encryptAndPack(changed))
```

分包状态以 `connectionId + direction` 为键保存。SDK 提供流缓冲和长度前缀编解码器；私有协议的边界、
密码学、序列号、校验和与重放语义由插件作者实现，宿主不会改写插件返回的合法字节。

## 插件包最小结构

```text
example.mod/
├─ plugin.json
├─ README.md
└─ dist/
   └─ <语言入口>
```

清单示例：

```json
{
  "manifestVersion": 2,
  "id": "example.binary-protocol",
  "name": "二进制协议 Mod",
  "description": "分包、解密、修改并重封包",
  "version": "1.0.0",
  "publisher": "example",
  "engines": { "host": ">=1.0.0", "api": "2.x" },
  "runtime": {
    "kind": "native",
    "entry": "dist/plugin.dll",
    "protocolVersion": "2.0"
  },
  "modules": [{
    "id": "protocol",
    "kind": "streamTransformer",
    "subscriptions": [{
      "stage": "tcpChunk",
      "order": 100,
      "match": { "ports": [9000] }
    }]
  }],
  "capabilities": ["author.binary-protocol"]
}
```

Python 与 TypeScript 把 `runtime.kind` 改为 `sidecar`，入口分别指向 `.py` 和构建后的 `.js`；独立 Go
进程使用 `nativeWorker`。宿主会按照运行方式加载入口，不需要作者编写 IPC、请求 ID 或内存释放代码。

## 稳定性边界

- Native 插件与宿主共享地址空间、权限和故障域，可以调用任意系统 API。
- Sidecar/Native Worker 是作者选择的进程边界，不是权限沙箱；子进程继承宿主可授予的系统权限。
- 标准输出属于 JSONL 协议通道，进程插件日志必须写入标准错误。
- 事件的 `serviceGeneration`、`recordingGeneration` 和 `eventId` 必须由 SDK 原样回传。
- `observeOnly` 事件对应已经放行的副本，物理上只能观察和标注，不能修改已发送的数据包。

更完整的阶段和动作说明见 [`Docs/pluginHookApi.md`](../Docs/pluginHookApi.md)，插件架构和生命周期见
[`Docs/features/38-pluginSystem.md`](../Docs/features/38-pluginSystem.md)。
