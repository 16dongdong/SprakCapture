# 插件与模块开发 API

本文定义 Sprak Capture 完整插件/模块平台的公开开发契约。插件作者只依赖本文、SDK 和版本化 Schema，不依赖宿主内部 Rust 类型、私有路由或数据库结构。

当前代码中的旧版 Native 流 ABI 是兼容适配层；新插件应使用 [`Server/PluginSDK`](../Server/PluginSDK/README.md) 提供的 Native ABI 或 Process JSONL。完整实现状态与迁移边界见 [插件系统现状与迁移审查](pluginSystemAudit.md)，系统架构见 [38 插件与模块系统](features/38-pluginSystem.md)，从创建到发布的工作流见 [插件与 Mod 开发者体验规范](pluginDeveloperGuide.md)。

## 1. 基本概念

| 名称 | 含义 |
|---|---|
| 插件包 | 可安装、升级、回滚和卸载的 `.tplugin.zip` 包 |
| 模块 | 插件包提供的一个独立功能单元，可订阅阶段、注册命令或贡献 UI |
| 插件实例 | 某个插件版本在一个服务代际中的运行实体 |
| 阶段 | 宿主在稳定处理边界发布的版本化事件 |
| 动作 | 插件对当前阶段返回的结构化决定 |
| 能力 | 插件声明自己会使用的公开接口类别，仅用于说明和诊断 |
| 贡献点 | 插件注册的设置、命令、检查器页签、上下文动作或解码器 |

同一插件包可以包含多个模块，并共享版本、配置目录和升级事务。进程内 Native 模块可自由共享内存；其他运行时可使用作者自定义 IPC，也可以使用宿主消息总线。

模块类型不是运行时类型。同一个 Wasm 或 sidecar 插件可以同时提供多个模块：

| 模块类型 | 主要职责 |
|---|---|
| `trafficHandler` | 在连接、TLS、HTTP、WebSocket、TCP、UDP、DNS 阶段观察或干预 |
| `protocolDecoder` | 将原始字节解释为结构化帧、字段和应用层协议 |
| `recordingPolicy` | 决定是否录制、仅录元数据、添加标签或生成派生视图 |
| `bodyViewer` / `mediaRenderer` | 为特定 MIME、文件签名或协议提供检查器展示 |
| `importer` / `exporter` | 导入或导出版本化会话格式 |
| `commandProvider` | 注册可被 UI、CLI、MCP 和自动化调用的命令 |
| `uiContribution` | 注册设置、页签、上下文动作、徽标和状态项 |
| `backgroundService` | 维护索引、缓存、外部集成或自定义监听器；生命周期由插件自行实现 |

## 2. 插件包

```text
example.protocol.tplugin.zip
├─ plugin.json
├─ README.md
├─ LICENSE
├─ schemas/
│  ├─ configuration.schema.json
│  └─ messages.schema.json
├─ dist/
│  ├─ plugin.wasm
│  └─ worker.exe
└─ ui/
   └─ contributions.json
```

包根必须只有一个 `plugin.json`。路径必须是 UTF-8 相对路径，不允许符号链接、设备路径、绝对路径和 `..`。安装器在暂存目录校验清单、路径、入口和 Schema 后，才原子替换正式版本；这些检查只保证包结构可加载，不限制插件运行能力。

### 2.1 manifest 清单

```json
{
  "manifestVersion": 2,
  "id": "example.protocol",
  "name": "协议扩展",
  "description": "解析并标注示例协议",
  "version": "2.1.0",
  "publisher": "example",
  "engines": {
    "host": ">=2.0.0 <3.0.0",
    "api": "2.x"
  },
  "runtime": {
    "kind": "wasm",
    "entry": "dist/plugin.wasm"
  },
  "modules": [
    {
      "id": "protocolDecoder",
      "kind": "protocolDecoder",
      "subscriptions": [
        {
          "stage": "responseBodyChunk",
          "order": 200,
          "match": {
            "schemes": ["https"],
            "hosts": ["*.example.com"],
            "methods": ["GET"]
          }
        }
      ]
    }
  ],
  "capabilities": [
    "traffic.observe",
    "capture.annotate",
    "ui.inspector"
  ],
  "dependencies": {
    "example.sharedSchema": "^3.2.0"
  },
  "limits": {
    "timeoutMs": 50,
    "maxPendingEvents": 128,
    "maxOutputBytes": 1048576,
    "maxStorageBytes": 67108864
  },
  "configurationSchema": "schemas/configuration.schema.json",
  "contributes": "ui/contributions.json"
}
```

未知字段默认拒绝。只有 `extensions` 对象允许插件保存命名空间字段；宿主不会解释其中内容。

