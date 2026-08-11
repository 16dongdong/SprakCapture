# 28 高级重复 / 负载（Advanced Repeat）

## Charles 对照

Advanced Repeat 支持并发、迭代次数、间隔，对同一请求做简易负载或批测。

## 目标

- 配置：并发度、总次数或持续时长、间隔、失败重试。
- 运行任务可取消；汇总成功/失败/延迟分位。
- 每个迭代仍可生成 Transaction（可开关「仅统计不录制」以防撑爆会话）。

## 非目标

- 不做完整 JMeter 替代（场景脚本、CSV 参数化可 P3）。
- 不分布式压测。

## 领域模型

```typescript
interface AdvancedRepeatPlan {
  name: string;
  base: ComposeRequest;
  concurrency: number; // 1..256
  totalIterations: number;
  intervalMilliseconds: number;
  /** 录制每次迭代 */
  recordEach: boolean;
  stopOnError: boolean;
}

type AdvancedRepeatState = "queued" | "running" | "completed" | "cancelled" | "failed";

interface AdvancedRepeatJob {
  jobId: string;
  state: AdvancedRepeatState;
  plan: AdvancedRepeatPlan;
  startedAtMilliseconds: number;
  finishedAtMilliseconds?: number;
  completedIterations: number;
  successCount: number;
  failureCount: number;
  latencyMilliseconds: {
    min: number;
    max: number;
    p50: number;
    p95: number;
    p99: number;
  };
  lastError: string | null;
}
```

## 行为

1. 任务由 backend 执行器调度，受全局 `maxConnections` 与独立 `maxLoadTestConnections` 限制。
2. 取消后不再启动新迭代，等待进行中结束（有超时）。
3. 统计无事务 body 的内存泄漏：recordEach=false 时不入 Recording 列表。

## 控制 API

```http
POST   /api/v1/loadTests
GET    /api/v1/loadTests/{jobId}
POST   /api/v1/loadTests/{jobId}/cancel
GET    /api/v1/loadTests
```

## UI 要点

- 从 Compose/事务打开「高级重复」对话框。
- 进度条与延迟摘要。
- 运行列表可取消。

## UI 操作指南

### 界面位置

右键 **高级重复…** → L3 参数对话框。

### 如何打开

选中事务或树节点 → 高级重复…。

### 操作步骤

1. 对话框填迭代、并发、是否新会话。
2. 生产主机警告 → L4 确认。
3. **开始** 后对话框可显示进度或收为可取消进度条。
4. 结束后 L2 切到结果（或新会话视图）。

### 预期行为

压测参数不进主设置页。


## 验收标准

- [ ] concurrency=5、total=20 时成功计数合理。
- [ ] cancel 后状态为 cancelled。
- [ ] recordEach=false 不膨胀事务列表。
- [ ] 超出并发上限被拒绝或排队策略明确。

## 交叉链接

- [27](27-repeatAndEdit.md) · [05](05-transactionModel.md) · [04](04-sessionAndRecording.md)
