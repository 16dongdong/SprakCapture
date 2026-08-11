# 构建与发布

## 工具链

- Rust stable，包含 `rustfmt` 和 `clippy`
- Node.js 24 或当前维护版本
- pnpm 10
- Windows WebView2 和 Tauri 2 构建依赖

## 安装依赖

```powershell
pnpm install
```

## 开发

```powershell
cargo run -p proxy-backend
pnpm web:dev
pnpm desktop:dev
```

## 完整验证

```powershell
pnpm check
pnpm test
```

## 发布

```powershell
pnpm desktop:build
```

Desktop 生命周期脚本会依次完成：

1. 以 `--release` 构建 `proxyService`。
2. 将后台程序临时复制为 Desktop 外部二进制资源。
3. 构建 React 静态资源。
4. 使用 Tauri 生成 MSI 与 NSIS 安装包。
5. 无论构建成功或失败，都删除源码目录中的临时后台资源。

发布产物生成后，需要在干净 Windows 环境验证安装、启动、托盘、悬浮窗、卸载和回滚。

源码仓库不提交 `target`、`node_modules`、`dist`、日志、配置、会话导出或安装包。