依赖解析只接受插件 ID 与语义版本范围，不执行安装脚本。宿主生成锁文件并固定最终版本、包摘要和发布者；循环依赖、运行时不兼容和发布者替换均在启用前拒绝。

### 2.2 运行时

| `runtime.kind` | 用途 | 约束 |
|---|---|---|
| `wasm` | 协议解析、规则、事务标注或作者选择的可移植实现 | 调度、内存和并发策略由作者实现 |
| `sidecar` | Node.js、Python、Go 或任意外部程序 | 进程、IPC、网络和文件访问由作者决定 |
| `nativeWorker` | 高频流处理、已有 C/C++/Rust 库 | 在独立工作进程装载本地库 |
| `native` | 最高性能、需要直接控制宿主进程行为的 Mod | 动态库直接进入宿主进程，与宿主共享权限和故障域 |
| `legacyNative` | 旧版连接/字节流插件 | 保留兼容，也可与完整模块同时安装 |

插件作者可自由选择任意运行时。运行时入口、协议版本和平台文件仍在安装阶段验证，避免把缺文件的包标记为可运行。

## 3. 事件信封

所有阶段共享以下逻辑字段；具体 SDK 可以映射为结构体、WIT 类型或 RPC 消息：

```json
{
  "apiVersion": "2.0",
  "eventId": "01J...",
  "stage": "requestHeaders",
  "serviceGeneration": 42,
  "recordingGeneration": 7,
  "pluginInstanceId": "example.protocol@2.1.0#42",
  "connectionId": "c_...",
  "transactionId": "t_...",
  "deadlineUnixMs": 1780000000000,
  "context": {},
  "payload": {}
}
```

- `eventId` 在宿主生命周期内唯一，用于幂等响应和诊断。
- `serviceGeneration` 防止旧插件实例影响重启后的连接。
- `recordingGeneration` 防止清空后的异步标注重新出现。
- `deadlineUnixMs` 是宿主提供的时间参考；插件可以忽略、采用或替换自己的调度策略，宿主不据此终止调用。
- 不适用字段为 `null`，不能用空字符串伪造身份。

## 4. 稳定阶段

### 4.1 服务与配置

| 阶段 | 可用动作 | 说明 |
|---|---|---|
| `serviceStarting` | `continue`、`reject` | 监听器启动前验证模块条件 |
| `serviceStarted` | `continue`、`annotate` | 服务已可用，不允许回滚启动 |
| `configurationChanged` | `continue`、`reject` | 配置提交前验证，成功后发送只读确认事件 |
| `serviceStopping` | `continue` | 停止新调用并释放实例资源 |

### 4.2 连接与目标

| 阶段 | 可用动作 | 主要字段 |
|---|---|---|
| `connectionAccepted` | `continue`、`reject`、`annotate` | 客户端地址、进程、入口 |
| `protocolClassified` | `continue`、`modify`、`reject` | 候选协议、置信度、证据 |
| `targetResolving` | `continue`、`modify`、`reject` | 原始主机、地址族、DNS 上下文 |
| `beforeConnect` | `continue`、`redirect`、`reject` | 原始目标、最终目标、二级代理 |
| `connected` | `continue`、`annotate` | 实际地址、连接耗时、协商协议 |
| `connectionClosing` | `continue`、`annotate` | 关闭发起方、错误、流量计数 |

`redirect` 只改变最终目标；原始目标永远保留在录制和审计字段中。

### 4.3 TLS

| 阶段 | 可用动作 |
|---|---|
| `clientHelloObserved` | `continue`、`annotate`、`reject` |
| `certificateSelecting` | `continue`、`modify`、`reject` |
| `tlsEstablished` | `continue`、`annotate` |
| `tlsFailed` | `continue`、`annotate` |

插件不直接读取根证书私钥。证书选择动作只能引用宿主管理的证书标识。

### 4.4 HTTP

| 阶段 | 可用动作 |
|---|---|
| `requestHeaders` | `continue`、`modify`、`respond`、`reject`、`annotate` |
| `requestBodyChunk` | `continue`、`modify`、`hold`、`drop`、`reject` |
| `requestComplete` | `continue`、`respond`、`reject`、`annotate` |
| `beforeUpstream` | `continue`、`redirect`、`respond`、`reject` |
| `responseHeaders` | `continue`、`modify`、`reject`、`annotate` |
| `responseBodyChunk` | `continue`、`modify`、`hold`、`drop`、`close` |
| `responseComplete` | `continue`、`annotate` |

HTTP 头使用有序多值数组，保留重复字段；不得把 `Set-Cookie`、`ETag`、`Content-Range` 等字段错误合并。修改正文后由宿主统一处理 Content-Length、Transfer-Encoding、压缩和 HTTP 版本边界。

