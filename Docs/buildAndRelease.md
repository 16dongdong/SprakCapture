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

## 首次发布

```powershell
pnpm desktop:build
```

Desktop 生命周期脚本会依次完成：

1. 以 `--release` 构建 `proxyService`。
2. 将后台程序临时复制为 Desktop 外部二进制资源。
3. 构建 React 静态资源。
4. 使用 Tauri 生成中英文 NSIS 安装包；安装器允许选择当前用户或所有用户，并允许选择安装目录。
5. 无论构建成功或失败，都删除源码目录中的临时后台资源。

首发只发布 `Sprak-Capture-Setup-x64.exe` 与 `SHA256SUMS.txt`，不生成 MSI，也不显示旧配置导入或迁移入口。安装包和应用当前未使用商业代码签名，GitHub Release 必须明确提示 Windows SmartScreen 可能显示“未知发布者”，并要求用户核对 SHA-256。WinDivert 驱动继续保留上游原始签名。

发布产物生成后，需要在干净 Windows 环境验证自定义安装目录、启动、托盘、悬浮窗、配置持久化、修复安装、保留数据卸载与彻底卸载。

源码仓库不提交 `target`、`node_modules`、`dist`、日志、配置、会话导出或安装包。
