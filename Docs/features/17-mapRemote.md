# 17 Map Remote

## Charles 对照

Map Remote 将请求的协议/主机/端口/路径映射到另一远程位置，然后继续访问映射后的目标。

## 目标

- 规则：from Location 模式 → to 目标模板。
- 保留未映射部分（如 path 后缀）。
- 在 Map Local 之前执行，使 Remote 与 Local 可串联语义清晰（先改写目标，再可能被 Local 截获——若需 Local 优先可调整规则；**固定顺序：Map Remote → Map Local**，与 Charles 常见理解一致：Local 最终可覆盖）。

## 非目标

- 不做基于响应内容的二次跳转映射。

## 领域模型

```typescript
interface MapRemoteTarget {
  protocol: string; // 空 = 保持
  host: string;     // 空 = 保持
  port: string;     // 空 = 保持
  path: string;     // 可含通配替换；空 = 保持
}

interface MapRemoteRule {
  id: string;
  enabled: boolean;
  from: Location;
  to: MapRemoteTarget;
}

interface MapRemoteConfiguration {
  enabled: boolean;
  rules: MapRemoteRule[];
}
```

### 路径映射示例

```text
from.path = /v1/*
to.path   = /v2/*
请求 /v1/users → /v2/users
```

## 行为

1. 命中后修改 PipelineContext 中的 URL/Host/Location。
2. `flags.mappedRemote = true`。
3. Host 头与 SNI（若后续 MITM）与映射后主机一致。
4. 循环映射检测：同一规则集最多应用一次 per 请求（避免 A→B→A）。

## 控制 API

```http
GET /api/v1/tools/mapRemote
PUT /api/v1/tools/mapRemote
```

## UI 要点

- 双列 From / To 编辑器。
- 规则排序。

## UI 操作指南

### 界面位置

**工具 → Map Remote…** L3；右键 **Map Remote…** 预填。

### 如何打开

菜单或右键 →「Map Remote」对话框。

### 操作步骤

1. 勾选启用。
2. 添加…（L4）：From Location + To 主机/路径。
3. 应用/确定。
4. L2 仍显示原始 URL，工具痕迹显示映射目标。

### 预期行为

与 Map Local 相同的标准规则表对话框交互。


## 验收标准

- [ ] host 映射后实际出站 IP/域名变更。
- [ ] path 通配替换正确。
- [ ] 不命中规则时行为不变。
- [ ] 事务 URL 显示映射后地址，overview 可备注 from（tags）。

## 交叉链接

- [16](16-mapLocal.md) · [03](03-locationMatching.md) · [18](18-rewrite.md)
- [02](02-platformArchitecture.md)
