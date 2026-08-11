# Desktop 外壳

该目录只负责 Tauri 2 桌面生命周期，不复制 `../Web` 的页面、样式或业务状态。主窗口与悬浮面板加载同一份
Web 构建产物，分别使用 `/` 与 `/floating` 路由。

## 原生生命周期

- 使用 Windows 原生标题栏，主窗口和悬浮面板关闭后隐藏到系统托盘。
- 托盘左键切换主窗口；菜单统一切换主窗口、悬浮面板或执行显式退出。
- 单实例启动；第二次运行只恢复并聚焦现有主窗口。
- Desktop 启动时监督 `proxyService`；首启或运行期失败均按固定间隔重试，Web 保持可用并显示控制连接失败。
- 显式退出先向服务标准输入写入 `shutdown`，等待三十五秒覆盖 SOCKS5 三十秒关闭上限、HTTP 控制面一秒排空和进程退出余量，再回收超时进程。

## Web 构建

- 开发地址：`http://127.0.0.1:5173`
- 生产产物：`../Web/dist`
- `pnpm dev` 与 `pnpm build` 会先调用同级 `Web` 工程脚本。

## 代理服务产物

`pnpm dev` 与 `pnpm build` 已统一管理后端产物，不需要手工设置环境变量或修改 Tauri 配置：

```powershell
pnpm dev
pnpm build
```

生命周期脚本在工作区根目录执行 `cargo build -p proxy-backend`：

- 开发态构建 debug 后端，并通过 `PROXY_SERVICE_PATH` 向 Tauri 进程注入绝对产物路径。
- 打包态构建 release 后端，将产物暂存为 `src-tauri/resources/proxyService.exe`；稳定的
  `bundle.resources` 映射会把它收录为安装目录中的 `proxyService.exe`。
- 后端默认把持久化数据写入 `proxyService.exe` 同级的 `data/`，主配置路径为
  `data/configuration.json`；安装到非系统盘后不会再使用 C 盘用户数据目录。
- Tauri 结束后始终删除暂存资源，构建产物只保留在 Cargo `target` 目录，不进入源码提交。
- 自定义 `CARGO_TARGET_DIR` 时，脚本按 Cargo 工作区根目录解析相对路径；未设置时从
  `cargo metadata` 获取实际目标目录。

路径解析测试可独立执行：

```powershell
pnpm test:paths
```
