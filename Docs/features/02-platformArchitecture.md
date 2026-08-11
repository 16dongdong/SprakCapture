# 02 平台架构

## Charles 对照

Charles 将「代理入口」「录制」「工具」拆开：多个监听器把流量送入同一处理管线，工具按固定顺序作用，UI 只消费会话树与检查器。Sprak Capture 对齐该分层，并落到现有 `proxyService` 控制面。

## 目标

- 多数据面入口统一进入 capture + 工具流水线。
- 控制面继续：权威快照、`revision` 单调递增、camelCase、回环绑定。
- 工具可热启停与改配置；监听地址等数据面配置允许运行中更新，控制面会先强制关闭当前连接，再按新配置重启监听器。
- 库边界清晰，可单测：`http-proxy-core`、`capture-core`、`tool-pipeline`、现有 `socks5-core`。

## 非目标

- 不为每个工具单独起进程。
- 不在前端实现改包逻辑。
- 不破坏现有 SOCKS5 库 API 的可独立测试性。

## 分层

```text
UI (Web)
  ↓ HTTP/WS
Control API (proxyService)
  ↓ 命令 / 配置 / 查询
Runtime Orchestrator
  ├─ Listeners: HTTP · SOCKS5 · Reverse · PortForward
  ├─ Tool Pipeline (固定顺序)
  ├─ capture-core (RecordingSession / Transaction / BodyStore)
  └─ Outbound (直连 · 上游代理 · DNS)
```

### 建议 crate / 模块

| 模块 | 职责 |
|---|---|
| `socks5-core`（现有） | SOCKS5 协议与中继 |
| `http-proxy-core` | HTTP 正向代理、CONNECT 隧道、HTTP 解析 |
| `ssl-mitm` | 根 CA、叶证书缓存、TLS 终端 |
| `capture-core` | 录制会话、事务元数据、正文 spill |
| `tool-pipeline` | Location 匹配 + 工具钩子调度 |
| `proxy-backend` | 进程、控制 API、编排、配置持久化（用户目录） |
| `plugin-host` / `extension-kernel` / `plugin-runtime` | 插件基础宿主已存在；按 [38](38-pluginSystem.md) 重构为阶段内核、隔离运行时和贡献注册表 |
| Stream Codec 扩展 | capture 保留原始正文，插件通过录制阶段贡献解码树和标注，不覆盖原始字节 |

## 工具流水线

### 请求钩子（顺序固定）

```text
onClientRequest:
  1. accessControl
  2. dnsSpoof
  3. blockList
  4. noCaching
  5. blockCookies
  6. mapRemote
  7. mapLocal        // 命中可短路，生成合成响应
  8. rewriteRequest
  9. breakpointRequest  // 可挂起
 10. throttling
 11. forwardOutbound
```

### 响应钩子

```text
onServerResponse / onSyntheticResponse:
  1. rewriteResponse
  2. breakpointResponse
  3. blockCookiesResponse
  4. throttling
  5. mirror
  6. autoSave
  7. captureCommit
```

Map Local 短路时仍走响应钩子与录制，保证 UI 可见完整事务。

### 工具接口草案

```typescript
/** 工具在流水线中的稳定标识 */
type ToolId =
  | "accessControl"
  | "dnsSpoof"
  | "blockList"
  | "noCaching"
  | "blockCookies"
  | "mapRemote"
  | "mapLocal"
  | "rewrite"
  | "breakpoints"
  | "throttling"
  | "mirror"
  | "autoSave";

interface ToolRegistration {
  id: ToolId;
  /** 是否参与请求/响应阶段 */
  phases: Array<"request" | "response">;
  enabled: boolean;
}

/** 流水线上下文：可变请求/响应缓冲 + 只读元数据 */
interface PipelineContext {
  transactionId: string;
  recordingSessionId: string;
  clientAddress: string;
  processInfo?: ClientProcessInfo;
  location: ResolvedLocation;
  request: MutableHttpMessage;
  response?: MutableHttpMessage;
  /** 工具可设置：短路并使用本地/映射响应 */
  shortCircuit?: boolean;
  /** 工具可设置：阻断并返回错误状态 */
  blocked?: { reason: string; statusCode?: number };
  /** 断点挂起标记 */
  suspended?: boolean;
}

interface MutableHttpMessage {
  method?: string;
  url?: string;
  statusCode?: number;
  reason?: string;
  headers: Array<{ name: string; value: string }>;
  body: BodyHandle; // 引用 BodyStore，非列表内联
}
```

## 控制面扩展原则

1. 现有路径保留：`/api/v1/snapshot`、`service/start|stop`、`configuration`、`sessions`、`events`。
2. 新增资源使用 REST 风格子路径，例如 `/api/v1/recording`、`/api/v1/transactions`、`/api/v1/tools/{toolId}`。
3. 成功写操作返回**完整权威快照**或带 `revision` 的资源视图；失败返回中文错误。
4. 事件类型扩展：`transactions`、`recording`、`tools`、`breakpoints` 等；前后端在同一阶段同步严格判别联合。
5. 列表快照**不含**请求/响应正文。

### 权威快照扩展草案

```typescript
interface ServiceSnapshot {
  revision: number;
  serviceState: ServiceState;
  listeners: ListenerSnapshots;  // 多监听状态和实际地址
  metrics: ServiceMetrics;        // 扩展 HTTP 计数
  sessions: SessionSnapshot[];    // SOCKS 会话
  configuration: PublicConfiguration;
  recording: RecordingSnapshot;
  tools: ToolsPublicState;
}

interface ListenerSnapshots {
  socks5: ListenerSnapshot;
  httpProxy: ListenerSnapshot;
}
```

高频 `transactions` 事件可只推元数据增量；正文永不进入事件。

## 安装目录数据

| 数据 | 位置 |
|---|---|
| 根 CA 私钥/证书 | 安装目录 `/data/certs/` |
| 工具规则 JSON | 安装目录 `/data/configuration.json` |
| 录制自动保存 | 用户配置路径 |
| 运行日志 | 安装目录 `/data/logs/` |

源码树与安装目录只读业务配置默认值，不写私钥。

## UI 要点

- 工具总览显示流水线顺序与启停开关（只读顺序，不可拖拽改序）。

## UI 操作指南

架构与 UI 层级映射：

| 架构概念 | UI 层级 |
|---|---|
| 控制面连接状态 | L1 底栏 |
| 数据面启停 | L1 顶栏状态动作 |
| 录制/工具流水线配置 | L3 对话框（非 L2 全页） |
| 事务观察 | L2 连接会话 |
| 断点挂起协作 | L3/L4 断点编辑器 |

开发时禁止新增与连接会话平级的「工具全页路由」作为主配置入口。


## 验收标准

- [ ] HTTP 与 SOCKS5 可同时监听并各自产生可观察记录。
- [ ] 工具按文档顺序执行；单测可断言钩子调用序。
- [ ] 列表事件无 body；按需 API 可取 body。
- [ ] 控制 API 全 camelCase；`revision` 单调。
- [ ] 现有 SOCKS5 测试保持绿。

## 交叉链接

- [00](00-productVision.md) · [01](01-featureMatrix.md) · [03](03-locationMatching.md)
- [04](04-sessionAndRecording.md) · [05](05-transactionModel.md) · [09](09-httpHttpsProxy.md)
- [34](34-implementationRoadmap.md) · [controlContract](../controlContract.md)
