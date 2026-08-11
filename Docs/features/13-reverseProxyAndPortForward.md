# 13 反向代理与端口转发

## Charles 对照

- **Reverse Proxies**：本地监听端口，将 HTTP 请求转发到远程主机，便于把公网/远程服务「映射」到本地端口调试。
- **Port Forwarding**：字节级 TCP 端口转发（非 HTTP 感知）。

## 目标

- P2 提供可配置的反向 HTTP 代理条目与 TCP 端口转发条目。
- 反向代理路径上仍可录制 HTTP Transaction 并走工具子集（Rewrite/Map 等）。
- 端口转发以连接为单位记录元数据（可选轻量 Transaction `protocol=tunnel`）。

## 非目标

- 不做完整负载均衡器（权重、健康检查集群）。
- 不做 UDP 端口转发（可后续）。
- 不暴露非本机可控的公网入口默认配置（默认 listen 127.0.0.1）。

## 领域模型

```typescript
interface ReverseProxyEntry {
  id: string;
  enabled: boolean;
  listenHost: string;
  listenPort: number;
  /** 上游基础：https://api.example.com:443 */
  remoteHost: string;
  remotePort: number;
  remoteScheme: "http" | "https";
  /** 是否保留原始 Host 头 */
  preserveHostHeader: boolean;
  /** 可选路径前缀剥离 */
  stripPathPrefix: string;
}

interface PortForwardEntry {
  id: string;
  enabled: boolean;
  listenHost: string;
  listenPort: number;
  targetHost: string;
  targetPort: number;
}

interface ReverseAndForwardConfiguration {
  reverseProxies: ReverseProxyEntry[];
  portForwards: PortForwardEntry[];
}
```

## 行为

### 反向代理

1. 接受本地 HTTP 请求，改写目标为 remote，再出站。
2. 创建 Transaction，`clientAddress` 为调用方。
3. HTTPS 上游使用系统信任或可选自定义；与 MITM 客户端侧无关。
4. 端口冲突检测：与 SOCKS/HTTP 代理/其他条目互斥。

### 端口转发

1. accept TCP 后 dial target，双向 copy。
2. 不解析应用层；限速工具可作用在字节流。

## 控制 API

```http
GET  /api/v1/listeners/reverseProxies
PUT  /api/v1/listeners/reverseProxies
GET  /api/v1/listeners/portForwards
PUT  /api/v1/listeners/portForwards
```

监听变更建议在服务 running 时支持热更新条目（新增 listen / 关闭旧 listen）；若实现复杂可要求 stop 后改。

## UI 要点

- 设置 → 反向代理 / 端口转发 表格：启用、本地、远程、操作。
- 冲突端口红色校验。

## UI 操作指南

### 界面位置

| 端口转发 | L3：**代理 → 端口转发…** |
| 反向代理 | L3：**代理 → 反向代理…** |

### 如何打开

代理菜单对应项（对话框，非设置全页）。

### 操作步骤

1. 打开对话框 → 添加…（L4）填监听与目标 → 确定回规则表。
2. 勾选启用规则 → 应用/确定。
3. 启动服务后验证；L2 可见对应事务。

### 预期行为

规则表型标准工具对话框布局（见 35§5.1）。


## 验收标准

- [ ] 本地 `listenPort` 访问等价于直连 remote 的 HTTP 响应。
- [ ] 结构中可见反向代理产生的事务。
- [ ] 端口转发可连通 SSH/自定义 TCP。
- [ ] 禁用条目后端口释放。

## 交叉链接

- [09](09-httpHttpsProxy.md) · [02](02-platformArchitecture.md) · [14](14-accessControlAndUpstream.md)