### 4.5 WebSocket、TCP、UDP 与 DNS

| 阶段 | 可用动作 |
|---|---|
| `webSocketOpening` | `continue`、`reject`、`annotate` |
| `webSocketFrame` | `continue`、`modify`、`drop`、`close` |
| `webSocketClosing` | `continue`、`annotate` |
| `tcpChunk` | `continue`、`modify`、`hold`、`drop`、`close` |
| `udpDatagram` | `continue`、`modify`、`drop`、`reject`、`annotate` |
| `dnsMessage` | `continue`、`modify`、`respond`、`reject`、`annotate` |

被动 WinDivert SNIFF 数据报会设置 `interceptionMode: observeOnly`，此时只允许 `continue`、`annotate` 和录制裁决；返回线上修改动作会得到 `actionNotSupportedForObservation`。

### 4.6 录制与展示

| 阶段 | 可用动作 |
|---|---|
| `beforeRecord` | `continue`、`drop`、`annotate` |
| `transactionUpdated` | `continue`、`annotate` |
| `transactionCompleted` | `continue`、`annotate` |
| `recordingCleared` | `continue` |
| `inspectorDataRequested` | `continue`、`annotate` |
| `commandInvoked` | 命令声明约定的结果 |
| `contextActionInvoked` | 上下文动作声明约定的结果 |

## 5. 标准动作

```json
{
  "eventId": "01J...",
  "action": "modify",
  "patch": [],
  "annotations": [],
  "output": null
}
```

| 动作 | 语义 |
|---|---|
| `continue` | 不改变当前阶段 |
| `modify` | 应用阶段允许的结构化补丁或输出块 |
| `hold` | 在预算内等待后续块；只适用于流式字节阶段 |
| `drop` | 丢弃当前数据块或取消本次录制 |
| `reject` | 返回协议正确的拒绝并结束当前操作 |
| `respond` | 生成完整合成响应，不连接上游 |
| `redirect` | 修改最终连接目标 |
| `annotate` | 添加标签、解码树或展示字段，不改变线上数据 |
| `close` | 关闭当前连接或关联 |

插件输出可以通过 SDK 构造器或作者自己的协议实现生成。宿主只复验事件身份、阶段动作和线上协议结构，不执行能力授权检查。

## 6. 匹配规则与顺序

订阅可匹配：入口、进程路径/名称、传输层、协议、方向、域名、IP/CIDR、端口、HTTP 方法、路径、状态码、MIME、WebSocket opcode、DNS 类型、事务标签。

- 字符串规范化和通配语义由公共 Schema 定义。
- 用户可以覆盖 manifest 默认启用状态、匹配条件和执行顺序。
- manifest 更新不得静默覆盖用户排序和配置。
- 同阶段按用户顺序、manifest 默认顺序、插件 ID、模块 ID稳定排序。
- `reject`、`respond`、`close` 终止当前阶段；前序修改仍写入审计记录。

## 7. Host API

### 7.1 能力说明表

| 能力 | 插件通常执行的行为 | 运行语义 |
|---|---|---|
| `traffic.observe` | 读取已订阅阶段的元数据和正文块 | 启用后直接可用 |
| `traffic.modify` | 修改允许干预阶段的结构化消息或字节 | 启用后直接可用 |
| `traffic.reject` | 拒绝请求、数据报或连接 | 启用后直接可用 |
| `connection.redirect` | 改写最终上游目标 | 启用后直接可用 |
| `http.respond` | 生成合成 HTTP 响应 | 启用后直接可用 |
| `capture.read` | 读取已录制事务 | 启用后直接可用 |
| `capture.annotate` | 添加标签、解码树和派生视图 | 启用后直接可用 |
| `capture.control` | 暂停、恢复或清空录制 | 启用后直接可用 |
| `configuration.read` | 读取插件配置 | 启用后直接可用 |
| `storage.readWrite` | 使用存储 | 启用后直接可用 |
| `network.outbound` | 发起任意出站请求 | 启用后直接可用 |
| `filesystem.pluginData` | 访问文件系统 | 启用后直接可用 |
| `commands.register` | 注册命令贡献 | 启用后直接可用 |
| `ui.inspector` | 注册检查器和查看器贡献 | 启用后直接可用 |
| `ui.workspace` | 注册独立工具页和状态贡献 | 启用后直接可用 |

能力清单接受作者自定义字符串并在安装页原样展示，不产生复选框、授权令牌或动作门禁。启用插件即允许其使用全部公开 Host API；进程内 Native 还天然拥有宿主进程可访问的操作系统资源。

### 7.2 上下文

