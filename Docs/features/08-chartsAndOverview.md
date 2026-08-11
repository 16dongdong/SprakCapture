# 08 图表与概览

## Charles 对照

- **Overview**：方法、URL、状态、大小、时长、客户端、SSL、备注等只读字段。
- **Chart / Timeline**：瀑布条显示 DNS/Connect/TLS/Request/Response 阶段；汇总图显示体积与耗时分布。

## 目标

- 检查器 Overview 完整展示 `TransactionSummary` + timings 分解。
- P1 提供单事务时间瀑布与当前过滤集合的简易汇总（耗时、内容类型）。
- 指标与控制面 `metrics` 扩展对齐（HTTP 事务计数）。

## 非目标

- 不做实时 APM 大盘与长期时序数据库。
- 第一版不做可导出 PDF 报告。

## 领域模型

```typescript
interface OverviewFields {
  urlDisplay: string;
  method: string;
  statusCode: number | null;
  protocol: TransactionProtocol;
  clientAddress: string;
  clientProcessName?: string;
  remoteAddress?: string;
  contentType: string;
  sizes: TransactionSizes;
  timings: TransactionTimings;
  durationMilliseconds: number | null;
  flags: TransactionFlags;
  notes: string;
  errorMessage: string;
}

interface TimingBarSegment {
  key: "dns" | "connect" | "tls" | "request" | "wait" | "download";
  startOffsetMs: number;
  durationMs: number;
}

interface AggregateChartModel {
  count: number;
  totalBytes: number;
  averageDurationMs: number;
  byStatusClass: { "2xx": number; "3xx": number; "4xx": number; "5xx": number; other: number };
  byContentTypeTop: Array<{ type: string; count: number; bytes: number }>;
}
```

### duration 计算

```text
duration = endAt - startAt
wait = responseStart - requestSent
download = endAt - responseStart
```

缺失阶段不画段，总条仍以 start/end 为准。

## 行为

1. Overview 数据全部来自摘要，无需 body。
2. 瀑布在有 timings 细节时渲染；隧道事务可只有 connect 段。
3. 汇总图在结构筛选变化时重算（前端 reduce）。
4. 服务 metrics 扩展：

```typescript
interface ServiceMetrics {
  // 现有 SOCKS 字段...
  httpTransactionsTotal: number;
  httpTransactionsActive: number;
  httpBytesUp: number;
  httpBytesDown: number;
  mitmHandshakesTotal: number;
  mitmHandshakesFailed: number;
}
```

## 控制 API

无独立图表 API。数据来自事务摘要与 metrics。

## UI 要点

- Overview 使用定义列表，密集但可读；长 URL 可换行。
- 瀑布彩色段 + 图例。
- 汇总图放在检查器「图表」页签或 Overview 底部。
- 空状态：无事务时显示引导文案。

## UI 操作指南

### 界面位置

- 单事务图表：L2 检查器 → **图表**
- 全局指标：L2 **概览** 页（主导航）

### 如何打开

选中事务后开图表页签；或顶栏/导航进概览。无专用设置对话框（P1 图表选项若有，放视图菜单小对话框）。

### 操作步骤

1. 查看瀑布分段；悬停看耗时。
2. 概览页只读卡片，点「监听摘要」可 **代理 → 代理设置…** 深链打开 L3。

### 预期行为

图表留在 L2；不把概览做成设置页。


## 验收标准

- [ ] 完整事务 Overview 字段无空关键项（URL/方法/状态/耗时）。
- [ ] 瀑布各段非负且不重叠乱序。
- [ ] 筛选后汇总 count 与列表条数一致。
- [ ] notes 编辑后 revision 更新并持久于会话内。

## 交叉链接

- [05](05-transactionModel.md) · [06](06-structureSequenceFocus.md) · [07](07-requestResponseViewers.md)
- [26](26-clientProcess.md)
