# 22 阻止 Cookie（Block Cookies）

## Charles 对照

Block Cookies 移除请求中的 `Cookie` 与响应中的 `Set-Cookie`，便于测试未登录态或隐私场景。

## 目标

- 按 Location 启用。
- 请求剥离 `Cookie`；响应剥离所有 `Set-Cookie`（含多份）。
- 可选剥离 `Cookie2` / 非标准变体。

## 非目标

- 不做 Cookie 罐存储与重放。
- 不做按 cookie 名细粒度过滤（P2 可加）。

## 领域模型

```typescript
interface BlockCookiesConfiguration {
  enabled: boolean;
  locations: Location[];
  stripRequestCookie: boolean;
  stripResponseSetCookie: boolean;
}
```

## 行为

1. 请求阶段：删除头名等于 `Cookie`（大小写不敏感）。
2. 响应阶段：删除所有 `Set-Cookie`。
3. 与 No Caching 独立，可同时启用。
4. 流水线位置见 [02](02-platformArchitecture.md)。

## 控制 API

```http
GET /api/v1/tools/blockCookies
PUT /api/v1/tools/blockCookies
```

## UI 要点

- 开关 + Location 表 + 两个方向复选。

## UI 操作指南

### 界面位置

**工具 → 阻止 Cookie…** 小对话框。

### 如何打开

菜单或右键。

### 操作步骤

启用 → 范围 → 确定；L2 检查器无 Cookie/Set-Cookie。

### 预期行为

同无缓存，小对话框层级。


## 验收标准

- [ ] 带 Cookie 的请求到达上游时无 Cookie 头。
- [ ] 上游 Set-Cookie 不返回客户端。
- [ ] 未匹配 Location 保留 Cookie。

## 交叉链接

- [21](21-noCaching.md) · [20](20-blockList.md) · [03](03-locationMatching.md)
