# 14 访问控制与二级代理

## 目标

- 允许融合监听器的所有出站 TCP 连接统一经过一个 HTTP CONNECT 或 SOCKS5 二级代理。
- HTTP 明文、HTTPS 隧道、SOCKS5 CONNECT 与 WinDivert 透明连接复用同一个 `OutboundConnector`，避免协议路径配置漂移。
- 二级代理口令只在更新请求和后端内存中出现，公开快照仅返回 `hasPassword`。

## 配置模型

```typescript
interface PublicUpstreamProxyConfiguration {
  enabled: boolean;
  protocol: "http" | "socks5";
  host: string;
  port: number;
  username: string;
  hasPassword: boolean;
}

interface UpstreamProxyUpdate {
  enabled: boolean;
  protocol: "http" | "socks5";
  host: string;
  port: number;
  username: string;
  /** null 保留现有口令，空字符串明确清除。 */
  password: string | null;
}
```

## 协议行为

### HTTP CONNECT

1. 连接二级代理并发送 `CONNECT targetHost:targetPort HTTP/1.1`。
2. 配置用户名时发送 Basic `Proxy-Authorization`；口令不进入日志或错误信息。
3. 仅接受 2xx 响应，完整消费响应头后把同一 TCP 流交给目标协议。

### SOCKS5

1. 协商无认证或 RFC 1929 用户名密码认证。
2. 以域名、IPv4 或 IPv6 地址类型发送 CONNECT，保留原始目标语义。
3. 仅成功回复进入转发；认证和连接失败均原样归类为二级代理错误。

## 稳定性边界

- 启用二级代理后不静默回退直连，避免请求绕过用户明确配置的出口。
- 连接超时沿用 HTTP 数据面的 `connectTimeoutMilliseconds`。
- DNS 映射只作用于直连路径；二级代理路径把目标主机交给二级代理解析。
- HTTP 与 SOCKS5 共用监听端口，但入站客户端认证和二级代理认证彼此独立。

## UI 与控制 API

- 设置 → 监听 → 二级代理：协议、主机、端口、用户名与新口令。
- `PUT /api/v1/configuration` 原子更新 `upstreamProxy`；运行中的服务会断开连接并按新配置重启。
- 留空口令且公开状态 `hasPassword=true` 时继续使用已保存口令。

## 验收

- HTTP 与 SOCKS5 入站请求均能通过两种二级代理到达测试上游。
- HTTP CONNECT Basic 和 SOCKS5 RFC 1929 的成功、拒绝、超时路径都有确定错误。
- 公开快照、日志、测试快照和最终报告不含口令原文。
