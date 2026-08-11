# 21 无缓存（No Caching）

## Charles 对照

No Caching 工具通过移除或改写缓存相关请求/响应头，迫使客户端与服务器每次走完整交互，便于调试。

## 目标

- 启用后对匹配 Location 的流量剥离或重写缓存头。
- 请求方向与响应方向均可处理。

## 非目标

- 不实现本地缓存代理的反向能力。
- 不修改 HTML 内 meta 缓存标签（仅 HTTP 头）。

## 领域模型

```typescript
interface NoCachingConfiguration {
  enabled: boolean;
  locations: Location[]; // 空 = 全部
  /** 请求阶段处理 */
  stripRequestHeaders: boolean;
  /** 响应阶段处理 */
  stripResponseHeaders: boolean;
  /** 强制请求头 Cache-Control: no-cache */
  injectRequestNoCache: boolean;
  /** 强制响应头禁止缓存 */
  injectResponseNoStore: boolean;
}

/** 默认剥离头名 */
const DEFAULT_REQUEST_STRIP = [
  "If-Modified-Since",
  "If-None-Match",
  "Cache-Control",
  "Pragma",
];

const DEFAULT_RESPONSE_STRIP = [
  "Expires",
  "Cache-Control",
  "Pragma",
  "ETag",
  "Last-Modified",
  "Age",
];
```

## 行为

1. 流水线位置：Block List 之后、Block Cookies 之前（请求）；响应阶段在 Rewrite 前或后——**固定在 blockCookies 响应附近之前/按 [02] 总序：请求 noCaching 在 blockCookies 前**。
2. 剥离头名大小写不敏感。
3. inject 时写入：
   - 请求：`Cache-Control: no-cache`、`Pragma: no-cache`
   - 响应：`Cache-Control: no-cache, no-store, must-revalidate`、`Pragma: no-cache`、`Expires: 0`
4. 不改变 body。

## 控制 API

```http
GET /api/v1/tools/noCaching
PUT /api/v1/tools/noCaching
```

## UI 要点

- 简单开关 + Location + 四个复选行为。
- 说明文案：可能导致服务器负载上升。

## UI 操作指南

### 界面位置

**工具 → 无缓存…** 小对话框（启用 + 位置范围）。

### 如何打开

菜单；或右键主机「无缓存」打开并预填位置。

### 操作步骤

1. 勾选启用；选全部或选定位置。
2. 选定位置时添加… Location。
3. 确定后重复请求，检查器可见缓存头被改。

### 预期行为

单一开关型 L3，无独立全页。


## 验收标准

- [ ] 启用后请求不再携带 If-None-Match。
- [ ] 响应带 no-store。
- [ ] Location 过滤生效。
- [ ] 关闭后头保持原样。

## 交叉链接

- [02](02-platformArchitecture.md) · [03](03-locationMatching.md) · [22](22-blockCookies.md)
