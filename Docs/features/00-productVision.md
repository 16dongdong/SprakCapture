# 00 产品愿景

## Charles 对照

Charles 是 HTTP/HTTPS 代理 + 抓包 + 协议分析 + 改包工具的集成工作台，而非单纯转发器。其核心价值在于：

1. 把客户端流量透明地引入本地代理；
2. 以「事务」为单位展示请求/响应；
3. 在同一套位置匹配规则上挂载 Map、Rewrite、断点、节流等工具；
4. 提供带资源标识的结构导航、内容查看器与导出能力。

Sprak Capture 对齐该产品形态，并保留已有的高性能 SOCKS5 能力，做成「代理 + 抓包 + 协议分析」网络数据工作台。

## 目标

| 目标 | 说明 |
|---|---|
| 多协议数据面 | HTTP/HTTPS 正向代理 + 现有 SOCKS5 + 可选反向代理/端口转发 |
| 统一抓包模型 | `RecordingSession` 录制会话 + `Transaction` 事务，与 SOCKS `Session` 明确区分 |
| 统一位置匹配 | 所有工具共用 `Location`（协议/主机/端口/路径通配） |
| 固定工具流水线 | 请求/响应路径上顺序确定、可单独启停的工具集合 |
| 权威快照控制面 | 延续 `proxyService` 控制契约：`revision`、camelCase API、完整快照 + 增量事件 |
| 唯一 Web UI | `Frontend/Web` 为唯一界面源，Desktop/浏览器共用 |
| HTTPS MITM | 按主机表解密；根 CA 存安装目录 `data/certs`，叶证书按主机签发 |
| 列表与正文分离 | 列表快照仅元数据；正文按需 API 拉取，支持 spill 与限额 |
| 插件与模块生态（**基础已存在，完整平台重构中**） | **对外开放**：第三方开发者扩展全处理阶段、协议解码、改写、录制、命令与检查器 UI；见 [38](38-pluginSystem.md) |
| Agent（**延后实现**） | 经 MCP 操作 Sprak Capture（含第三方插件）；见 [39](39-agentSystem.md) |

## 非目标

- 不做企业级集中式 APM / 日志平台。
- 不做完整浏览器引擎或自动化浏览器。
- 当前解密面支持 HTTP/1.0、HTTP/1.1 与 HTTP/2；HTTP/3 仍属于 QUIC/UDP 数据面，不冒充 TCP/TLS 解析。
- 不复制 Charles 的商业授权、更新服务或闭源 UI 像素级还原。
- 不在源码目录写入运行配置、证书私钥或会话正文。
- 不引入第二套 Web UI 或双轨控制协议。

## 产品边界

```text
┌─────────────────────────────────────────────────────────┐
│  Desktop Shell (Tauri) / 浏览器                          │
│  唯一 React Web UI                                       │
└──────────────────────────┬──────────────────────────────┘
                           │ HTTP + WebSocket 控制面
                           │ 127.0.0.1 回环，camelCase，revision
┌──────────────────────────▼──────────────────────────────┐
│  proxyService 控制面                                     │
│  状态机 · 配置 · 快照 · 事件 · 工具配置 · 录制控制         │
└──────────────────────────┬──────────────────────────────┘
                           │
     ┌─────────────────────┼─────────────────────┐
     ▼                     ▼                     ▼
 HTTP/HTTPS 代理      SOCKS5 数据面         反向/端口转发
     │                     │                     │
     └──────────► capture-core / 工具流水线 ◄────┘
                       │
                       ▼
              RecordingSession + Transaction
```

## 关键决策摘要

1. **多协议数据面**：HTTP 代理与 SOCKS5 可同时监听；反向代理/端口转发为 P2 可选入口。
2. **Location + 工具流水线**：横切能力，见 [03](03-locationMatching.md)、[02](02-platformArchitecture.md)。
3. **Transaction ≠ SOCKS Session**：前者是应用层请求-响应对；后者是传输层中继会话。
4. **RecordingSession ≠ SOCKS Session**：前者是抓包容器（启停录制、清除、保存）；后者是活跃连接。
5. **MITM 主机表**：默认不解密；仅匹配主机才签发叶证书并解密。
6. **正文按需**：`GET /api/v1/transactions/{id}/request|response` 取正文；列表事件不含 body。
7. **优先级**：P0 底座 → P1 日常改包 → P2 增强 → P3 深度协议/自动化。路线图见 [34](34-implementationRoadmap.md)。

## 成功定义

开发者可以完成 Charles 日常主路径：


## 与现有架构的关系

| 现有组件 | 演进 |
|---|---|
| `socks5-core` | 保留为独立数据面库；会话仍进 `sessions` 快照 |
| `proxyService` | 扩展为多监听入口 + capture + 工具配置 |
| 控制契约 | 字段只增不删一个版本；JSON 继续 camelCase |
| Web 连接工作台 | 结构报文绑定 Transaction；SOCKS 会话只作底层诊断源 |
| Desktop | 仍只负责窗口/托盘/子进程，不复制业务逻辑 |

## UI 操作指南（产品级）

层级与对话框总图见 [35-uiShellAndNavigation.md](35-uiShellAndNavigation.md)。

日常主路径（工作台 + 对话框，无全页工具编辑）：

1. 启动应用 → L2 默认进入 **连接会话**。
2. 菜单 **代理 → 代理设置…** 配置监听；**SSL 代理设置…** 装证书（L3 对话框）。
4. 顶栏 **录制** 打开 → 在结构中观察带类型图标的事务 → 检查器分析（均在 L2）。
5. 需要改包：菜单 **工具 → Map Local… / Rewrite… / 断点…** 打开 **L3 对话框** 配规则。
6. 菜单 **文件 → 导出会话…** 打开导出对话框。

原则：**主窗口只看流量；配置一律对话框。**


## 交叉链接

- [01 功能矩阵](01-featureMatrix.md)
- [02 平台架构](02-platformArchitecture.md)
- [34 实现路线图](34-implementationRoadmap.md)
- [总体架构](../architecture.md)
- [控制契约](../controlContract.md)
- [术语](../terminology.md)
