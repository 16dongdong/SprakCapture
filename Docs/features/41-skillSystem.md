# 41 Skill 系统设计

## 1. 定位

**Skill** 是给 AI 宿主使用的 **操作手册**：描述何时用哪些 **MCP tools** 完成与人类相同的 Sprak Capture 工作流。

| 产物 | 受众 | 内容 |
|---|---|---|
| MCP Server | 运行时 | 可调用的 tools |
| Skill | AI 模型 | 场景、步骤、参数示例、注意事项 |

**强制同步：** 每完成一个功能 → 更新 MCP → **同步更新 Skill**。缺 Skill 视为功能未交付完整。

## 2. 原则

1. **等价人工路径**：Skill 步骤应能映射到 UI 操作指南（features 里「UI 操作指南」或菜单路径）。  
2. **先查后改**：指导 AI 先 `get_snapshot` / list，再 mutate。  
3. **破坏性操作**：Skill **要求** AI 先向用户确认（清空、再生证书、白名单模式等）；**不**依赖 MCP 权限围栏。  
4. **中文**：Skill 正文默认中文，与产品 UI 一致。  
5. **无权限表**：Skill 不写「需要 xx 权限」；只写操作后果。

## 3. 目录布局（固定）

**唯一落地路径：** `Server/Skill/`（不要写到仓库根 `Skills/` 或其他位置）。

```text
Server/
  Skill/
    SKILL.md                     # 总控：何时加载、全局流程
    references/
      service-and-config.md
      recording-and-transactions.md
      ssl-mitm.md                 # 随 M2 增加
      tools-map-rewrite.md        # 随 M3 增加
      tools-breakpoints.md
      export-import.md
    examples/                    # 可选
      capture-http-api.md
```

与 Grok/Claude Skill 习惯对齐：`SKILL.md` 含 frontmatter（name、description、触发词）。  
后续 **Agent** 开发时加载同一套 `Server/Skill`，经 **MCP** 调工具（见 [40](40-mcpSystem.md)、[39](39-agentSystem.md)）。

### 3.1 SKILL.md frontmatter 示例

```yaml
---
name: capture
description: >
  操作本机 Sprak Capture 网络数据工作台：代理启停、录制、查看/改写 HTTP(S) 事务、
  Map/Rewrite/断点、导出 HAR。通过 MCP server「capture」调用，与人工 UI 等价。
triggers:
  - Sprak Capture
  - 抓包
  - Charles
  - Map Local
  - 断点改包
---
```

## 4. 编写规范

每个 reference 文档固定结构：

```markdown
# 场景名

## 对应人工操作
菜单/顶栏路径（与 features UI 操作指南一致）

## 前置
服务是否需 running、是否需录制等

## MCP 步骤
1. tool 名 + 参数示例
2. …

## 成功标准
AI 应观察到的返回字段

## 失败处理
常见中文错误与下一步

## 同步版本
对应 Plan 阶段 / 功能文档编号
```

## 5. 与阶段同步

| 阶段 | Skill 必须覆盖 |
|---|---|
| MCP-0 / 基线 | 启停服务、读 snapshot、改配置（运行中自动断连并重启数据面）、清 sessions |
| M1c–M1d | 录制开关、列事务、读正文、清空录制 |
| M2 | SSL 主机表、导出/提示安装根证、解密验收流 |
| M3b–M3e | 各工具 get/set、断点 continue/abort、导出 HAR |

总控 `SKILL.md` 维护 **决策树**：「用户想改包 → 读 tools-map-rewrite / breakpoints」。

## 6. 与 MCP 的关系

```text
用户意图
  → Skill 选择流程
    → MCP tools 调用
      → Control API
```

Skill **禁止** 教 AI 直接改用户数据目录文件绕过 API（除非 MCP 尚未提供且任务明确要求——应优先补 MCP）。

## 7. 验收

- [ ] 新 MCP tool 在 48h 交付窗口内出现在某 Skill reference  
- [ ] 按 Skill 逐步执行可完成声明场景（人工用 MCP 客户端演练）  
- [ ] 破坏性步骤含「先问用户」  
- [ ] 无过时 tool 名  

## 8. 交叉链接

- MCP：[40-mcpSystem.md](40-mcpSystem.md)  
- 落地目录：**`Server/Skill/`**（唯一）  
- 计划与 catalog：`Docs/Plan/00-mcpAndSkillSync.md`、`Docs/Plan/mcp/toolCatalog.md`  
- Agent 复用 MCP + 本 Skill：[39-agentSystem.md](39-agentSystem.md)  
- UI 对照：`Docs/features/35`、各功能 UI 操作指南
