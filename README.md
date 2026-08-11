# Sprak Capture

Sprak Capture 是面向 Windows 的开源网络调试工作台，使用 Rust、React 与 Tauri 2 构建。它把 HTTP(S)、SOCKS5、WinDivert 进程流量、TCP/UDP 录制、WPE 封包滤镜和插件运行时汇合到同一条事务处理管线中，适合协议分析、接口调试与本地流量诊断。

> 项目仍在快速开发阶段。公开接口以 `Server/PluginSDK/PROTOCOL.md` 和仓库内控制契约为准。

## 主要能力

- HTTP/HTTPS 正向代理、CONNECT 隧道与证书信任链管理
- SOCKS5 `CONNECT`、`BIND`、`UDP ASSOCIATE`，覆盖 IPv4、IPv6 与域名目标
- WinDivert 按进程捕获 TCP/UDP，动态跟踪进程路径与新 PID
- 实时事务树、请求/响应正文、十六进制视图与音频/视频/图片/页面预览
- WPE 风格封包滤镜：通配搜索、变长替换、丢弃与关闭连接
- 重写、断点、本地映射、远程映射、SSL 代理和录制规则集
- 原生与 Sidecar 插件运行时，以及 Rust、Go、C++、Python、TypeScript SDK
- WebSocket/SSE 实时事件、MCP 控制接口与 Tauri 桌面客户端
- 配置、规则和进程路径持久化；正文采用内存预算与磁盘 spill 管理

## 仓库结构

```text
Server/Backend/           Rust 后端、代理、录制、插件与进程捕获
Server/Frontend/Web/      React Web 工作台
Server/Frontend/Desktop/  Tauri 2 桌面外壳
Server/PluginSDK/         多语言插件 SDK、协议与示例
Server/Mcp/               MCP 控制适配器
Docs/                     架构、协议、配置与功能文档
Client/                   客户端扩展边界
```

## 环境要求

- Windows 10/11 x64
- Rust stable（仓库提供 `rust-toolchain.toml`）
- Node.js 24 或当前维护版本
- pnpm 10
- WebView2（桌面客户端）
- WinDivert 进程捕获需要管理员权限

## 从源码运行

```powershell
pnpm install
cargo run -p proxy-backend
pnpm web:dev
```

浏览器打开 `http://127.0.0.1:5173/connections`。控制接口默认监听 `127.0.0.1:17890`，代理默认监听 `0.0.0.0:1080`。

桌面客户端：

```powershell
pnpm desktop:dev
```

## 构建与测试

```powershell
pnpm check
pnpm test
```

更多信息见 [构建与发布](Docs/buildAndRelease.md) 和 [测试说明](Docs/testing.md)。

## 插件开发

插件可订阅协议阶段、读取或修改载荷、改变连接决策，并接管受支持的扩展点。SDK 覆盖 Rust、Go、C++、Python 和 TypeScript。请从 [Plugin SDK](Server/PluginSDK/README.md) 和 [冻结协议](Server/PluginSDK/PROTOCOL.md) 开始。

## 安全与隐私

请勿在公开 Issue 中提交私钥、令牌、完整抓包或个人数据。安全问题请按 [SECURITY.md](SECURITY.md) 的流程报告。配置文件、证书私钥、录制正文、日志和数据库均不应提交到 Git。

## 参与贡献

提交 Issue 或 Pull Request 前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)，并遵守 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。

## 第三方组件

WinDivert 按其 LGPL-3.0-or-later 许可分发，许可证文本位于 `Server/Backend/ProcessCapture/vendor/LICENSE.WinDivert.txt`。其他依赖的许可信息以各自清单和锁文件为准。

## 许可证

Sprak Capture 使用 [MIT License](LICENSE) 开源。
