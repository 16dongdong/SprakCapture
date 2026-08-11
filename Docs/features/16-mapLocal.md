# 16 Map Local

## Charles 对照

Map Local 将匹配请求映射到本地文件或目录，不访问远程，返回文件内容作为响应。

## 目标

- 规则：Location + 本地路径（文件或目录）。
- 目录映射时按 URL path 相对拼接；支持默认 `index.html`。
- 命中后短路出站，标志 `mappedLocal`，仍录制完整事务。
- 推断 Content-Type；可覆盖状态码。

## 非目标

- 不做本地 SPA 路由回退的复杂服务器逻辑（可选简单 fallback 文件 P2）。
- 不执行本地 CGI。

## 领域模型

```typescript
interface MapLocalRule {
  id: string;
  enabled: boolean;
  location: Location;
  localPath: string; // 绝对路径或相对用户映射根
  /** 目录映射 */
  isDirectory: boolean;
  statusCode: number; // 默认 200
  /** 额外响应头 */
  responseHeaders: Array<{ name: string; value: string }>;
  contentTypeOverride: string; // 空则嗅探
}

interface MapLocalConfiguration {
  enabled: boolean;
  rules: MapLocalRule[]; // 先匹配先生效
}
```

## 行为

1. 流水线在 Map Remote 之后、Rewrite 之前执行（见 [02](02-platformArchitecture.md)）。
2. 文件不存在 → 404 合成响应，事务 complete/failed 可配置（默认 complete + 404）。
3. 路径穿越：规范化后必须仍在目录根内，否则 403。
4. 大文件流式读入 BodyStore，受 maxBodyBytes 限制。

## 控制 API

```http
GET /api/v1/tools/mapLocal
PUT /api/v1/tools/mapLocal
```

## UI 要点

- 以 **工具 → Map Local…** 模态对话框为主（启用勾选 + 规则表 + 添加/编辑子对话框）。
- 标准按钮：应用 / 确定 / 取消；右键可预填 Location。

## UI 操作指南

### 界面位置

| 主入口 | 菜单 **工具 → Map Local…** → L3 对话框 |
| 快捷 | L2 结构报文右键 **Map Local…** → 同 L3，并预填 L4 |
| 痕迹 | L2 检查器概览/工具页签 |

### 如何打开

`工具 → Map Local…` 弹出标题为「Map Local」的模态对话框（不是打开工具对话框）。

### 操作步骤

1. 对话框顶部勾选 **启用 Map Local**。
2. 点 **添加…** → L4 规则编辑：Location + 本地路径（浏览… 文件框）+ 目录选项。
3. L4 **确定** 回到 L3 规则表。
4. L3 **应用** 或 **确定** 写入后端。
5. 浏览器刷新；L2 事务标记 mappedLocal。

### 预期行为

| 场景 | 预期 |
|---|---|
| 应用 | 对话框可保持打开，继续加规则 |
| 确定 | 保存并关闭，回到 L2 工作台 |
| 取消 | 未应用草稿丢弃 |
| 右键预填 | L4 主机/路径已填，只需选文件 |


## 验收标准

- [ ] 映射 JSON 文件后响应体与文件一致且未出站。
- [ ] 目录映射 `/app/main.js` → `{dir}/app/main.js`。
- [ ] `../` 穿越被拒绝。
- [ ] 标志在结构报文与检查器中可见。

## 交叉链接

- [17](17-mapRemote.md) · [03](03-locationMatching.md) · [02](02-platformArchitecture.md)
- [05](05-transactionModel.md)
