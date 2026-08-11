# 04 会话与录制

## Charles 对照

Charles 的 Recording 控制是否把新事务记入当前会话文件；可暂停录制、清空、限制大小、忽略特定 Location。保存的是「会话文件」，与 TCP 连接生命周期无关。

## 目标

- 引入 `RecordingSession`：抓包容器，可启停、清空、忽略。
- 与 SOCKS `Session`（传输连接）严格区分命名与 API。
- 录制关闭时仍可转发流量，只是不入事务列表。
- 完整持久化所有已接纳事务与正文，直到用户显式清空。

## 非目标

- 第一版不做多录制会话并行标签页（仅一个 active RecordingSession；历史文件通过导入打开为只读视图可 P2）。
- 不在录制层做内容搜索引擎。

## 领域模型

```typescript
type RecordingState = "recording" | "paused";

interface RecordingLimits {
  /** 只读兼容字段，固定为 JavaScript 安全整数上限 */
  maxTransactions: number;
  /** 只读兼容字段，不参与正文写入 */
  maxBodyBytes: number;
  /** 只读兼容字段，不参与事务淘汰 */
  maxTotalBodyBytes: number;
}

interface RecordingSnapshot {
  recordingSessionId: string;
  state: RecordingState;
  startedAtMilliseconds: number;
  transactionCount: number;
  droppedCount: number;
  totalBodyBytes: number;
  /** 事务摘要、索引、正文引用与两侧持久头的当前保守逻辑计费 */
  totalMetadataBytes: number;
  /** 固定会话元数据内存预算 */
  metadataMemoryBudgetBytes: number;
  limits: RecordingLimits;
  ignoreLocations: Location[];
  /** 是否录制未解密的 CONNECT 隧道元数据 */
  recordTunnelMetadata: boolean;
}

interface RecordingUpdate {
  state?: RecordingState;
  ignoreLocations?: Location[];
  recordTunnelMetadata?: boolean;
}
```

### 与 SOCKS Session 对照

| | RecordingSession | SOCKS Session |
|---|---|---|
| 含义 | 抓包容器 | 一条控制连接/中继 |
| ID | `recordingSessionId` | `sessionId` |
| 生命周期 | 用户清空/新会话 | 连接关闭 |
| 快照字段 | `recording` | `sessions[]` |

## 行为

1. 服务启动时自动创建 active `RecordingSession`，默认 `recording`。
2. `paused`：新流量不入事务列表；已有事务可继续补全进行中的响应。
3. 忽略列表命中：不创建事务。
4. `DELETE`/clear 清空：删除所有事务元数据与正文；不重置服务累计 metrics。
5. 所有已接纳事务与正文完整保留；正文超过内存阈值后写入会话 spill 文件。
6. 只有用户显式 clear 才删除事务；磁盘或内存错误显式失败，不生成截断正文或静默淘汰。
7. clear 的正文引用在物理删除成功前仍计入 `totalMetadataBytes`；删除失败或任务取消时
   保留可重试 tombstone 和准确账本。

## 控制 API

| 方法 | 路径 | 作用 |
|---|---|---|
| `GET` | `/api/v1/recording` | 录制快照 |
| `PUT` | `/api/v1/recording` | 更新状态/忽略列表 |
| `POST` | `/api/v1/recording/clear` | 清空事务 |
| `POST` | `/api/v1/recording/export` | 触发导出（见 [30](30-importExport.md)） |

写操作成功后：`revision++`，事件 `recording` + 可能的 `transactions` 清空通知。

权威总快照中嵌入 `recording: RecordingSnapshot`。

## UI 要点

- 工具栏红色录制按钮：recording / paused 切换。
- 清空需确认。
- 设置中编辑 limits 与 ignore Locations。
- 状态栏显示事务数 / dropped。

## UI 操作指南

### 界面位置

| 元素 | 层级与位置 |
|---|---|
| 录制开关 | L1 顶栏红点；菜单 代理 → 开始/暂停录制 |
| 录制设置 | L3：**代理 → 录制设置…** |
| 清空 | L1 顶栏；**文件 → 清空会话** → L4 确认 |
| 状态 | L1 底栏 |

### 如何打开

- 开关：顶栏录制（不弹窗）。
- 限额/忽略：**代理 → 录制设置…** 打开 L3 对话框。

### 操作步骤

1. 顶栏打开录制，在 L2 连接会话观察事务。
2. 需改限额：菜单打开 **录制设置** 对话框 → 改 max 与忽略列表 → 应用/确定。
3. 结构树右键 **忽略主机**：即时写入忽略；可再到录制设置中查看完整列表。
4. 清空：顶栏清空 → L4 确认 → L2 列表变空。

### 预期行为

| 场景 | 预期 |
|---|---|
| 仅开关录制 | 无对话框，底栏状态变 |
| 打开录制设置 | 遮罩 + L3，工作台仍在背景但不可操作 |
| 达上限 | Toast + 底栏；代理继续 |


## 验收标准

- [ ] 暂停后新 HTTP 请求不出现在事务结构。
- [ ] 忽略 `*.doubleclick.net` 后相关事务不出现。
- [ ] 清空后列表为空且正文 API 404。
- [ ] 超限丢弃最旧，计数正确。
- [ ] 与 SOCKS `DELETE /sessions` 互不影响。

## 交叉链接

- [05](05-transactionModel.md) · [03](03-locationMatching.md) · [06](06-structureSequenceFocus.md)
- [25](25-autoSave.md) · [30](30-importExport.md) · [02](02-platformArchitecture.md)
