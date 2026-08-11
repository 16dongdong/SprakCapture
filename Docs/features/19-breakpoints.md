# 19 Breakpoints（断点）

## Charles 对照

Breakpoints 在请求或响应阶段暂停，弹出编辑器允许修改后执行/中止。可按 Location 与查询方向配置。

## 目标

- 支持请求断点、响应断点。
- 挂起事务进入 `suspended` 队列，UI 编辑 headers/body/status/URL 后 resume 或 abort。
- 超时自动放行或中止（可配），防止连接卡死。
- 最大同时挂起数限制。

## 非目标

- 不做条件表达式语言（status>400 等）一版可用简单状态码过滤 P2。
- 不在无 UI 连接时永久挂起（超时策略必须有）。

## 领域模型

```typescript
interface BreakpointRule {
  id: string;
  enabled: boolean;
  location: Location;
  onRequest: boolean;
  onResponse: boolean;
}

interface BreakpointsConfiguration {
  enabled: boolean;
  rules: BreakpointRule[];
  /** 挂起超时秒，默认 120 */
  suspendTimeoutSeconds: number;
  maxSuspended: number; // 默认 32
  onTimeout: "continue" | "abort";
}

type BreakpointAction = "continue" | "abort";

interface SuspendedBreakpoint {
  breakpointId: string;
  transactionId: string;
  phase: "request" | "response";
  suspendedAtMilliseconds: number;
  expiresAtMilliseconds: number;
  /** 可编辑快照 */
  draft: EditableHttpMessage;
}

interface EditableHttpMessage {
  method?: string;
  url?: string;
  statusCode?: number;
  reason?: string;
  headers: Array<{ name: string; value: string }>;
  bodyBase64: string;
}
```

## 行为

1. 命中规则 → 填充 `SuspendedBreakpoint`，事件通知 UI。
2. `POST resume` 带 draft → 写回 pipeline 继续。
3. `abort` → 客户端连接关闭或 5xx。
4. 超时按 `onTimeout`。
5. `flags.breakpointHit = true`。
6. 服务停止时全部 abort 或 continue（推荐 abort 并清理）。

## 控制 API

```http
GET  /api/v1/tools/breakpoints
PUT  /api/v1/tools/breakpoints
GET  /api/v1/breakpoints/suspended
POST /api/v1/breakpoints/suspended/{transactionId}/continue
POST /api/v1/breakpoints/suspended/{transactionId}/abort
```

`continue` body：`EditableHttpMessage`。

事件：`breakpoints` 携带挂起列表增量。

## UI 要点

- 挂起时模态或侧栏编辑器（Headers + Body Text）。
- 工具栏显示挂起计数角标。
- 快捷键：Continue / Abort。

## UI 操作指南

### 界面位置

| 规则配置 | L3：**工具 → 断点…** |
| 总开关 | L1 顶栏断点；菜单可同步勾选 |
| 命中编辑 | 专用 L3/L4 **断点** 编辑器（高于普通设置对话框） |
| 快捷加规则 | 右键 **断点…** |

### 如何打开

- 配规则：工具 → 断点…  
- 命中时：自动弹出断点编辑器（无需用户先找菜单）

### 操作步骤

#### 配置规则

1. 打开断点对话框 → 添加规则（Location + 请求/响应阶段）。
2. 应用/确定。
3. 顶栏打开断点总开关。

#### 处理命中

1. 自动出现断点编辑器：左队列右草稿。
2. 改头/体 → **继续** 或 **中止**。
3. 关闭后回到 L2；新事务状态已更新。

### 预期行为

| 场景 | 预期 |
|---|---|
| 命中时正开着 Map 对话框 | 断点编辑器盖在其上 |
| 角标 | 等于挂起数；点击可聚焦断点队列 |


## 验收标准

- [ ] 请求断点可改 URL 再发送。
- [ ] 响应断点可改状态码与体。
- [ ] 超时后不泄漏挂起槽位。
- [ ] 超 maxSuspended 新命中按 continue 并打日志/metrics。

## 交叉链接

- [18](18-rewrite.md) · [07](07-requestResponseViewers.md) · [02](02-platformArchitecture.md)
- [05](05-transactionModel.md)
