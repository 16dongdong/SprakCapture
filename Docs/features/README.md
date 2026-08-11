# Charles 对等能力 — 功能设计文档索引

> 调研基于 Charles 官方文档（[charles.xin](https://www.charles.xin/documentation/index.html)、[charlesproxy.com](https://www.charlesproxy.com/documentation/)）。  
> 产品定位：**代理 + 抓包 + 协议分析** 一体的网络数据工作台，而非单纯 SOCKS5 转发器。

## 阅读顺序

1. [00 产品愿景](00-productVision.md)
2. [01 功能矩阵](01-featureMatrix.md)
3. [02 平台架构](02-platformArchitecture.md)
4. [35 UI 壳层、层级与对话框体系](35-uiShellAndNavigation.md)（**界面层级 + Charles 式对话框，必读**）
5. [36 UI 组件规范](36-componentSpec.md)（**令牌、对话框/按钮/表单/工作台组件，必读**）
6. [03 位置匹配](03-locationMatching.md)（横切）
7. 按矩阵优先级阅读各功能设计；每篇含 **UI 操作指南**
8. [37 插件与 Agent 调研](37-pluginAndAgentResearch.md) → [38 插件系统](38-pluginSystem.md) → [39 Agent 系统](39-agentSystem.md)
9. [34 实现路线图](34-implementationRoadmap.md)
10. [分阶段开发计划 Docs/Plan](../Plan/README.md)（**委派 Codex 用**）

## 文档列表

| 编号 | 文档 | 优先级 |
|---|---|---|
| 00 | [产品愿景](00-productVision.md) | — |
| 01 | [功能矩阵](01-featureMatrix.md) | — |
| 02 | [平台架构](02-platformArchitecture.md) | P0 |
| 03 | [位置匹配](03-locationMatching.md) | P0 |
| 04 | [会话与录制](04-sessionAndRecording.md) | P0 |
| 05 | [事务模型](05-transactionModel.md) | P0 |
| 06 | [结构导航与聚焦](06-structureSequenceFocus.md) | P0/P1 |
| 07 | [请求响应查看器](07-requestResponseViewers.md) | P0 |
| 08 | [图表与概览](08-chartsAndOverview.md) | P1 |
| 09 | [HTTP/HTTPS 正向代理](09-httpHttpsProxy.md) | P0 |
| 10 | [SSL MITM 与证书](10-sslMitm.md) | P0 |
| 11 | [SOCKS 代理增强](11-socksProxy.md) | P0 |
| 12 | [带宽限制](12-throttling.md) | P1 |
| 13 | [反向代理与端口转发](13-reverseProxyAndPortForward.md) | P2 |
| 14 | [访问控制与上游代理](14-accessControlAndUpstream.md) | P2 |
| 15 | [WinDivert 进程捕获与代理路由](15-processCaptureAndProxyRouting.md) | P0 |
| 16 | [Map Local](16-mapLocal.md) | P1 |
| 17 | [Map Remote](17-mapRemote.md) | P1 |
| 18 | [Rewrite](18-rewrite.md) | P1 |
| 19 | [Breakpoints](19-breakpoints.md) | P1 |
| 20 | [屏蔽列表 / 白名单](20-blockList.md) | P1 |
| 21 | [无缓存](21-noCaching.md) | P1 |
| 22 | [阻止 Cookie](22-blockCookies.md) | P1 |
| 23 | [DNS 欺骗](23-dnsSpoofing.md) | P2 |
| 24 | [镜像](24-mirror.md) | P2 |
| 25 | [自动保存](25-autoSave.md) | P2 |
| 26 | [客户端进程](26-clientProcess.md) | P2 |
| 27 | [重复与编辑](27-repeatAndEdit.md) | P2 |
| 28 | [高级重复 / 负载](28-advancedRepeatLoadTest.md) | P2 |
| 29 | [W3C 验证](29-validate.md) | P3 |
| 30 | [导入导出](30-importExport.md) | P1 |
| 31 | [Protobuf / AMF](31-protobufAndAmf.md) | P3 |
| 32 | [网页控制与 CLI](32-webInterfaceAndCli.md) | P3 |
| 33 | [移动设备抓包](33-mobileDeviceCapture.md) | P1 |
| 34 | [实现路线图](34-implementationRoadmap.md) | — |
| 35 | [UI 壳层、层级与对话框体系](35-uiShellAndNavigation.md) | P0 |
| 36 | [UI 组件规范](36-componentSpec.md) | P0 |
| 37 | [插件与 Agent 业界调研](37-pluginAndAgentResearch.md) | 调研存档 |
| 38 | [插件与模块系统](38-pluginSystem.md) | **legacy 基础已交付，完整平台重构中** |
| 39 | [Agent 系统设计](39-agentSystem.md) | 延后 |
| 40 | [MCP 系统](40-mcpSystem.md) | **P0 同步扩展（不延后）** |
| 41 | [Skill 系统](41-skillSystem.md) | **P0 同步扩展（不延后）** |
| 42 | [国际化 i18n](42-i18n.md) | **P0 同步扩展（不延后）** |
| 43 | [封包滤镜](43-packetFilters.md) | P1 |

## MCP / Skill / i18n（与功能同步，不延后）

| 系统 | 文档 | 要点 |
|---|---|---|
| **MCP** | [40](40-mcpSystem.md) | AI 经 tools 操作 = 人工 UI/API；**无权限围栏**；每功能必增 tool |
| **Skill** | [41](41-skillSystem.md) | 操作手册；落地 `Server/Skill`；与 MCP 同步 |
| **i18n** | [42](42-i18n.md) | 国际主流语言；Tier-1 十语同步补键 |
| 计划 | [00-mcpAndSkillSync](../Plan/00-mcpAndSkillSync.md)、[00-i18nSync](../Plan/00-i18nSync.md) | 交付纪律 |
| 目录 | [toolCatalog](../Plan/mcp/toolCatalog.md)、[localeCatalog](../Plan/i18n/localeCatalog.md) | 勾选表 |

## 扩展能力（插件 / Agent）

| 系统 | 文档 | 状态 |
|---|---|---|
| 调研 | [37](37-pluginAndAgentResearch.md) | 存档 |
| **插件** | [38](38-pluginSystem.md) · [开发 API](../pluginHookApi.md) · [开发者体验](../pluginDeveloperGuide.md) · [实施计划](../Plan/Plugin/PLUGIN-full-plan.md) | 开放可信 Mod、完整阶段内核、作者自选运行时、开发工具链、模块与 UI 贡献 |
| **Agent** | [39](39-agentSystem.md) | M1–M6 后做；经 **MCP + `Server/Skill`**，可驱动第三方插件 |

当前迭代 **只推进 Charles 对等主路径**（代理 / 抓包 / 工具 / UI）+ **MCP / Skill / i18n 同步**。  
插件系统已进入完整平台重构；Agent 仍按 39 的独立节奏实施，并继续复用 MCP。

## UI 操作说明约定

00–33 每篇均含 **UI 操作指南**，固定结构：

1. **界面位置**：菜单路径 / 顶栏开关 / 右键 / **对话框层级（L2–L4）**
2. **如何打开**：打开的是哪一个 **对话框**（或 L2 工作台区域）
3. **操作步骤**：对话框内启用、添加规则、**应用 / 确定 / 取消**
4. **预期行为**：成功、失败、边界时界面与数据表现

### 界面层级与 Charles 式对话框（必读）

| 层级 | 含义 |
|---|---|
| **L2** | 主工作区：仅 **概览** + **连接会话**（结构/检查器） |
| **L3** | 模态对话框：工具与代理配置（Map / Rewrite / SSL / 录制设置…） |
| **L4** | 子对话框：单条规则、确认、文件选择、断点编辑 |

**配置进对话框，工作台只观察**——对齐 Charles 的 Tools / Proxy 菜单。  
- 放在哪、谁打开谁 → [35](35-uiShellAndNavigation.md)  
- 长什么样、按钮/Esc/间距 → [36](36-componentSpec.md)

## 维护约定

- 实现某功能前以对应文档为契约；行为变更先改文档。
- 新增能力时：更新矩阵 + 设计文档 + UI 操作指南 + 路线图。
- 增删菜单项 / 对话框 / 顶栏开关时先改 [35](35-uiShellAndNavigation.md)。
- 新增通用控件、改按钮语义或令牌时先改 [36](36-componentSpec.md)。
- **禁止**再引入与连接会话平级的「工具全页」作为主配置入口。
- 控制 API 字段变更同步 [controlContract.md](../controlContract.md)。
