# 40 MCP 系统设计

## 1. 定位

Sprak Capture **MCP（Model Context Protocol）服务器** 是与 **人工 UI / 控制 API 等价** 的机器操作面：

| 操作者 | 入口 |
|---|---|
| 人 | Web/Desktop UI → Control API |
| 外部 AI 宿主 | MCP tools → **同一** Control API |
| **未来分析 Agent / 自动化 Agent** | **优先经 MCP**（或同构调用）操作 Sprak Capture，与人/外部 AI 共用工具面 |

**核心原则：**

1. **功能同步扩展**：每完成一个产品功能，必须同步增加对应 MCP tool（及 `Skill` 说明），禁止「功能已上线但 AI 无法操作」。
2. **等价人工**：MCP 能完成的动作 ⊆ 人工通过 UI/API 能完成的动作；语义对齐菜单/对话框/顶栏，不另造旁路特权业务逻辑。
3. **无权限围栏**：MCP **不**实现插件式权限清单、不降权、不隐藏危险操作。本地控制面已绑定回环；谁能连 MCP 即视为与本机操作者同权。
4. **单一事实源**：业务状态仍以 `proxyService` 权威 snapshot/`revision` 为准；MCP 不持有第二套状态机。
5. **Agent 复用 MCP**：后续开发 Agent（见 [39](39-agentSystem.md)）时，**以 MCP 为标准工具面**，避免 Agent 再实现一套私有控制客户端；Skill 仍写在 `Skill`。

### 2.1 当前界面上下文

主 Web、独立窗口和账号管理 Web 通过 `PUT /api/v1/ui/context` 上报页面、页签、焦点与稳定资源
标识；`capture_ui_get_context` 读取同一聚合视图。该状态只用于让 Agent 接续用户正在查看的对象，
不包含正文、凭据或表单草稿，不持久化，也不推进权威业务 `revision`。窗口停止五秒心跳后按固定
二十秒窗口自动淘汰；乱序请求由窗口内单调 `sequence` 丢弃。

> 与 **完整插件与模块平台（38）** 不同：插件是第三方扩展且有权限模型；MCP 是 **一等公民操作通道**，默认全开。
> 与 **延后的 Agent 产品（39）** 的关系：Agent **实现可延后**，但 MCP **现在就建**；Agent 上线后 **消费 MCP**，而不是另起 API。

## 2. 非目标

- 不在 MCP 层做 RBAC、审批流、二次确认强制（确认类交互由 **Skill 文案** 指导 AI 向用户确认，而非服务端围栏）。
- 不把 MCP 做成公网 API 网关（默认 `127.0.0.1`）。
- 不在 MCP 内复制完整 HAR 解析业务；调用既有导出实现。
- 不在 MCP 任务内实现插件运行时或远程 Agent；插件控制 tools 只复用 38 定义的控制 API。

## 3. 架构

```text
┌──────────────────────────────────────────────────────────┐
│  AI 宿主 / 未来 Analysis Agent / 自动化编排               │
│  + Skill（操作手册，与 MCP 同步维护）                │
└──────────────────────────▲───────────────────────────────┘
                           │ MCP (stdio 或 SSE)
┌──────────────────────────┴───────────────────────────────┐
│  capture-mcp（MCP Server）                                 │
│  · tools/*  1:1 映射控制能力 · 无权限围栏                   │
└──────────────────────────▲───────────────────────────────┘
                           │ HTTP/WS 127.0.0.1:17890
┌──────────────────────────┴───────────────────────────────┐
│  proxyService（与 UI 相同控制面）                          │
└──────────────────────────────────────────────────────────┘
```

**部署：**

- 推荐独立二进制/包：`Mcp/` 或 `tools/capture-mcp`，通过 HTTP 调控制面（解耦、易重启）。
- 可选：嵌入 `proxyService` 同进程（减少进程）；仍须 tool 表与本文目录一致。

**传输：** 默认 **stdio**（桌面 AI 集成最常见）；可选 `http://127.0.0.1:<port>/mcp` 供远程调试（仅回环）。

## 4. 命名与风格

| 项 | 约定 |
|---|---|
| server 名 | `capture` |
| tool 名 | `capture_` + 动词短语，snake_case，稳定不随意改名 |
| 参数 | JSON，**camelCase** 与控制契约一致 |
| 返回 | 结构化 JSON；错误含中文 `message` + 可选 `code` |
| 幂等 | 查询类只读；启停/清空等与 API 相同语义 |

### 4.1 工具分组前缀

| 前缀 | 域 |
|---|---|
| `capture_service_*` | 服务启停、snapshot |
| `capture_recording_*` | 录制 |
| `capture_transaction_*` | 事务查询/正文/备注 |
| `capture_config_*` | 配置 |
| `capture_ssl_*` | SSL/证书 |
| `capture_tool_*` | 各 Charles 工具（block/map/rewrite/…） |
| `capture_breakpoint_*` | 断点队列 |
| `capture_export_*` / `capture_import_*` | 导入导出 |

