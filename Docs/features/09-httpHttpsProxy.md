# 09 HTTP/HTTPS 正向代理

## Charles 对照

Charles 作为 HTTP 代理：解析绝对 URI 请求、处理 `CONNECT` 建立隧道，并可在 SSL Proxying 开启时在隧道内 MITM。支持 HTTP 代理认证（可选）。

## 目标

- 实现标准 HTTP 正向代理（RFC 7230 网关形态）：
  - 明文：`GET http://host/path HTTP/1.1`
  - 隧道：`CONNECT host:port HTTP/1.1`
- 与工具流水线、capture-core 集成。
- HTTP 与 SOCKS5 通过首字节识别共用同一个监听 host/port，避免重复端口配置。
- HTTPS 应用层解析在 MITM 成功后由 [10](10-sslMitm.md) 接管。

## 非目标

- Windows 透明入口由 [15](15-processCaptureAndProxyRouting.md) 的 WinDivert 进程捕获负责，HTTP 层不直接修改数据包。
- 不做完整缓存代理（304 智能缓存）；No Caching 工具另述。
- 不做 HTTP/3 监听。

## 领域模型

```typescript
interface HttpProxyConfiguration {
  enabled: boolean;
  listenHost: string;
  listenPort: number; // 始终与 SOCKS5 融合监听端口一致
  /** 代理协议认证：none | basic */
  authenticationMode: "none" | "basic";
  /** 仅更新时写入；快照只回用户名列表 */
  authenticationUsernames: string[];
  maxHeaderBytes: number;
  maxBodyBytes: number;
  supportProxyConnectionHeader: boolean;
  allowAbsoluteUriHttps: boolean;
}

interface HttpProxyPublicState {
  enabled: boolean;
  boundEndpoint: string | null;
  authenticationMode: "none" | "basic";
  authenticationUsernames: string[];
}
```

配置并入扩展后的 `PublicConfiguration.httpProxy`；修改监听配置时控制面会强制断开数据面连接并重启，工具级开关仍可运行时更新。

## 行为

### 明文 HTTP

1. 解析请求行绝对 URI 或 `Host` + path。
2. 构造 `ResolvedLocation`，创建 Transaction。
3. 走请求流水线；未短路则出站 TCP 连接目标并转发。
4. 普通响应在正文工具完成后返回；`text/event-stream` 跳过需要完整响应正文的工具并逐帧转发，同时持续镜像到 BodyStore。
5. keep-alive：第一版可对客户端 keep-alive、对上游短连接，后续优化连接池。

### CONNECT

1. 验证 `host:port`，返回 `200 Connection Established`。
2. 若 SSL 主机表匹配 → MITM（[10](10-sslMitm.md)）。
3. 否则字节透传隧道；可选 `protocol=tunnel` 元数据事务。

### 错误

- 目标不可达：事务 `failed`，客户端 502/504 风格响应。
- 头过大：400 并记录。

## 控制 API

```http
PUT /api/v1/configuration
```

扩展 body 含 `httpProxy` 字段；或：

```http
PUT /api/v1/listeners/httpProxy
GET /api/v1/listeners/httpProxy
```

服务 `start` 时只绑定一个融合监听器；首字节 `0x05` 进入 SOCKS5，其余连接进入 HTTP，两个协议共享连接上限、生命周期和绑定错误。

## UI 要点

- 设置 → 监听只展示一个融合地址与端口。
- 概览页的 HTTP 与 SOCKS5 状态指向同一 `boundEndpoint`。

## UI 操作指南

### 界面位置

| 配置 | L3：**代理 → 代理设置…**（页签 HTTP 代理） |
| 启停 | L1 顶栏服务状态动作 |
| 状态 | L1 底栏；L2 概览卡片 |

### 如何打开

菜单 **代理 → 代理设置…**（不要使用已废弃的「设置全页」作为主路径）。

### 操作步骤

1. 若需改端口：先顶栏 **停止** 服务。
2. 打开 **代理设置** 对话框 → HTTP 代理页签 → 启用并填 host/port。
3. **应用/确定**。
4. 顶栏 **启动**。
5. 浏览器走该代理；流量出现在 L2 连接会话。

### 预期行为

| 场景 | 预期 |
|---|---|
| 对话框打开 | 居中遮罩，标题「代理设置」 |
| 端口冲突 | 对话框内或 Toast 中文错误，不关闭服务其它部分误报 |
| 运行中改端口 | 字段禁用 + 提示先停止 |


## 验收标准

- [ ] curl `-x http://127.0.0.1:port http://example.com` 返回 200 级响应且结构中可见事务。
- [ ] CONNECT 隧道可访问 HTTPS（未 MITM 时加密流量）。
- [ ] 与 SOCKS 同时运行无端口冲突配置校验。
- [ ] 头解析 fuzz 不崩溃（恶意输入关闭连接）。

## 交叉链接

- [10](10-sslMitm.md) · [11](11-socksProxy.md) · [14](14-accessControlAndUpstream.md)
