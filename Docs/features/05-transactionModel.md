# 05 事务模型（Transaction）

## Charles 对照

Charles 将一次 HTTP 请求-响应对显示为一行 Session；CONNECT 隧道、WebSocket 等也有对应行。Notes、Timing、Sizes、Status 构成 Overview。

## 目标

- 定义 Sprak Capture 统一 `Transaction` 元模型，覆盖 HTTP(S)、隧道、可选 WebSocket 升级。
- 列表快照仅元数据；正文与解码视图按需加载。
- 支持工具标记（mapped、rewritten、blocked、breakpoint、throttled 等）。
- 时间线字段足够驱动瀑布图与时长列。

## 非目标

- 不把每个 UDP 数据包建模为 Transaction；一个 SOCKS UDP ASSOCIATE 生命周期只生成一条事务摘要。
- 不做分布式 trace id 关联。

## 领域模型

```typescript
type TransactionProtocol =
  | "http"
  | "https"
  | "ws"
  | "wss"
  | "tunnel"   // CONNECT 未解密或透传
  | "socks";   // SOCKS CONNECT、BIND 或 UDP ASSOCIATE 会话摘要

type TransactionStatus =
  | "pending"      // 已见请求，等待响应
  | "complete"
  | "failed"
  | "blocked"
  | "cancelled";

interface TransactionTimings {
  startAtMilliseconds: number;
  dnsEndAtMilliseconds?: number;
  connectEndAtMilliseconds?: number;
  tlsEndAtMilliseconds?: number;
  requestSentAtMilliseconds?: number;
  responseStartAtMilliseconds?: number;
  endAtMilliseconds?: number;
}

interface TransactionSizes {
  requestHeaderBytes: number;
  requestBodyBytes: number;
  responseHeaderBytes: number;
  responseBodyBytes: number;
}

interface TransactionFlags {
  mappedLocal: boolean;
  mappedRemote: boolean;
  rewritten: boolean;
  breakpointHit: boolean;
  throttled: boolean;
  mitmDecrypted: boolean;
  bodyTruncated: boolean;
  headersTruncated: boolean;
  fromCache: boolean;
}

interface TransactionSummary {
  transactionId: string;
  recordingSessionId: string;
  sequence: number;
  protocol: TransactionProtocol;
  method: string;          // CONNECT/GET/...
  host: string;
  port: number;
  path: string;
  query: string;
  urlDisplay: string;      // 完整展示
  status: TransactionStatus;
  statusCode: number | null;
  clientAddress: string;
  clientProcessName?: string;
  clientProcessId?: number;
  contentType: string;
  timings: TransactionTimings;
  sizes: TransactionSizes;
  flags: TransactionFlags;
  error: {
    code: string;
    messageKey: string;
    params: Record<string, string>;
  } | null;
  notes: string;
  tags: string[];          // 工具可打标
  appliedTools: string[];
}

interface TransactionPage {
  revision: number;
  recordingSessionId: string;
  /** 第一页生成；后续页原样回传，集合变化时服务端返回 409 */
  collectionToken: string;
  total: number;
  offset: number;
  limit: number;
  hasPrevious: boolean;
  hasMore: boolean;
  /** 下一页的真实起点；4 MiB 预算缩短当前页时不等于 offset + limit */
  nextOffset: number | null;
  truncated: boolean;
  itemsTruncated: boolean;
  items: TransactionSummary[];
}

/** 列表事件/快照使用 TransactionSummary[]，无 body */
interface TransactionsEvent {
  type: "transactions";
  revision: number;
  /** 有界权威页；后续分页必须携带 collectionToken */
  transactions: TransactionPage;
}

interface BodyHandleMeta {
  transactionId: string;
  side: "request" | "response";
  contentType: string;
  encoding: string;        // identity | gzip | br | ...
  storedBytes: number;
  originalBytes: number;
  truncated: boolean;
  sha256?: string;
}
```

### 当前事务读取 API

```http
GET /api/v1/transactions?offset&limit&collectionToken
GET /api/v1/transactions/{transactionId}
GET /api/v1/transactions/{transactionId}/request/body
GET /api/v1/transactions/{transactionId}/response/body
```

