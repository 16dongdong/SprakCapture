# 07 请求响应查看器

## Charles 对照

Charles Contents 提供 Headers、Text、JSON、XML、Image、Hex、Form URL-encoded、Cookies、Raw 等视图，按 Content-Type 自动选择默认页签。

## 目标

- 检查器 Contents 区进入后自动加载 headers + body，不提供额外启用配置。
- P0 必达：Headers、Text、JSON、Hex、Raw 元信息。
- P1：Form、Cookies、Image 预览、自动解压 gzip/br 后展示。
- 只读查看；编辑改包走 Breakpoints / Compose（[19](19-breakpoints.md)、[27](27-repeatAndEdit.md)）。

## 非目标

- P0 不做完整 HTML DOM 渲染引擎。
- 不做在线病毒扫描。

## 领域模型

```typescript
type ViewerTab =
  | "headers"
  | "text"
  | "json"
  | "hex"
  | "form"
  | "cookies"
  | "image"
  | "raw";

interface ViewerModel {
  side: "request" | "response";
  headers: Array<{ name: string; value: string }>;
  bodyMeta: BodyHandleMeta | null;
  text?: string;
  parseError?: string;
  defaultTab: ViewerTab;
  availableTabs: ViewerTab[];
}

interface JsonViewerState {
  expandLevel: number;
  search: string;
}

interface HexViewerState {
  offset: number;
  bytesPerRow: 16 | 32;
}
```

### Content-Type → 默认页签

| Content-Type | 默认 |
|---|---|
| application/json、+json | json |
| text/*、application/javascript、xml | text |
| image/* | image |
| application/x-www-form-urlencoded | form |
| 其他 / 空 | headers 或 hex（二进制启发式） |

## 行为

1. 选中事务 → Overview 即时显示 Summary；切换 Contents 时自动请求 body API。
2. 若 `Content-Encoding` 为 gzip/br/deflate，提供 `?decode=1`（默认 true）解码正文，meta 标明 `decodedFrom`。
3. JSON 解析失败 → 显示 Text + parseError。
4. 超大正文：Hex/Text 虚拟化滚动；JSON 树懒展开。
5. 复制：复制 headers、复制 body 文本、复制 URL。

### Body API

```http
GET /api/v1/transactions/{id}/response/body?decode=1
```

```typescript
interface BodyResponse {
  meta: BodyHandleMeta & { decodedFrom?: string };
  base64: string;
}
```

## UI 要点

- Request | Response 两侧页签或上下分割。
- Headers 表：名称、值；搜索过滤。
- JSON：可折叠树 + 原始文本切换。
- Hex：偏移 | hex | ASCII。
- 截断横幅：正文字节已截断。

## UI 操作指南

### 界面位置

L2 **连接会话 → 检查器**（非对话框）。保存文件时用系统 L4 文件框。

### 如何打开

在 L2 选中事务即可；无需菜单。

### 操作步骤

1. 切换概览/请求/响应等页签与正文子视图。
2. 复制用检查器工具条；保存用 **保存…** → 系统文件对话框。
3. 备注在检查器备注页签失焦保存。

### 预期行为

检查器永不打开成「挡住整桌的唯一窗口」；与 L3 工具对话框同时存在时 L3 在上，关闭 L3 后检查器内容仍在。


## 验收标准

- [ ] JSON API 响应默认进入 JSON 树且可展开。
- [ ] gzip 响应 decode 后可读。
- [ ] 切换事务会取消或忽略过期 body 请求（无串数据）。
- [ ] 无 body 的 204 不报错。
- [ ] Hex 与 base64 解码字节一致。

## 交叉链接

- [05](05-transactionModel.md) · [06](06-structureSequenceFocus.md) · [31](31-protobufAndAmf.md)
- [19](19-breakpoints.md) · [08](08-chartsAndOverview.md)
