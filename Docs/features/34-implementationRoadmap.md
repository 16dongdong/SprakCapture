# 实现路线图

> **可委派的任务切片**见 [`Docs/Plan/`](../Plan/README.md)（按 M1a→M6 拆分，供 Codex 分阶段开发）。

## 原则

1. 每期可演示、可测试、可回滚。
2. 先底座后工具，先 HTTP 明文后 MITM，先录制后改写。
3. 不破坏现有 SOCKS5 与控制契约兼容（字段只增不删一个版本）。

## 里程碑

### M0 — 产品重定位与文档（本批）

- [x] Charles 功能调研与矩阵
- [x] 分功能设计文档
- [x] UI 壳层总图 + 各功能「UI 操作指南」（位置/打开/步骤/预期）
- [x] Charles 式对话框配置 + L0–L4 界面层级（见 35）
- [x] UI 组件规范：令牌、对话框页脚、表单与工作台控件（见 36）
- [x] 插件 / Agent 调研与设计（见 37–39）
- [x] 更新根 README 产品描述

### M1 — 事务底座 + HTTP 明文代理（P0）

**交付**

- `http-proxy-core`：HTTP 正向代理 + CONNECT 隧道
- `capture-core`：RecordingSession / Transaction 元数据 + 正文存储
- 录制开关、忽略列表、大小限制
- 控制 API 扩展 + Web 结构报文绑定 Transaction
- 基础查看器：Headers / Text / JSON / Hex

**验收**：浏览器经代理访问 HTTP 站点，工作台可见事务。

**依赖文档**：[02](02-platformArchitecture.md), [03](03-locationMatching.md), [04](04-sessionAndRecording.md), [05](05-transactionModel.md), [06](06-structureSequenceFocus.md), [07](07-requestResponseViewers.md), [09](09-httpHttpsProxy.md)

### M2 — HTTPS MITM + 证书（P0）

- 根 CA、叶证书、SSL 主机表
- 解密后 HTTP 解析接入工具链空管线
- Windows 证书导出/安装引导
- 移动端帮助页

**验收**：信任根后查看 HTTPS JSON。

**依赖**：[10](10-sslMitm.md), [33](33-mobileDeviceCapture.md)

### M3 — 核心工具（P1）

顺序建议实现：

1. Block List — [20](20-blockList.md)
2. No Caching / Block Cookies — [21](21-noCaching.md), [22](22-blockCookies.md)
3. Map Remote — [17](17-mapRemote.md)
4. Map Local — [16](16-mapLocal.md)
5. Rewrite — [18](18-rewrite.md)
6. Breakpoints — [19](19-breakpoints.md)
7. Throttling — [12](12-throttling.md)
8. 导出 HAR / Native — [30](30-importExport.md)

**验收**：Charles 日常改包主路径可完成。

### M4 — 系统集成与分析增强（P1–P2）

- Focus、图表瀑布 — [06](06-structureSequenceFocus.md), [08](08-chartsAndOverview.md)
- Client Process — [26](26-clientProcess.md)
- DNS Spoof — [23](23-dnsSpoofing.md)
- Upstream Proxy / ACL 强化 — [14](14-accessControlAndUpstream.md)
- 导入 HAR — [30](30-importExport.md)

### M5 — 重复、镜像、自动保存、端口（P2）

- Repeat / Edit — [27](27-repeatAndEdit.md)
- Advanced Repeat — [28](28-advancedRepeatLoadTest.md)
- Mirror / Auto Save — [24](24-mirror.md), [25](25-autoSave.md)
- Port Forward / Reverse Proxy — [13](13-reverseProxyAndPortForward.md)

### M6 — 协议深度与自动化（P3）

- Protobuf / 可选 AMF — [31](31-protobufAndAmf.md)
- Validate — [29](29-validate.md)
- CLI / headless 完备 — [32](32-webInterfaceAndCli.md)
- 透明代理
- Charles XML 兼容

### MCP / Skill / i18n — **全程同步（不延后）**

- 设计：[40](40-mcpSystem.md)、[41](41-skillSystem.md)、[42](42-i18n.md)
- 计划：
  - MCP：`Docs/Plan/MCP0-scaffold.md`、`00-mcpAndSkillSync.md`、`mcp/toolCatalog.md`
  - i18n：`Docs/Plan/I18N0-scaffold.md`、`00-i18nSync.md`、`i18n/localeCatalog.md`
- **每完成用户可见/控制面能力 → 同交付 MCP + Skill + Tier-1 全语言文案**
- MCP 无权限围栏；Skill 在 `Skill/`
- 一等语言：`en` `zh-Hans` `zh-Hant` `ja` `ko` `es` `fr` `de` `pt-BR` `ru`
- MCP-0 / I18N-0 可与 M1a 并行

### M7 — 完整插件与模块平台（**正在从 legacy 宿主重构**）

> 当前包管理、配置、管理页和 Native 原始流适配器已经存在；它们只是迁移基础。
> 完整阶段内核与隔离运行时按 [`Docs/Plan/Plugin/PLUGIN-full-plan.md`](../Plan/Plugin/PLUGIN-full-plan.md) 的 P0–P9 实施。

要点：

- 覆盖服务、连接、TLS、HTTP、WebSocket、TCP、UDP、DNS、录制和 UI 阶段。
- 默认 Wasm，sidecar 与 Native 工作进程共享同一阶段/动作契约。
- 能力授权、用户排序、匹配规则、预算、熔断和诊断属于宿主基础能力。
- UI 使用设置、命令、检查器、上下文动作和状态贡献点。
- MCP、CLI、桌面端和 SDK 使用同一控制 API。

设计：[38](38-pluginSystem.md)

### M8 — Agent（**更后**）

- [39](39-agentSystem.md)；操作经 **MCP**，可驱动第三方插件  

调研：[37](37-pluginAndAgentResearch.md)

### M0 补充

- [x] 插件 / Agent 业界调研与完整插件系统契约

## 建议 PR 切片（跨里程碑）

| PR 主题 | 内容 |
|---|---|
| capture-model | Transaction/Recording 类型与存储 |
| http-proxy-listen | 明文 HTTP 代理 |
| ui-transaction-bind | 前端接新模型 |
| ssl-ca | 证书与 MITM |
| tool-pipeline | 流水线框架 + Location |
| tool-block-map | Block + Map Local/Remote |
| tool-rewrite-bp | Rewrite + Breakpoints |
| throttle-export | 节流 + HAR |
| repeat-mirror | 重复与镜像 |
| extension-kernel | 按插件计划 P0–P9 独立切片，不继续扩展 legacy ABI |
| （延后）capture-agent-gw | 同上 |
| （延后）analysis-l1 | 同上 |

## 测试策略

- Rust：协议单元 + 代理集成（hyper/reqwest 客户端）
- 前端：查看器/规则编辑 vitest
- 手工：Chrome 代理、手机一条龙脚本清单见 `Docs/testing.md` 后续增补
- 回归：现有 `socks5-core` 测试必须保持绿

## 风险

| 风险 | 缓解 |
|---|---|
| MITM 与 HTTP/2/QUIC | HTTP/1.x/2 已由统一连接解析器处理；UDP QUIC 仍保持隧道语义 |
| 正文内存膨胀 | spill + 限额 + 列表无正文 |
| 断点卡死连接 | 超时、最大挂起、退出放行 |
| 范围膨胀 | 严格按 M1→M6，P3 不插入 P0 |

## 成功定义（产品级）


## 交叉链接

- [00](00-productVision.md) · [01](01-featureMatrix.md) · [02](02-platformArchitecture.md)
- [索引](README.md)