完整顺序遍历必须从 `offset=0` 开始，并在 `hasMore=true` 时使用响应的
`nextOffset` 作为下一次 `offset`；禁止按请求 `limit` 推算步长。省略 `offset`
得到的是最新视图，适合界面首屏，不作为完整顺序遍历的起点。

事务详情一次返回两侧有序头与正文元信息；实际正文按侧独立读取，Web 进入正文视图后
自动发起请求。`notes`、`tags` 等用户
字段的写入 API 属于后续修改工具阶段，不在 M1c 建立空路由。

正文响应推荐 JSON base64，便于 Web：

```typescript
interface BodyResponse {
  meta: BodyHandleMeta;
  /** 标准 base64 */
  base64: string;
}
```

### 存储

- 元数据：固定预算的内存环形缓冲；事务摘要与头都受会话预算约束。
- 正文：内存阈值 + 磁盘 spill（用户临时目录）；清除录制时删除 spill 文件。

## 行为

1. HTTP 请求头收齐后创建 `pending` 事务并进入列表。
2. 响应完成或失败后更新 `status` / `statusCode` / timings / sizes。
3. Map Local 合成响应：`flags.mappedLocal=true`，仍完整展示。
4. Block：`status=blocked`，可无响应体或短 HTML/空。
5. 未解密 CONNECT：`protocol=tunnel`，method=`CONNECT`，无应用层 path。
6. MITM 解密成功：同一逻辑连接上的 HTTP 事务 `protocol=https`，`flags.mitmDecrypted=true`。
7. SOCKS 命令和真实目标解析后创建 `protocol=socks` 事务；上下行字节取会话绝对计数。
8. UDP ASSOCIATE 使用首个成功转发数据报的远端作为 Location，并在控制连接关闭时完成事务。
9. SOCKS 成功转发载荷自动保存双向完整原始流和分片索引；不按流长度、活动会话总量或片段数截断，
   `sizes` 与 `BodyHandleMeta` 均反映真实入库长度，正常录制下 `truncated=false`
   的差异。终态镜像保留到录制器确认接管，以支持广播丢帧恢复；确认后会话历史不再持有原始镜像。
10. 清空录制会推进捕获代际；清空前已经排队的旧代际会话事件不得重新创建事务。

## UI 要点

- 结构树：Host:Port → 报文；报文行显示资源类型图标、方法、真实位置与大小。
- SOCKS 没有应用层 Path，直接显示 `socks5://host:port`，不生成“根路径”节点。
- 检查器：Overview 用 Summary；进入 Contents 后自动加载 body。

## UI 操作指南

事务只在 **L2 连接会话** 展示，无独立配置对话框。

### 界面位置

结构树 / 检查器（均为 L2）。

### 如何打开

主导航 **连接会话** → 单击事务行或树节点。

### 操作步骤

1. 在结构中选中报文。
2. 检查器查看元数据与正文（自动加载）。
3. 需要针对该 URL 配工具时：右键 **Map Local…** 等 → 进入 L3/L4，不在检查器内嵌规则表。

### 预期行为

选中变化只影响 L2 检查器；打开 L3 时选中冻结在打开前那条（除非对话框关闭后用户再点）。


## 验收标准

- [ ] 列表 JSON 无 body 字段。
- [ ] 完成的 GET 事务 timings/sizes 合理非负。
- [ ] 截断标记与 storedBytes 一致。
- [ ] 清空录制后详情 API 返回 404。
- [ ] 50ms 合并窗口下 UI 不丢最终状态。
- [ ] SOCKS CONNECT、失败连接和 UDP ASSOCIATE 都进入事务列表，且一个 UDP 关联不按包放大事务数。
- [ ] 新增或发生字节变化的报文亮起并自动消散，静止事务不重复闪烁。

## 交叉链接

- [04](04-sessionAndRecording.md) · [06](06-structureSequenceFocus.md) · [07](07-requestResponseViewers.md)
- [08](08-chartsAndOverview.md) · [30](30-importExport.md) · [02](02-platformArchitecture.md)
