# 01 功能矩阵

## Charles 对照

本矩阵按 Charles 官方能力域归类，标注 Sprak Capture 实现优先级与对应设计文档。优先级定义：

| 优先级 | 含义 | 典型里程碑 |
|---|---|---|
| P0 | 没有则无法称为抓包工作台 | M1–M2 |
| P1 | 日常开发改包主路径 | M3–M4 |
| P2 | 增强与运维便利 | M4–M5 |
| P3 | 深度协议/自动化，不阻塞主路径 | M6 |

## 矩阵

### 平台与导航

| 能力 | Charles | 优先级 | 文档 |
|---|---|---|---|
| 产品定位与边界 | Proxy + 抓包工作台 | — | [00](00-productVision.md) |
| 平台架构 / 工具流水线 | Tools 管线 | P0 | [02](02-platformArchitecture.md) |
| Location 位置匹配 | Focus / 工具共用匹配 | P0 | [03](03-locationMatching.md) |
| 录制会话 | Recording | P0 | [04](04-sessionAndRecording.md) |
| 事务模型 | Session/Transaction | P0 | [05](05-transactionModel.md) |
| 结构 / 报文标识 / 聚焦 | Structure / Resource Type / Focus | P0/P1 | [06](06-structureSequenceFocus.md) |
| 请求响应查看器 | Contents / Overview | P0 | [07](07-requestResponseViewers.md) |
| 图表与概览 | Overview / Chart | P1 | [08](08-chartsAndOverview.md) |

### 代理与传输

| 能力 | Charles | 优先级 | 文档 |
|---|---|---|---|
| HTTP/HTTPS 正向代理 | Proxy Settings | P0 | [09](09-httpHttpsProxy.md) |
| SSL Proxying / MITM | SSL Proxying | P0 | [10](10-sslMitm.md) |
| SOCKS 代理 | SOCKS Proxy | P0 | [11](11-socksProxy.md) |
| 带宽限制 | Throttling | P1 | [12](12-throttling.md) |
| 反向代理 / 端口转发 | Reverse Proxy / Port Forwarding | P2 | [13](13-reverseProxyAndPortForward.md) |
| 访问控制 / 上游代理 | Access Control / External Proxy | P2 | [14](14-accessControlAndUpstream.md) |

### 工具链

| 能力 | Charles | 优先级 | 文档 |
|---|---|---|---|
| Map Local | Map Local | P1 | [16](16-mapLocal.md) |
| Map Remote | Map Remote | P1 | [17](17-mapRemote.md) |
| Rewrite | Rewrite | P1 | [18](18-rewrite.md) |
| Breakpoints | Breakpoints | P1 | [19](19-breakpoints.md) |
| Block List / Allow List | Block List / Allow List | P1 | [20](20-blockList.md) |
| No Caching | No Caching | P1 | [21](21-noCaching.md) |
| Block Cookies | Block Cookies | P1 | [22](22-blockCookies.md) |
| DNS Spoofing | DNS Spoofing | P2 | [23](23-dnsSpoofing.md) |
| Mirror | Mirror | P2 | [24](24-mirror.md) |
| Auto Save | Auto Save | P2 | [25](25-autoSave.md) |
| Client Process | Client Process | P2 | [26](26-clientProcess.md) |
| 封包滤镜 | WPE Filter | P1 | [43](43-packetFilters.md) |

### 重复、导出与扩展

| 能力 | Charles | 优先级 | 文档 |
|---|---|---|---|
| Repeat / Compose | Repeat / Compose | P2 | [27](27-repeatAndEdit.md) |
| Advanced Repeat | Advanced Repeat | P2 | [28](28-advancedRepeatLoadTest.md) |
| Validate | Validate | P3 | [29](29-validate.md) |
| 导入导出 | Export / Import / HAR | P1 | [30](30-importExport.md) |
| Protobuf / AMF | Protobuf / AMF | P3 | [31](31-protobufAndAmf.md) |
| Web Interface / CLI | Web Interface / CLI | P3 | [32](32-webInterfaceAndCli.md) |
| 移动设备抓包 | SSL Proxying Mobile | P1 | [33](33-mobileDeviceCapture.md) |
| 实现路线图 | — | — | [34](34-implementationRoadmap.md) |

### 扩展：插件与 Agent

| 能力 | 业界对照 | 优先级 | 文档 |
|---|---|---|---|
| 插件与模块系统 | mitmproxy addon / Burp extension / VS Code contribution | **legacy 基础已交付，完整平台重构中** | [38](38-pluginSystem.md) |
| 采集 Agent | 远程节点汇入 / CI sidecar | **延后** | [39](39-agentSystem.md) |
| 分析 Agent | 本地启发式 + 可选 LLM | **延后** | [39](39-agentSystem.md) |
| 扩展调研 | — | 存档 | [37](37-pluginAndAgentResearch.md) |

> 不占用当前 P0–P3 主路径排期；完成 Charles 对等能力后再评估启动。

## 依赖关系（简图）

```text
03 Location ──► 所有工具 (16–25)
04 Recording + 05 Transaction ──► 06/07/08 视图
09 HTTP 代理 ──► 10 MITM ──► 解密后进流水线
02 流水线顺序固定，工具只注册钩子
30 导出 ──► 依赖 05 元数据 + 正文存储
02 流水线扩展槽 ──► 38 插件钩子
05 事务汇入 ──► 39 采集 Agent
05 + 命令总线 ──► 39 分析 Agent / 38 插件命令
```

## 工具流水线顺序（请求方向）

与 Charles 习惯对齐，请求路径固定顺序（响应路径对称/子集，见 [02](02-platformArchitecture.md)）：

1. Access Control / ACL
2. DNS Spoof
3. Block List / Allow List
4. No Caching
5. Block Cookies
6. Map Remote
7. Map Local
8. Rewrite（请求规则）
9. Breakpoints（请求）
10. Throttling
11. 实际上游转发 / 出站

响应路径（上游返回后）：Rewrite（响应）→ Breakpoints（响应）→ Block Cookies（响应剥离）→ Throttling → Mirror / Auto Save 钩子 → 录制落库。

## 与现有实现的映射状态

| 能力 | 当前状态 |
|---|---|
| SOCKS5 数据面 | 已实现 |
| 控制快照 / 事件 | 已实现（仅服务/SOCKS 会话） |
| 结构报文 UI 壳 | 已有会话工作台，待绑定 Transaction |
| HTTP 代理 / MITM / 工具 | 未实现（本矩阵规划） |

## UI 操作指南

本矩阵为能力索引。操作一律按 [35](35-uiShellAndNavigation.md) 的 **菜单 → 对话框** 模式。

| 想做… | 打开方式 |
|---|---|
| 看请求 | L2 连接会话（结构/检查器） |
| 改本地映射 | **工具 → Map Local…** 对话框 |
| 改写头/体 | **工具 → Rewrite…** |
| 断点改包 | **工具 → 断点…** + 命中编辑器 |
| SSL 证书 | **代理 → SSL 代理设置…** |
| 弱网 | 顶栏节流开关；详细 **代理 → 带宽限制设置…** |
| 手机抓包 | **帮助 → 移动设备抓包…** |
| 导出 | **文件 → 导出会话…** 或顶栏导出… |
| 管理插件 / 模块 | 工具栏插件页；完整授权、顺序、诊断、更新和贡献点按 38 实施 |


## 交叉链接

- [00 产品愿景](00-productVision.md)
- [02 平台架构](02-platformArchitecture.md)
- [34 实现路线图](34-implementationRoadmap.md)
