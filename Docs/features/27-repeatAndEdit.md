# 27 重复与编辑（Repeat / Edit）

## Charles 对照

- **Repeat**：重新发送选中请求。
- **Compose / Edit**：编辑方法、URL、头、体后发送，生成新事务。

## 目标

- 从已有 Transaction 一键 Repeat（原样重放请求）。
- Compose 编辑器创建/编辑请求并发送，经代理出站（或直连配置）。
- 重放走完整工具流水线与录制。

## 非目标

- 不做浏览器 cookie 罐自动填充的完整浏览器仿真。
- 第一版不重放 WebSocket 帧流。

## 领域模型

```typescript
interface ComposeRequest {
  method: string;
  url: string; // 绝对 URL
  headers: Array<{ name: string; value: string }>;
  bodyBase64: string;
  /** 是否经当前 HTTP 代理监听自举；默认 true 以便工具生效 */
  viaProxy: boolean;
}

interface RepeatRequest {
  transactionId: string;
  /** 覆盖字段可选 */
  overrides?: Partial<ComposeRequest>;
}

interface ComposeResult {
  transactionId: string;
  revision: number;
}
```

## 行为

1. Repeat：读取原 request headers/body（需 body 仍在存储中），构造 Compose。
2. 若正文已丢弃 → 错误提示。
3. 发送在后台任务执行，立即返回新 `transactionId`。
4. 不自动跟随重定向（可配置 followRedirects P2）。

## 控制 API

```http
POST /api/v1/compose
POST /api/v1/transactions/{id}/repeat
```

Body 分别为 `ComposeRequest` / `RepeatRequest`。

## UI 要点

- 右键：重复、编辑并重复。
- Compose 窗口：方法、URL、头表、body 文本。
- 发送中按钮 loading。

## UI 操作指南

### 界面位置

| 重复 | 右键/检查器工具条，即时无对话框 |
| 编辑并重复 | L3/L4 **编辑请求** 对话框 |

### 如何打开

选中 HTTP 事务 → 右键 **编辑并重复…**

### 操作步骤

1. 重复：一点即发，L2 出现新行。
2. 编辑并重复：对话框改 Method/URL/头/体 → **发送**（关闭对话框）→ 新事务。

### 预期行为

编辑 UI 是对话框不是新浏览器页；发送后焦点回 L2 新事务。


## 验收标准

- [ ] Repeat 产生新事务且请求关键字段一致。
- [ ] Compose 改 body 后上游收到新体。
- [ ] 原文被清空后 Repeat 失败信息明确。

## 交叉链接

- [05](05-transactionModel.md) · [28](28-advancedRepeatLoadTest.md) · [19](19-breakpoints.md)
- [07](07-requestResponseViewers.md)
