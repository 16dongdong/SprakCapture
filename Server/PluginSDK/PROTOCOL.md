# Plugin SDK v2 线上协议

本文冻结所有语言 SDK 与宿主之间的两种生产契约。语言 SDK 可以提供更符合自身习惯的类、函数、装饰器
或宏，但不得改变这里的字段、所有权和生命周期语义。

## Native ABI v2

动态库必须导出 C 符号：

```c
int32_t capture_extension_init(
    const CaptureExtensionInitRequest* request,
    CaptureExtensionExports* exports);
```

初始化输入：

```c
struct ByteSlice { const uint8_t* pointer; size_t length; };
struct CaptureExtensionInitRequest {
    uint32_t apiVersion;
    ByteSlice manifest;
    ByteSlice configuration;
};
```

导出表：

```c
struct NativeExtensionBuffer {
    const uint8_t* pointer;
    size_t length;
    void* releaseContext;
    void (*release)(void*, const uint8_t*, size_t);
};

struct CaptureExtensionExports {
    uint32_t apiVersion;
    void* pluginContext;
    int32_t (*invoke)(void*, ByteSlice, NativeExtensionBuffer*);
    void (*stop)(void*);
    void (*destroy)(void*);
};
```

`invoke` 输入是 UTF-8 JSON `RuntimeInvocation`，输出是 UTF-8 JSON `ExtensionAction`。插件拥有输出缓冲；
`release` 非空时，宿主复制后恰好调用一次，SDK 必须替作者实现这一所有权规则。`release` 为空表示静态或
实例期缓冲，插件必须让它至少保持到下一次约定写入或 `destroy`，不得返回临时栈内存。`invoke` 可以被
多个连接并发调用，同步策略由插件作者决定。宿主停止时按 `stop → destroy → unload` 调用。

## Process JSONL v2

`sidecar` 与 `nativeWorker` 使用 UTF-8、每行一个 JSON 对象的双向协议。标准输入是 Host→worker，标准
输出是 worker→Host。每帧写完必须换行并刷新；日志只能写标准错误。

初始化：

```json
{"type":"initialize","apiVersion":2,"manifest":{},"configuration":{}}
{"type":"ready","apiVersion":2}
```

调用：

```json
{"type":"invoke","requestId":1,"invocation":{}}
{"type":"result","requestId":1,"action":{}}
```

单次调用失败：

```json
{"type":"error","requestId":1,"message":"作者定义的错误"}
```

停止：

```json
{"type":"stop"}
```

请求可以并发在途，worker 可以乱序返回，但 `requestId` 必须原样返回。宿主不施加超时、队列容量或
输出配额；SDK 可以按作者显式配置选择串行、线程池、协程或自定义调度器。

Process JSONL 为保证 Python、JavaScript 等语言都能无损解析，所有以 JSON number 传输的数字 ID、
代际和 deadline 必须位于 `0..9007199254740991`。Host 在进入进程边界前把无期限 deadline 规范为该
上限；代际或 `requestId` 超域时明确拒绝，不允许静默舍入。Native ABI 不经过这一 JSONL 互操作边界，
仍保留完整 `u64` 数值域。初始化 `manifest` 与 `configuration` 中的整数同样必须处于
`-9007199254740991..9007199254740991`；Host 在启动 worker 前递归检查并拒绝超域配置。JSON 浮点数
继续采用 IEEE-754 近似语义，精确大整数应由作者在配置 Schema 中声明为十进制字符串。

## RuntimeInvocation

```json
{
  "pluginId": "example.protocol",
  "moduleId": "transform",
  "moduleKind": "streamTransformer",
  "envelope": {
    "apiVersion": "2.0.0",
    "eventId": "1:42",
    "stage": "tcpChunk",
    "serviceGeneration": 7,
    "recordingGeneration": 3,
    "pluginInstanceId": "example.protocol@1.0.0#1",
    "connectionId": "1",
    "transactionId": null,
    "deadlineUnixMs": 9007199254740991,
    "context": {
      "transport": "tcp",
      "protocol": "tcp",
      "direction": "up",
      "host": "example.com",
      "port": 9000,
      "interceptionMode": "intercept"
    },
    "payload": { "bytes": [1, 2, 3], "endOfStream": false }
  }
}
```

## ExtensionAction

```json
{
  "eventId": "1:42",
  "action": "modify",
  "patch": [],
  "annotations": [{"protocol":"example"}],
  "output": {"bytes":[9,8,7]}
}
```

动作必须引用当前 `eventId`。执行 `modify` 时，Host 先用 `output` 替换完整 payload，再按数组顺序把
`patch` 应用到替换结果；未提供 `output` 时，`patch` 直接应用到当前 payload。两者同时存在时必须确保
patch 路径适用于新的 output 结构。SDK 的 `modifyBytes` 推荐使用根路径 JSON Patch，只替换 payload
并保留事件元数据；需要一次提供完整 payload 时才使用 `output`。`bytes` 的所有元素必须是 0..255 的整数。
`hold` 表示保留当前半包并等待后续块，`drop` 丢弃当前块，`close` 关闭连接，其他阶段动作与
`Docs/pluginHookApi.md` 保持一致。

## SOCKS5 插件认证

服务配置为 `authenticationMode: "plugin"` 时，SOCKS5 仍使用 RFC1929 用户名密码方法，但凭据判定完全交给订阅 `socks5Authentication` 阶段的插件。事件 `payload` 为 `{ "username": string, "password": string }`，连接地址位于 `context.address`。口令仅用于本次同步调用，不进入事务、日志或事件广播。

插件用 `respond({"principalId":"用户主体"})` 接受认证；`reject(...)` 或 `close()` 拒绝；全部插件只返回 `continue`、无订阅插件、空主体、非法动作或运行时失败均按拒绝处理。认证通过后的 `principalId` 会写入 SOCKS5 会话身份，并同时适用于 CONNECT、BIND 与 UDP ASSOCIATE。
