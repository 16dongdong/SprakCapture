# 23 DNS 映射（DNS Spoofing）

## Charles 对照

DNS Spoofing 将指定主机名解析到自定义 IP，而不修改系统 hosts。对代理出站 dial 生效。

## 目标

- 规则：hostname 模式 → IP 地址。
- 在出站连接前应用；流水线靠前。
- 支持 `*` 通配主机。

## 非目标

- 不做完整权威 DNS 服务器。
- 不劫持系统 DNS（仅代理进程解析路径）。

## 领域模型

```typescript
interface DnsSpoofRule {
  id: string;
  enabled: boolean;
  /** 支持 *.example.com */
  hostPattern: string;
  ipAddress: string; // IPv4 或 IPv6
}

interface DnsSpoofingConfiguration {
  enabled: boolean;
  rules: DnsSpoofRule[];
}
```

## 行为

1. 解析目标 host 时先查 spoof 表，命中则直接用 IP，SNI/Host 头仍用原主机名。
2. HTTPS MITM 叶证书仍按原主机名签发。
3. 多规则先匹配先生效。
4. 非法 IP 配置拒绝保存。

## 控制 API

```http
GET /api/v1/tools/dnsSpoofing
PUT /api/v1/tools/dnsSpoofing
```

## UI 要点

- 主机模式与 IP 两列表格。
- 与系统 hosts 差异说明。

## UI 操作指南

### 界面位置

**工具 → 映射 → DNS 映射…** 有序规则表。

### 如何打开

从顶部工具菜单进入独立设置页。

### 操作步骤

1. 启用 → 添加主机通配与 IP → 应用/确定。
2. 访问域名；概览显示解析 IP。

### 预期行为

规则列表支持新增、删除、排序和逐项启停；右侧编辑主机模式与目标 IP。


## 验收标准

- [x] 映射后 HTTP、HTTPS、SOCKS5 TCP 与 SOCKS5 UDP 出站连到指定 IP。
- [x] HTTP Host 与 TLS SNI 仍为原域名。
- [x] 关闭总开关或单条规则后恢复系统 DNS 解析路径。
- [x] 非法 IP 更新被原子拒绝，旧规则继续生效。

## 交叉链接

- [02](02-platformArchitecture.md) · [09](09-httpHttpsProxy.md) · [10](10-sslMitm.md)
- [03](03-locationMatching.md)
