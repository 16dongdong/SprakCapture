# 26 客户端进程（Client Process）

## Charles 对照

在部分平台 Charles 可显示发起连接的本地进程名/PID，便于区分浏览器与 CLI。

## 目标

- Windows：对来自本机的连接尝试解析源端口 → 进程 PID → 映像名。
- 将 `clientProcessName` / `clientProcessId` 写入 Transaction 与 SOCKS Session 扩展字段。
- 解析失败时留空，不影响代理。

## 非目标

- 不做远程机器进程识别。
- 不注入到目标进程。

## 领域模型

```typescript
interface ClientProcessInfo {
  processId: number;
  processName: string;
  executablePath?: string;
}

interface ClientProcessConfiguration {
  enabled: boolean;
  /** 缓存源端口映射 TTL 毫秒 */
  cacheTtlMilliseconds: number;
  captureExecutablePath: boolean;
}
```

摘要字段见 [05](05-transactionModel.md)：

```typescript
// TransactionSummary 扩展
clientProcessName?: string;
clientProcessId?: number;
```

SOCKS `SessionSnapshot` 可增可选字段（只增不删）：

```typescript
clientProcessName?: string;
clientProcessId?: number;
```

## 行为

1. accept 后异步查询，不阻塞握手关键路径超过短超时（如 20ms 缓存未命中则先空后补丁更新）。
2. 使用 Windows `GetExtendedTcpTable` 等 API（via Rust）。
3. 权限不足时静默失败。

## 控制 API

```http
GET /api/v1/tools/clientProcess
PUT /api/v1/tools/clientProcess
```

## UI 要点

- 结构报文详情可选显示「进程」。
- Overview 显示进程名与 PID。

## UI 操作指南

### 界面位置

**工具 → 客户端进程…** 小对话框；展示在 L2 结构报文/检查器。

### 如何打开

菜单启用。

### 操作步骤

勾选启用 → 确定 → 本机请求显示进程名。

### 预期行为

小对话框；结果只读展示在工作台。


## 验收标准

- [ ] 本机 curl/浏览器流量显示合理进程名。
- [ ] 远程客户端 IP 非本机时字段为空。
- [ ] 关闭功能后不再查询系统表。

## 交叉链接

- [05](05-transactionModel.md) · [08](08-chartsAndOverview.md) · [11](11-socksProxy.md)
