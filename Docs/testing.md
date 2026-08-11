# 测试策略

## Rust

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

必须覆盖：

- 方法协商和用户名密码认证。
- IPv4、IPv6、域名编解码。
- `CONNECT` 回环转发与半关闭。
- `BIND` 两阶段响应。
- `UDP ASSOCIATE` 封装、源端点绑定和控制连接生命周期。
- 非法版本、保留字段、地址类型、命令和截断消息。
- 重复启动、重复停止和有序关闭。
- HTTP 控制响应与 WebSocket 修订号。

## Web

```powershell
pnpm web:build
pnpm web:test
```

必须覆盖：

- 单一状态动作的五种状态。
- 控制接口失败时不伪造成功。
- 单一结构视图的中文文案与端点分组。
- 事务选择、过滤、检查器页签和分隔条。
- 主窗口与悬浮路由共用状态。
- 长域名、IPv6 和中文错误信息完整可访问。

## Desktop

```powershell
pnpm desktop:check
pnpm --dir Server/Frontend/Desktop test
```

必须覆盖：

- 单实例配置。
- 主窗口关闭隐藏。
- 托盘退出有序关闭。
- 悬浮窗口显示和隐藏。
- 后台程序异常退出后的状态传播。

发布前还需要在 Windows 实机执行安装、重复启动、托盘退出和重启回归。

## 测试目录边界

- `src/` 只存放可发布的业务实现，不得放置 `*.test.*`、`*.spec.*`、`#[cfg(test)]` 模块、测试夹具或测试专用辅助函数。
- Rust 每个 crate 的测试放在同级 `tests/`；跨 crate 的共享夹具放在对应 `tests/support/`。
- Web 测试、夹具和测试初始化放在 `Server/Frontend/Web/tests/`，按 `unit/`、`support/` 分层；生产模块只能被测试导入，不能反向依赖测试实现。
- 新增测试前先运行布局检查；布局检查失败时不得将测试文件放回业务目录。
