# 37 插件与 Agent 业界调研

> **状态：调研存档 · 插件平台已进入重构 · Agent 仍为设计预留**
> 完整插件与模块平台以 [38](38-pluginSystem.md) 和[实施计划](../Plan/Plugin/PLUGIN-full-plan.md)为准；Agent 仍见 [39](39-agentSystem.md)。

本文调研主流网络调试 / 安全代理工具的 **扩展模型**，作为 Sprak Capture「插件系统」与「Agent 系统」设计输入。
产品目标仍是：代理 + 抓包 + 协议分析工作台（见 [00](00-productVision.md)），扩展不得破坏控制面权威快照与 L2 工作台分层。

## 1. 调研范围

| 产品 | 扩展形态 | 与 Sprak Capture 相关性 |
|---|---|---|
| mitmproxy | Python Addon：事件钩子 + Options + Commands | 数据面流水线扩展范本 |
| Burp Suite | Java/Kotlin Extension（Montoya API）+ BApp Store | 成熟插件市场与能力边界 |
| Fiddler | FiddlerScript（JS）规则 + 远程设备代理接入 | 脚本改包 + 远程流量 |
| Proxyman | JavaScript Scripting / Add-ons | 现代 Charles 类工具的脚本扩展 |
| Charles | 内置 Tools，几乎无第三方插件 API | 功能完整但扩展弱 → 我们的差异化机会 |
| Wireshark | 解复用器 / 协议 dissectors（C/Lua） | 协议分析扩展，偏包级 |
| IDE / 浏览器扩展 | 远程 Agent、DevTools Protocol | 「旁路采集 Agent」类比 |

## 2. mitmproxy Addon

### 机制

- 插件是 Python 对象，响应 **事件钩子**（`request` / `response` / `tcp_message` / `load` 等）。
- 通过 `Loader` 注册 **Options**（配置项）与 **Commands**（可被 UI/控制台调用）。
- 大量内置能力本身也是 addon，与第三方同一模型。
- 支持脚本热重载；错误记录日志，尽量不影响主进程存活。

### 可借鉴

1. **「核心也是插件」**：内置 Map/Rewrite 与第三方扩展共享钩子，降低双轨逻辑。  
2. **事件驱动**：扩展不轮询，不直接拿 socket。  
3. **Options 声明式**：配置进 snapshot / 对话框表单自动生成的基础。  
4. **命令面**：插件可注册「对当前事务做 X」的动作。

### 不照搬

- 进程内任意 Python 执行面过大，安全与打包（Rust 桌面）成本高。  
- Sprak Capture 主运行时是 Rust，扩展语言需单独选型（见 [38](38-pluginSystem.md)）。

## 3. Burp Suite Extension

### 机制

- 官方推荐 **Montoya API**（Java，Kotlin 可编为 jar）。  
- 扩展可：被动/主动扫描、改请求响应、加 UI 页签、上下文菜单、持久化配置。  
- **BApp Store** 提供发现、安装与审核标准。

### 可借鉴

1. **能力分级 API**：不是裸事件，而是按场景（HTTP 处理、UI、扫描）分包。  
2. **分发与信任**：市场/签名/权限清单，避免任意 jar 即 root。  
3. **UI 扩展点**：页签、上下文菜单，而不是让插件重画整个工作台。

### 不照搬

- 完整 JVM 插件宿主对当前 Rust + React 栈过重。  
- 安全扫描器产品形态与 Sprak Capture 不同，扫描引擎非首期目标。

## 4. Fiddler / Proxyman 脚本

### 机制

- **FiddlerScript**：在 `OnBeforeRequest` / `OnBeforeResponse` 中改 session。  
- **Proxyman Scripting**：JS `onRequest` / `onResponse`，官方 snippet 与 add-on 库。  

### 可借鉴

1. **脚本型插件**上手快，适合改包/过滤/标注。  
2. Snippet 库降低编写成本。  
3. 远程抓包文档化（与 [33](33-mobileDeviceCapture.md) 一致）。

### 不照搬

- 仅脚本无法覆盖「新协议解码器」「新导出格式」等重扩展。  
- 需与声明式工具（Rewrite 对话框）并存，避免强迫用户写代码做简单改头。

## 5. Charles

- 强项：内置工具完整、对话框配置清晰。  
- 弱项：**几乎没有第三方插件生态**；复杂逻辑只能断点手改或外部脚本。  
- Sprak Capture 若做好受控扩展，可作为差异点，但 **内置工具仍必须开箱即用**。

## 6. 「Agent」在业界的多义性

调研中「Agent」常指不同东西，设计必须拆开命名：

| 含义 | 典型场景 | 业界例子 |
|---|---|---|
| **A. 远程采集 Agent** | 在另一台机器/容器内转发或镜像流量到工作台 | 旁路 sidecar、部分 APM probe、企业代理节点 |
| **C. 自动化 / AI Agent** | 根据会话自动分析、生成规则、回归断言 | LLM + 工具调用；CI 里跑代理脚本 |
| **D. 用户代理 UA** | HTTP Header | 与本设计无关，文档避免简称混淆 |

Sprak Capture 的 **Agent 系统** 同时覆盖 **A（采集）** 与 **C（分析自动化）**，并与插件系统正交（见 [39](39-agentSystem.md)）。

## 7. 对比摘要

| 维度 | mitmproxy | Burp | Proxyman/Fiddler | Charles | Sprak Capture 建议 |
|---|---|---|---|---|---|
| 扩展语言 | Python | Java | JS 脚本 | 无 | 分阶段：Wasm/脚本 + 进程插件 |
| 钩子位置 | 流量事件 | 流量+UI+扫描 | 请求/响应脚本 | 内置工具 | 对齐 tool-pipeline 钩子 |
| 配置 | Options | 扩展自管 | 脚本/UI | 对话框 | 插件 manifest + 控制面 |
| 分发 | 本地脚本 | BApp Store | 内置库 | — | 本地目录 + 可选签名 |
| 远程流量 | 上游/模式 | 协作 | 远程设备代理 | 远程设备代理 | Capture Agent + 现有代理 |
| AI | 社区自建 | 社区 | — | — | Analysis Agent 独立子系统 |

## 8. 对 Sprak Capture 的结论

1. **插件** = **对外开放的生态扩展**（第三方开发者发布；流水线/Stream Codec/查看器/导出/UI 贡献），用于丰富协议与场景多样性；**不是** Sprak Capture 内置工具的另一份拷贝。
2. **Agent** = 扩展 **部署拓扑与智能闭环**；操作侧优先走 **MCP**（可启用他人插件）。  
3. 二者共享：日志、控制契约扩展；插件有权限模型，MCP 无围栏。  
4. 分期：扩展内核与契约 → 受控运行时 → 全阶段贡献点 → SDK/开发工具 → 包分发与信任。
5. 安全默认：插件默认只读；stream mutate / 密钥需显式权限；密钥不进 snapshot。

## 9. 参考链接（调研时点）

- mitmproxy Addons：https://docs.mitmproxy.org/stable/addons/overview/
- Burp 扩展与 Montoya：https://portswigger.net/burp/documentation/desktop/extend-burp/extensions/creating
- Proxyman Scripting：https://docs.proxyman.io/scripting/script
- Fiddler 远程捕获文档（厂商文档）

## 10. 后续文档

- [38 完整插件与模块平台](38-pluginSystem.md)（权威架构）
- [插件全量实施计划](../Plan/Plugin/PLUGIN-full-plan.md)
- [39 Agent 系统设计](39-agentSystem.md)
