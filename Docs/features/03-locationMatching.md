# 03 位置匹配（Location）

## Charles 对照

Charles 的 Location 用 Protocol / Host / Port / Path（及可选 Query）描述目标集合，通配符 `*` 匹配任意段。几乎所有工具（SSL、Map、Rewrite、Block、Focus 等）共用同一匹配语义。

## 目标

- 提供全项目唯一的 Location 解析与匹配实现（Rust），前后端共享 JSON 形状。
- 支持 host 后缀/通配、端口区间或任意、路径前缀/通配。
- 规则列表按「先匹配先生效」或工具自定义优先级；匹配器本身只回答 yes/no。

## 非目标

- 不在 Location 内实现正则全文（Rewrite 等工具可另有 regex 字段）。
- 不做地理位置 IP 库匹配。
- 第一版不强制 Query 匹配（可选扩展字段，默认忽略 query）。

## 领域模型

```typescript
/** 位置：描述一组 URL/连接目标 */
interface Location {
  /** http | https | ws | wss | * | 空表示任意 */
  protocol: string;
  /** 支持 *.example.com、example.com、* 、字面 IPv4/IPv6 */
  host: string;
  /**
   * 空或省略 = 任意端口
   * 单端口: "443"
   * 列表: "80,443"
   * 范围: "8000-8100"
   */
  port: string;
  /** 以 / 开头；* 通配；空 = 任意路径 */
  path: string;
  /** 可选；空 = 不限制 */
  query?: string;
}

interface ResolvedLocation {
  protocol: string;
  host: string;
  port: number;
  path: string;
  query: string;
  /** 原始目标展示串，如 https://api.example.com:443/v1/x */
  display: string;
}

interface LocationMatchOptions {
  /** 默认 false：host 大小写不敏感 */
  caseSensitiveHost?: boolean;
  /** 默认 true：路径匹配忽略末尾多余 / 的差异（根路径除外） */
  normalizePath?: boolean;
}
```

### 匹配语义

| 字段 | 规则 |
|---|---|
| protocol | `*` 或空匹配任意；否则大小写不敏感相等 |
| host | `*` 全匹配；`*.example.com` 匹配子域（不含 `example.com` 自身，除非另有规则）；字面量大小写不敏感 |
| port | 空/`*` 任意；解析列表与闭区间 |
| path | `*` 或空任意；`/api/*` 前缀+通配；支持单段 `*` |
| query | 若配置则子串或 `*` 通配；未配置则忽略 |

CONNECT 隧道在解密前 path 视为 `/` 或空；解密后按完整 URL 再匹配工具。

## 行为

1. 数据面在拿到目标后构造 `ResolvedLocation`。
2. 工具规则带 `Location`（或 location 列表）；引擎用统一 `locationMatches(rule, resolved)`。
3. Focus（聚焦）也是 Location 列表过滤，只影响 UI/可选录制过滤，不改变转发。
4. SSL 主机表使用 Location 子集（通常 protocol=https、path 忽略）。

## 控制 API

Location 本身无独立资源；嵌入各工具配置。提供调试辅助（可选 P2）：

```http
POST /api/v1/debug/location/match
```

```json
{
  "location": { "protocol": "https", "host": "*.example.com", "port": "443", "path": "/api/*" },
  "candidate": { "protocol": "https", "host": "a.example.com", "port": 443, "path": "/api/v1", "query": "" }
}
```

响应：`{ "matched": true }`。

## UI 要点

- 共用 `LocationEditor` 组件：协议下拉、主机、端口、路径。
- 占位提示通配示例：`*.cdn.com`、`/static/*`。
- 无效端口范围即时校验（中文错误）。

## UI 操作指南

Location 无独立菜单项，只出现在 **L3/L4 对话框** 的位置字段组中。

### 界面位置

- 各工具「添加/编辑规则」**子对话框（L4）** 内：协议/主机/端口/路径
- 录制设置、SSL 主机表等 L3 对话框中的列表行

### 如何打开

1. 任意工具菜单打开 L3 → **添加…** 或 **编辑…** → L4 规则编辑器。
2. 结构树右键「Map Local…」等：L3 打开后自动带出 L4，Location 已预填。

### 操作步骤

1. 在 L4 填写协议、主机通配、端口、路径。
2. （可选）点 **试匹配**，输入完整 URL 查看结果。
3. **确定** 回到 L3 规则表 → **应用/确定** 写回服务。

### 预期行为

| 场景 | 预期 |
|---|---|
| 预填后改主机 | 仅改当前草稿，确定前不写后端 |
| 试匹配 | 对话框内即时结果，不关闭 |
| 空位置 | 显示标签「任何位置」 |


## 验收标准

- [ ] 单测覆盖：通配 host、端口范围、路径前缀、大小写、IPv6 字面量。
- [ ] 同一 Location JSON 在 Block / Map / SSL 行为一致。
- [ ] 非法 Location 在保存工具配置时被拒绝。

## 交叉链接

- [02](02-platformArchitecture.md) · [06](06-structureSequenceFocus.md) · [10](10-sslMitm.md)
- [16](16-mapLocal.md)–[25](25-autoSave.md) 各工具
