# TypeScript 插件 SDK

该 SDK 让插件作者使用普通函数处理阶段事件、TCP 明文帧和 UDP 数据报，不需要手写 JSON 分派、JSON Patch、半包状态或长度重算。

## 最短插件

```ts
import { definePlugin, modifyBytes, serve } from "@sprak/capture-plugin-sdk";

const plugin = definePlugin();
plugin.on("udpDatagram", (event) =>
  modifyBytes(event, event.bytes().map((value) => value ^ 0x20)),
);

await serve(plugin);
```

TypeScript 先编译为 JavaScript。`plugin.json` 使用 `runtime.kind: "sidecar"`、`protocolVersion: "2.0"` 和相对 `.js`/`.mjs` 入口，生产 Host 按扩展名启动 Node，并通过冻结的 Sidecar JSONL 与 `serve(plugin)` 通信；JavaScript 不会被伪装成 Native DLL。

## Sidecar JSONL

- Host → worker：`initialize`、`invoke`、`stop`
- worker → Host：`ready`、`result`、`error`
- 每条消息独占一行并立即写出；`requestId` 原样关联。
- 标准输出只承载协议。作者诊断使用 `console.error`。

默认 `serve(plugin)` 串行等待每个作者函数，最适合有状态流协议。作者确认实现允许并发后可显式开启：

```ts
await serve(plugin, { concurrentInvocations: true });
```

并发模式允许多个 `invoke` 同时在途并按完成时间乱序返回，SDK 始终以 JSON number 原样关联当前 Host 从 1 递增的 `requestId`，并验证它属于非负 JavaScript 安全整数；超出范围时拒绝而不发生精度截断。异步写队列保证每条 stdout JSON 帧完整且处理管道背压。收到 `stop` 后会等待全部作者任务，再且仅执行一次 `plugin.onStop(...)` 生命周期。Host 不注入并发数量限制。

## 二进制流

```ts
const pipeline = new StreamPipeline(
  (connectionId, direction) => new LengthPrefixedCodec(2),
  (connectionId, direction) => new MyCipher(connectionId, direction),
);
plugin.tcp(pipeline, (frame, event) => rewrite(frame.payload));
```

每个 `connectionId + direction` 拥有独立半包、编解码器和密码状态，执行顺序固定为：

```text
增量分包 → 解密 → 作者函数 → 加密 → 重封包
```

UDP 使用 `plugin.udp(function)`，始终保留数据报边界。

## 构建与测试

```powershell
cd Server/PluginSDK/TS
npm install
npm test
```

构建会把完整的可加载示例生成到 `dist/examples/binaryProtocol`，其中 `plugin.json` 与其相对入口
`plugin.js` 位于同一目录，可将该目录直接交给 Host 加载。源码位于 `examples/binaryProtocol`；
`ManifestBuilder` 可生成 sidecar 清单，`Simulator` 可在代理服务之外运行固定夹具。