## 5. 与功能同步的强制规则

### 5.1 完成定义（DoD）扩展

任一功能任务 **「已完成」** 必须同时满足：

1. 后端/UI 按设计可用  
2. **MCP**：本功能对应 tools 已实现并在 catalog 勾选  
3. **Skill**：对应 Skill 段落或独立 Skill 文件已更新（见 [41](41-skillSystem.md)）  
4. 冒烟：用 MCP 调用走通与人工等价的最小路径  

未完成 2–4 → 任务状态不得标「已完成」。

### 5.2 变更流程

```text
改控制 API / 新工具配置
    → 更新 Docs/controlContract.md（若适用）
    → 实现 MCP tool
    → 更新 Docs/Plan/mcp/toolCatalog.md
    → 更新 Skills
    → 验收
```

### 5.3 禁止

- 只做 UI 不做 MCP  
- MCP 走未文档化的私有后门（除非同步升为正式 API）  
- 以「太危险」为由在 MCP 删除人工已有的能力（无围栏政策）

## 6. 工具目录（按阶段增量）

完整表维护于：`Docs/Plan/mcp/toolCatalog.md`。  
下表为阶段摘要：

| 阶段 | 必须新增的 MCP 能力 |
|---|---|
| **M1c** | `capture_service_get_snapshot`、`start`/`stop`、`capture_recording_*`、`capture_transaction_list/get/get_body`、`capture_config_get/update` |
| **M1d** | 与 UI 对齐的查询已覆盖即可；可选 `capture_ui_focus` 不强制 |
| **M2** | `capture_ssl_get/set`、`capture_ssl_export_root`、主机表增删 |
| **M3a** | `capture_pipeline_get_status`（工具启用摘要） |
| **M3b** | `capture_tool_block_*`、`no_caching_*`、`block_cookies_*` |
| **M3c** | `capture_tool_map_local_*`、`map_remote_*` |
| **M3d** | `capture_tool_rewrite_*`、`capture_breakpoint_*` |
| **M3e** | `capture_tool_throttle_*`、`capture_export_*` |
| **M5** | `capture_transaction_repeat`、`repeat_advanced`、`mirror_*`、`auto_save_*`、port/reverse |
| **M6** | validate、protobuf 辅助、CLI 对等命令 |

**已有 SOCKS 基线（可先实现）：** 当前 `controlContract` 已有 snapshot/start/stop/configuration/sessions → MCP 应 **立即** 提供等价 tools，再随 M1 扩展。

## 7. Resources（可选）

| URI | 内容 |
|---|---|
| `capture://snapshot` | 最新权威快照 JSON |
| `capture://recording` | 录制状态 |
| `capture://ssl/root.cer` | 根证书（若已生成） |

Resources 只读；写操作一律走 tools。

## 8. 错误与超时

- 控制面 4xx/5xx → MCP tool 返回 `isError` + 中文 message  
- 长操作（导出大 HAR、高级重复）：支持超时参数；可返回 jobId（若后端有异步，P2）  
- 断点 continue：与 UI 相同 draft schema

## 9. 安全说明（无围栏 ≠ 无文档）

- 默认仅本机；文档声明：**MCP 与本机 UI 同权，可清空会话、改配置、导出流量**。  
- 不在 MCP 内嵌第二套认证；若未来控制面加 token，MCP 配置同一 token。  
- Skill 中应提示：对破坏性操作先向用户确认（产品/操作规范，非服务端拒绝）。

## 10. 实现分期

| 包 | 内容 |
|---|---|
| MCP-0 | 脚手架 + 现有 controlContract 全量 tools + catalog |
| 随 M1c+ | 录制/事务 tools |
| 随各 Mx | 按 catalog 增量 |
| MCP-pkg | README：如何在 Claude Desktop / Cursor 注册 |

任务切片见 `Docs/Plan/MCP0-scaffold.md` 与各 `M*` 中的「MCP/Skill 同步」节。

## 11. 验收（系统级）

- [ ] 人工能做的主路径，MCP 均有 tool  
- [ ] 无「仅 UI 有、MCP 无」的已发布功能  
- [ ] 无 MCP 侧权限拒绝码（除控制面本身业务错误，如运行中改配置冲突）  
- [ ] catalog 与代码 tool 列表一致  
- [ ] 每阶段 Skill 可指引 AI 完成等价操作  

## 12. 交叉链接

- Skill：[41-skillSystem.md](41-skillSystem.md)  
- 工具表：`Docs/Plan/mcp/toolCatalog.md`  
- 控制契约：`Docs/controlContract.md`  
- 计划：`Docs/Plan/README.md`
