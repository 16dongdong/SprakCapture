# 11 SOCKS 代理增强

## Charles 对照

Charles 支持 SOCKS 代理作为额外入口，流量同样可进入会话与部分工具。Sprak Capture 已具备完整 SOCKS5（CONNECT/BIND/UDP），本设计定义与抓包工作台的对齐方式。

## 目标

- 保留并稳定现有 `socks5-core` 行为与测试。
- 将 SOCKS 配置纳入统一多监听配置模型。
- CONNECT、BIND 与 UDP ASSOCIATE 会话摘要原生投影到统一 Transaction 工作台。
- UDP ASSOCIATE 继续统计包级 metrics，但一个关联只生成一条事务，不为每个数据报建行。

## 非目标

- 不在本阶段重写协议库。
- 不做 SOCKS4。
- 不在 SOCKS 层做 TLS MITM（TLS 在 HTTP CONNECT 路径做）。

## 领域模型

```typescript
interface SocksProxyConfiguration {
  enabled: boolean;
  listenHost: string;
  listenPort: number;
  authenticationMode: "none" | "password";
  authenticationUsernames: string[];
  maxConnections: number;
  connectTimeout: number;
  bindTimeout: number;
  idleTimeout: number;
  shutdownTimeout: number;
  readTimeout: number;
  relayBufferSize: number;
  udpBindHost: string;
  udpMaxPacketSize: number;
}

/** 与现网 SessionSnapshot 兼容 */
interface SessionSnapshot {
  sessionId: string;
  clientAddress: string;
  username: string;
  command: "connect" | "bind" | "udpAssociate" | "";
  targetAddress: string;
  state: SessionState;
  bytesUp: number;
  bytesDown: number;
  createdAtMilliseconds: number;
  updatedAtMilliseconds: number;
  closedAtMilliseconds: number;
  errorMessage: string;
}
```

现有字段保持不变；新增仅扩展，不删字段。

## 行为

1. `service/start` 启动 `enabled` 的 SOCKS 监听。
2. 会话注册表、背压、超时、有序关闭逻辑保持。
3. 控制契约路径保持：`DELETE /api/v1/sessions` 清已结束 SOCKS 会话。
4. 与 HTTP 代理共享 ACL/上游代理配置（[14](14-accessControlAndUpstream.md)）时，SOCKS CONNECT 出站走同一 Outbound 层。
5. 命令与目标解析完成后立即创建 `protocol=socks` 的 pending 事务，流量更新 sizes，关闭或失败时提交终态。
6. `sessions` 保留为数据面生命周期诊断源；主工作区只消费统一 Transaction，不恢复旧会话表或并行兼容入口。
7. UDP 控制请求声明的是客户端端点，首个成功数据报的远端才写入事务 Location。
8. SOCKS5 成功转发的双向载荷自动保留完整正文，并按转发片段建立完整索引；
   会话终态由统一正文存储确认接管后释放历史镜像，不按前缀、索引数或共享预算截断。
9. 原始流不伪造 HTTP URL、头或 MIME；检查器以 `application/octet-stream`、`binary`
   和 Hex 展示。TLS 解密后的应用层报文仍属于后续协议解码边界。

## 控制 API

延续：

| 方法 | 路径 |
|---|---|
| `POST` | `/api/v1/service/start` |
| `POST` | `/api/v1/service/stop` |
| `PUT` | `/api/v1/configuration` |
| `DELETE` | `/api/v1/sessions` |

`configuration` 中 SOCKS 字段属于当前唯一配置结构；实际监听状态和地址从
`listeners.socks5` 读取。

## UI 要点

- 设置中 SOCKS 区与现网一致，标注「数据面：SOCKS5」。
- 连接工作台使用单一事务模型展示 HTTP 与 SOCKS，不提供旧会话界面切换。
- 结构中 SOCKS 报文直接显示 `socks5://host:port` 和原始流图标，不显示“根路径”。
- 概览同时显示两路 boundEndpoint。

## UI 操作指南

### 界面位置

L3：**代理 → 代理设置…** → 页签 **SOCKS5**。

### 如何打开

菜单代理设置对话框内切换页签（对话框内层级，不是主导航）。

### 操作步骤

1. 停止服务 → 打开代理设置 → SOCKS5 页签 → 启用/端口/认证。
2. 应用/确定 → 启动服务。
3. 客户端连 SOCKS；L2 用过滤器看 socks 事务。

### 预期行为

HTTP 与 SOCKS 配置同属一个 L3，用页签分开，避免两个平级「设置页」打架。


## 验收标准

- [ ] 现有 `socks5-core` 与后端测试全绿。
- [ ] 启停与配置更新语义不变。
- [ ] 与 HTTP 代理双开时 SOCKS 功能不受影响。
- [ ] 快照仍含 `sessions` 数组且 camelCase。
- [ ] 真实 SOCKS CONNECT、失败连接和 UDP ASSOCIATE 会在统一事务工作台出现。
- [x] 完成的 SOCKS 双向流可从请求/响应正文端点读取完整原始流。

## 交叉链接

- [09](09-httpHttpsProxy.md) · [02](02-platformArchitecture.md) · [14](14-accessControlAndUpstream.md)
- [socks5Protocol](../socks5Protocol.md) · [controlContract](../controlContract.md)
