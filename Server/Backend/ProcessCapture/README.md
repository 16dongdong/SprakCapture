# WinDivert 进程捕获核心

该模块使用 WinDivert 2.2.2 的三层事件模型：

- `SOCKET` 层观察 TCP/UDP `CONNECT/CLOSE` 元数据，并在用户态按可热更新 PID 集取得原始五元组；
- `FLOW` 层补充无连接 UDP 的权威远端并在删除事件中清理端点生命周期；
- `NETWORK` 层改写已进入五元组表的 TCP 包；命中的 IPv4/IPv6 UDP 数据报进入统一封包写线入口，决定放行、替换或丢弃，成功回注后才发布最终 payload 录制事件。

出站包通过交换源/目标 IP、分配唯一反射源端口并把目标端口改为融合代理端口，转为本机入站连接。代理回复执行逆向改写，恢复原远端 IP、远端端口与客户端本地端口。唯一反射源端口避免同一进程本地端口并发访问同一远端 IP 的不同端口时发生连接碰撞。

UDP 不套用 TCP 地址反射，也不伪造 SOCKS5 UDP 首部。SOCKET/FLOW 提供 PID 与五元组归属，NETWORK 主动拦截后把完整 payload 交给与 SOCKS5 共用的最终写线规则；未修改数据报原样回注，缩短修改会同步更新 IP/UDP 长度与校验和，分片则重组决策后按原边界回注并移除不再承载正文的尾片。SOCKS5 客户端仍通过标准 `UDP ASSOCIATE` 使用独立物理中继，但规则解释与热更新快照不再分叉。

`vendor/WinDivert.dll`、`vendor/WinDivert.lib` 与 `vendor/WinDivert64.sys` 来自官方
`WinDivert-2.2.2-A.zip`。DLL 的 SHA-256 为
`C1E060EE19444A259B2162F8AF0F3FE8C4428A1C6F694DCE20DE194AC8D7D9A2`，导入库为
`C5678D544EB0121A189D1139F54E0C67854DC64D1C897111A27EF2E52CB38EB3`，驱动为
`8DA085332782708D8767BCACE5327A6EC7283C17CFB85E40B03CD2323A90DDC2`。工程固定链接官方动态库，
避免静态实现导出的 CRT 内存函数在发布优化后覆盖系统实现。`build.rs` 会把 DLL、驱动与许可证同时复制到
`target/{profile}` 与 `target/{profile}/deps`；Tauri 安装包也会把三者放到桌面程序可执行文件目录。
进程仍需在管理员权限下启动，驱动服务由 WinDivert 在句柄打开和关闭时管理。

普通测试不会请求管理员权限，真实驱动验证默认忽略。需要验证驱动加载和透明重定向时，应在管理员 PowerShell 中显式执行：

```powershell
$targetDir = Join-Path ([System.IO.Path]::GetTempPath()) ("sprak-process-capture-e2e-" + [guid]::NewGuid().ToString("N"))
try {
    cargo test -p process-capture-core --test runtimeDriverSmoke --target-dir $targetDir -- --ignored --nocapture --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "真实驱动验证失败，退出码：$LASTEXITCODE" }
} finally {
    cargo clean --target-dir $targetDir
    if (Test-Path -LiteralPath $targetDir) {
        Remove-Item -LiteralPath $targetDir -Recurse -Force -ErrorAction Stop
    }
}
```

测试构建完成后，`runtimeDriverSmoke`、`WinDivert.dll`、`WinDivert64.sys` 与
`LICENSE.WinDivert.txt` 均位于同一个 `deps` 目录；任一运行组件缺失都会直接报告加载错误而不是静默跳过。

WinDivert 采用 LGPL-3.0-or-later/GPL 双许可证，原始许可文本保存在
`vendor/LICENSE.WinDivert.txt`，普通发布目录和 Windows 安装包会把该文本与 DLL、驱动一并分发。
