# 插件系统现状与迁移审查

本文只记录当前实现与完整模块平台之间的差距，不再承担目标架构定义。目标规范见 [38 插件与模块系统](features/38-pluginSystem.md)、[插件与模块开发 API](pluginHookApi.md)和[插件与 Mod 开发者体验规范](pluginDeveloperGuide.md)。

## 1. 当前已实现

- 插件目录扫描与 `.tplugin.zip` 安装。
- 启用、禁用、重载、配置和卸载控制 API。
- 启停状态与配置文件原子持久化。
- 声明式标量配置 Schema 和秘密字段脱敏。
- 管理页面和基础 MCP 列表/启停工具。
- legacy Native 的连接打开、原始流数据和连接关闭回调。
- SOCKS5 TCP/UDP 与未解密 CONNECT 隧道接入 legacy 流回调。
- 完整 manifest 的严格结构模型，包括开放运行时、模块、34 个稳定阶段、动作、自由能力说明、匹配、作者运行说明、依赖和贡献点；插件作者可选择进程内 Native、工作进程、Sidecar 或 Wasm。
- `ExtensionKernel` 的预编译匹配计划、稳定排序、用户覆盖、结构化修改链、失败策略、服务/录制代际和固定容量调用追踪；开放可信模式不执行能力授权、调用超时、事件队列、输出配额或自动熔断。
- `ProcessExtensionRuntime` 已把 Python、TypeScript、Go 和任意独立程序接入 Sidecar/Native Worker JSONL；并发请求用 requestId 归并，宿主不施加超时、并发或输出限制。
- `Server/PluginSDK/` 提供 C++、Rust、Go、Python、TypeScript 的函数式作者 API、示例、构建文件和契约测试。
- 宿主级 `extensionPlatform.json`，完整持久化启停、活动版本、模块顺序、订阅覆盖、失败策略、作者运行说明、配置 Schema 版本、配置正文和秘密引用；更新使用同步临时文件与原子替换。
- 进程内 Native 完整 Mod 生产加载器，以及 SOCKS5 TCP/UDP、CONNECT 隧道的 `TcpChunk`、`UdpDatagram`、`ConnectionClosing` 阶段接线。
- 有状态双向 `StreamTransformerSession`：两个方向独立半包缓冲，支持粘包、多帧输出、修改、扩长、丢弃、关闭、半关闭校验和 exactly-once 资源回收。
- `capture-plugin` 开发命令：从空目录生成插件骨架、校验展开目录、生成公共 JSON Schema、校验阶段夹具；CLI、测试和宿主复用同一模型。
- 扩展平台控制 API：读取/写入/删除完整用户配置，读取运行实例快照与最近调用追踪，清空诊断追踪。

## 2. 已确认差距

| 领域 | 当前状态 | 完整系统要求 |
|---|---|---|
| HTTP | 解密消息只经过内置 `ToolPipeline` | 请求/响应头、流式正文、合成响应、重定向阶段 |
| TLS | 无插件阶段 | ClientHello、证书选择、成功和失败观察 |
| WebSocket | 无帧阶段 | 打开、帧、关闭 |
| WinDivert UDP | 被动录制不经过 legacy 插件 | 明确 observe-only 阶段与录制裁决 |
| DNS | 无结构化阶段 | 查询/响应解析、修改、合成和标注 |
| 录制 | 无插件阶段 | 录制前裁决、事务标注、完成和清空代际 |
| UI | 只有插件管理页 | 设置、命令、检查器页签、上下文动作、状态项 |
| 运行时 | 统一运行时接口，以及 Native、Sidecar、Native Worker 生产适配器已完成 | Wasm 生产适配器 |
| 能力说明 | manifest 能力清单与用户配置已持久化；开放可信模式不把能力清单作为运行门禁 | 管理页展示接口使用范围与调用诊断 |
| 顺序 | 插件清单 与 ExtensionKernel 已支持用户排序、匹配覆盖和执行痕迹；legacy 仍使用 priority | 全部数据面迁移到统一计划 |
| 状态 | 连接私有内存值 | 作者自管容量且可迁移的持久存储 |
| 诊断 | 调用、延迟、输入输出大小、动作、错误和实例并发快照已完成 | 运行时日志、长期指标与诊断包 |
| 分发 | 安装/卸载基础 | 签名、来源、依赖、更新、锁文件和回滚 |
| 开发者工具 | 五种 SDK、可加载示例、目录校验、公共 Schema、阶段夹具校验、模拟器与有状态流核心已完成 | 统一打包、签名、发布与可视化追踪 |

## 3. legacy 风险

- 同步 Native 回调可以阻塞 Tokio 网络任务。
- 动态库越界访问和崩溃会影响代理进程。
- 原地缓冲只能在输入容量内修改，不能通用生成更长输出。
- `streamMatch` 只覆盖主机、端口和传输层。
- 插件无法声明进程、协议、HTTP 方法、路径、MIME、方向或事务标签规则。
- legacy 回调无法贡献结构化解码和 UI。

## 4. 迁移原则

1. 保留现有包和连接行为，不再扩展 legacy ABI。
2. 先建立 ExtensionKernel，再接入新的运行时和阶段。
3. legacy 插件通过适配器映射到有限阶段，并在 UI 标记兼容模式。
4. 新插件只使用完整 manifest 和模块 API；线字段 `manifestVersion: 2` 仅是当前文件格式编号。
5. 每迁移一个阶段都增加空插件基线、命中、顺序、失败隔离、停止和重启测试。
6. 进程内 Native 始终作为无隔离、最高自由度的正式运行方式保留；工作进程只是作者可选项。

## 5. 基线证据

- `Server/Backend/PluginHost/src/lib.rs`：legacy manifest、包、配置、生命周期与 C ABI。
- `Server/Backend/HttpProxy/src/pipeline.rs`：当前结构化 HTTP 工具阶段。
- `Server/Backend/Socks5/src/relay.rs`：TCP legacy 流回调接入。
- `Server/Backend/Socks5/src/udpRelay.rs`：显式 SOCKS5 UDP legacy 回调接入。
- `Server/Backend/src/controlApi/pluginControl.rs`：插件控制 API。
- `Server/Frontend/Web/src/pages/pluginManagerPage.tsx`：当前管理页面。

迁移完成前，测试报告必须区分“legacy 流 Hook 通过”和“完整模块系统阶段通过”。