- 读取当前事件、连接、事务、原始目标和最终目标。
- 读取插件公开配置和秘密引用；秘密明文不进入日志与快照。
- 读取插件私有会话值和持久键值。

### 7.3 行为

- 请求关闭当前连接。
- 创建宿主管理的正文输出块或合成响应。
- 添加事务标签、注释、解码节点、媒体描述和协议字段。
- 发布插件日志、指标和进度。
- 调用宿主命令。
- 发起出站请求；网络范围由插件作者决定。
- 读取本插件通过声明式依赖公开的只读服务，并发布有 Schema 的模块消息。

### 7.4 兼容性边界

- 宿主只对版本化公开 API 承诺兼容性；直接访问私有 Rust 布局、内部数据库或 DOM 的 Native Mod 需要自行跟随宿主版本变化。
- 被动 SNIFF 事件是副本，不能在事件返回值中改写已经放行的原包；插件可改用主动转发阶段实现线上修改。
- 进程内 Native 与宿主共享崩溃域；作者选择该模式即自行承担 ABI、线程和资源生命周期。

## 8. 配置与状态

每个插件有四类独立数据：

| 数据 | 文件/存储 | 生命周期 |
|---|---|---|
| 安装清单 | 版本目录，只读 | 随版本替换 |
| 用户启停和顺序 | 宿主插件配置文件 | 跨版本保留 |
| 插件配置 | Schema 校验的配置存储 | 跨重启保留，支持迁移 |
| 插件状态 | 私有存储 | 由插件作者决定容量、格式和迁移 |

配置写入流程为：校验新配置 → 插件验证 → 原子持久化 → 创建新实例 → 发布代际 → 回收旧实例。任一步失败都保留旧权威配置和旧运行实例。

## 9. UI 贡献

`contributes` 支持：

- `settings`
- `commands`
- `inspectorTabs`
- `transactionContextActions`
- `connectionBadges`
- `statusItems`
- `decoders`
- `bodyViewers`
- `mediaRenderers`
- `importers`
- `exporters`
- `workspacePanels`

贡献内容可以使用声明式 Schema、独立 WebView、独立窗口或 Native 自定义界面。声明式路径享有兼容保证；直接依赖主窗口 DOM 或私有状态的 Mod 由作者自行维护。

导入器可生成宿主事务草稿，也可使用 Native API 实现自定义存储。查看器和渲染器使用正文租约可获得稳定生命周期；绕过租约直接读取内部文件属于无兼容保证的作者自管路径。

## 10. 作者自管运行语义

- `limits` 只作为作者运行说明保留；宿主不执行并发、队列、超时、输出或存储配额。
- 插件可以选择同步热路径、异步任务、自建线程或子进程；作者负责对应性能和生命周期结果。
- Wasm/sidecar/worker 是否采用超时和故障隔离由作者决定；进程内 Native 崩溃会直接影响宿主。
- 宿主不自动熔断、降级或停用连续失败的插件，只记录调用失败供开发者诊断。
- 数据面按阶段配置 `failOpen` 或 `failClosed`；用户必须能看到实际策略。
- 禁用、重载和服务停止会停止新事件、取消旧调用、回收运行时和清理贡献点。
- 旧 `serviceGeneration` 或 `recordingGeneration` 的结果一律丢弃并计数。

## 11. 控制 API

完整控制面必须覆盖：

```text
GET    /api/v1/plugins
POST   /api/v1/plugins/packages
GET    /api/v1/plugins/{id}
PUT    /api/v1/plugins/{id}/enabled
PUT    /api/v1/plugins/{id}/configuration
PUT    /api/v1/extensions/configuration/{id}
PUT    /api/v1/plugins/{id}/subscriptions
POST   /api/v1/plugins/{id}/reload
POST   /api/v1/plugins/{id}/rollback
DELETE /api/v1/plugins/{id}
GET    /api/v1/plugins/{id}/diagnostics
GET    /api/v1/plugins/{id}/logs
POST   /api/v1/plugins/{id}/commands/{commandId}
```

桌面端、CLI、MCP 和自动化测试共用这些端点，不复制生命周期逻辑。

## 12. SDK 与兼容

官方 SDK 至少提供 Rust、C/C++、TypeScript 和 Python：

- 类型和 Schema 由同一接口描述生成。
- 提供本地宿主模拟器、固定事件夹具、包校验器和兼容性测试。
- API 使用语义版本；新增可选字段不破坏同一主版本，删除或改变语义必须提升主版本。
- 包可以声明 API 版本范围；宿主选择共同支持的最高版本。
- `legacyNative` 通过适配器只映射连接和原始流阶段，不能假装支持完整模块 API。
