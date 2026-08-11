# 20 屏蔽列表 / 白名单

## Charles 对照

Block List 阻断匹配请求；Allow List（白名单模式）仅允许列表内通过。可用于屏蔽广告域名或限制只测某 API。

## 目标

- 两种模式：`blockList`（黑名单）与 `allowList`（白名单，仅允许命中）。
- Location 匹配；可返回 403 或直接关闭连接。
- 流水线靠前执行（DNS 后、No Caching 前）。

## 非目标

- 不做远程过滤规则订阅（Hosts 广告列表一键导入可 P3）。

## 领域模型

```typescript
type BlockMode = "off" | "blockList" | "allowList";

interface BlockListConfiguration {
  mode: BlockMode;
  locations: Location[];
  /** 合成响应状态码，默认 403 */
  statusCode: number;
  /** 可选纯文本/HTML 体 */
  responseBody: string;
  closeConnection: boolean;
}
```

## 行为

1. `blockList`：命中 → 阻断。
2. `allowList`：未命中 → 阻断；列表空时拒绝全部（需 UI 警告）。
3. `status=blocked`，可录制以便排查。
4. CONNECT 也可在建立前阻断。

## 控制 API

```http
GET /api/v1/tools/blockList
PUT /api/v1/tools/blockList
```

## UI 要点

- 模式单选 + Location 表。
- 白名单空列表危险提示。

## UI 操作指南

### 界面位置

**工具 → 屏蔽列表…** L3；右键 **屏蔽主机** 可即时加规则并可选打开对话框。

### 如何打开

菜单打开「屏蔽列表」对话框。

### 操作步骤

1. 选择模式：关 / 黑名单 / 白名单（白名单红字警告）。
2. 添加… Location → 应用/确定。
3. L2 见 blocked 事务。

### 预期行为

标准开关 + 列表对话框；白名单切换需 L4 确认。


## 验收标准

- [ ] 黑名单域名返回配置状态码且无出站。
- [ ] 白名单外域名被拒，名单内通过。
- [ ] mode=off 无影响。

## 交叉链接

- [03](03-locationMatching.md) · [02](02-platformArchitecture.md) · [22](22-blockCookies.md)
