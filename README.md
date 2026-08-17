# Sprak Capture

仓库由两个可独立构建、独立发布的项目组成：

- [`Client/`](Client/README.md)：Android SOCKS5 客户端，使用 Kotlin 与 Jetpack Compose。
- [`Server/`](Server/README.md)：Windows 网络调试服务、Web 工作台与桌面外壳，使用 Rust、React 与 Tauri 2。

根目录不再承担 Cargo 或 pnpm 工作区职责。进入对应项目目录后，使用该项目自己的工具链完成构建、测试和发布。

## 项目关系

```text
SprakCapture/
├─ Client/       Android 应用工程
├─ Server/       服务端与管理工作台工程
└─ Contracts/    两个项目之间的稳定协作契约
```

客户端与服务端在源码和构建系统上完全分离。运行时依赖方向只有 `Client -> Server`：客户端通过标准 SOCKS5 协议连接服务端，并在 APK 构建阶段注入节点地址；服务端不引用客户端源码，也不要求客户端参与构建。

双方共同遵守的协议、认证、节点注入和兼容性规则见 [客户端—服务端协作契约](Contracts/clientServerContract.md)。

## 客户端

```powershell
cd Client
.\gradlew.bat :app:testDebugUnitTest :app:lintDebug :app:assembleDebug `
  "-PclientNodeHost=127.0.0.1" `
  "-PclientNodePort=1080"
```

完整说明见 [Client/README.md](Client/README.md)。

## 服务端

```powershell
cd Server
pnpm install
pnpm check
pnpm test
```

开发运行和桌面打包说明见 [Server/README.md](Server/README.md)。

## 许可证与贡献

项目使用 [MIT License](LICENSE)。提交变更前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 与 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。安全问题按 [SECURITY.md](SECURITY.md) 处理。
