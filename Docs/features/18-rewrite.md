# 18 Rewrite

## Charles 对照

Rewrite 按规则修改 URL、头、体、状态码、查询参数等；分请求/响应；支持正则与匹配/替换。

## 目标

- 规则集：名称、启用、Location 范围、多条 RewriteRule。
- 支持类型：URL host/path/query、header add/set/remove、body regex、response status。
- 请求阶段与响应阶段分离执行。

## 非目标

- 不做完整脚本引擎（JS rewrite P3 可选）。
- 不在二进制体上默认正则（仅文本 Content-Type 或显式开启）。

## 领域模型

```typescript
type RewriteRuleType =
  | "urlHost"
  | "urlPath"
  | "urlQuery"
  | "requestHeader"
  | "responseHeader"
  | "requestBody"
  | "responseBody"
  | "responseStatus";

interface RewriteRule {
  id: string;
  enabled: boolean;
  type: RewriteRuleType;
  matchRegex: string;
  replace: string;
  /** header 名（header 类规则） */
  headerName?: string;
  matchValueRegex?: string;
  /** 对 header：add | modify | remove */
  headerAction?: "add" | "modify" | "remove";
  caseSensitive: boolean;
  matchAllOccurrences: boolean;
}

interface RewriteSet {
  id: string;
  name: string;
  enabled: boolean;
  locations: Location[]; // 空 = 全局
  rules: RewriteRule[];
}

interface RewriteConfiguration {
  enabled: boolean;
  sets: RewriteSet[];
}
```

## 行为

1. `rewriteRequest`：仅应用请求类规则。
2. `rewriteResponse`：响应类规则。
3. 正则编译失败 → 配置保存时拒绝。
4. body 替换后更新 Content-Length；若存在 Content-Encoding 已解码再写回 identity（与 Charles 类似简化）。
5. `flags.rewritten = true` 任一规则生效时。

## 控制 API

```http
GET /api/v1/tools/rewrite
PUT /api/v1/tools/rewrite
POST /api/v1/tools/rewrite/validate  // 校验正则
```

## UI 要点

- 规则集列表 + 内部规则表。
- 类型切换显示不同字段。
- 正则测试小工具（样例输入）。

## UI 操作指南

### 界面位置

**工具 → Rewrite…** L3；集合与规则在对话框内编辑；单条规则 L4。

### 如何打开

菜单 **工具 → Rewrite…**

### 操作步骤

1. 启用 Rewrite。
2. 添加重写集（可在 L3 内列表选中集）。
3. 为集添加规则：L4 选类型（Header/Body…）、方向、匹配与替换。
4. 应用/确定。
5. （可选）试运行区在 L3 底部展开，不另开浏览器页。

### 预期行为

复杂规则仍困在对话框内滚动，不把主窗口切成编辑器页。


## 验收标准

- [ ] 修改请求头后上游可见新头。
- [ ] 响应体正则替换后客户端收到新体。
- [ ] Location 限制生效。
- [ ] 非法正则无法保存。

## 交叉链接

- [02](02-platformArchitecture.md) · [03](03-locationMatching.md) · [19](19-breakpoints.md)
- [16](16-mapLocal.md) · [17](17-mapRemote.md)
