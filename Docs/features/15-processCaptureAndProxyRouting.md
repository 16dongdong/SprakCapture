# 15 WinDivert 进程捕获与代理路由

## 目标

- 按明确的 Windows 进程编号捕获 IPv4/IPv6 TCP 与 UDP 流量；TCP 透明转入本地 HTTP / SOCKS5 融合监听端口，UDP 在保持原地址路径的同时进入统一封包写线入口并逐包录制。
- 保留原始目标地址，使透明连接能够直接转发、进入 HTTPS 检查或经二级代理建立出站隧道。
- 服务停止、驱动退出或流关闭时恢复数据包方向并清理映射，不留下操作系统全局配置。

## 数据路径

1. WinDivert `SOCKET`/`FLOW` 层观察 TCP 与 UDP 事件，工作线程按可热更新 PID 集记录双栈五元组，删除事件负责回收。
2. `NETWORK` 层仅匹配配置的 `processIds`，排除本地融合端口的递归流量。
3. 出站包重写到融合监听端口，并为每条原始连接分配唯一反射源端口。
4. 监听器通过对端地址查询 `originalTargetForPeer`；命中时跳过公开端口的 HTTP / SOCKS5 分类，以原始 IP 建立固定路由隧道。
5. 隧道分类器在配置的请求头容量与读取超时内识别分片或延迟到达的 HTTP/1、HTTP/2 前言和 TLS ClientHello；HTTP `Host` 与 TLS SNI 只恢复应用层域名，不参与实际路由，因此 CDN、DNS 轮转或不同解析视图不会退化为 IP/TCP 展示。
6. TCP 返回包恢复原始源地址和客户端目标地址，重新计算校验和后注入协议栈。
7. UDP 不修改地址和端口；命中选中进程的双向数据报先进入与 SOCKS5 共用的封包滤镜，未命中时原样回注，等长修改时重算校验和，丢弃时不回注。成功写线后以 `udp://` 事务保存最终 payload 和方向。QUIC 保持 UDP 原始协议，不伪装成 HTTP/3。

## 配置与状态

```typescript
interface ProcessCaptureConfiguration {
  enabled: boolean;
  processIds: number[];
  proxyPort: number; // 控制面强制等于融合监听端口
}

interface ProcessCaptureSnapshot {
  running: boolean;
  configuredProcessIds: number[];
  trackedFlows: number;
  acceptedConnections: number;
  redirectedPackets: number;
  restoredPackets: number;
  bytesUp: number;
  bytesDown: number;
  lastError: string | null;
}
```

## 进程选择与配置持久化

主工具栏的“进程选择器”打开与设置同级的独立进程管理窗口。受视口约束的下拉菜单按可执行路径
合并运行实例，并显示系统提取的应用图标与当前 PID；搜索框支持名称、路径与 PID。添加动作持久化
可执行路径，同一路径的多个实例会展开为全部当前 PID。
已保存但未运行的程序仍保留在列表中，因此应用或代理服务重启后无需重新添加。

进程路径与代理核心设置统一保存在安装目录 `data/configuration.json`，设置页不再接受手工 PID。后台每秒
重新解析全部匹配实例；用户启用的程序尚未运行时仍保持双栈内部监听和 WinDivert 句柄就绪，新实例出现
后无需重启服务即可原子加入。新增、移除路径以及捕获开关全部热更新 WinDivert，不重启公开代理监听；
既有 TCP 控制块会被关闭并由客户端自行重连，UDP 新数据报立即按新 PID 集生效。
二级代理位于左侧独立导航，“代理设置”只承载融合监听、UDP 与客户端认证。

## 生命周期

- 先成功绑定融合监听器，再以管理员权限加载 WinDivert；加载失败会回收监听器并发布 `faulted`。
- Cargo 使用官方动态导入库链接，并把 `WinDivert.dll`、`WinDivert64.sys` 和 `LICENSE.WinDivert.txt` 复制到普通二进制目录及集成测试 `deps` 目录；Tauri Windows 安装包把两项运行组件与许可证文本作为根资源部署到桌面程序可执行文件同目录。
- 停止时先停止捕获并排空已接收数据包，再关闭融合监听器，避免连接尾包被错误路由。
- 后台路径同步器会在进程重启后自动提交新的进程编号；运行时流表不跨服务生命周期复用。

## 验收

- 未选中的进程保持原网络路径；选中进程的双栈 TCP 流量命中融合端口，UDP 数据报按统一滤镜决定后从原地址路径回注并进入事务录制。
- 同一远端 IP 的多个端口和并发连接均能恢复到各自原始目标。
- 延迟超过 100ms、请求头超过 16KiB 或当前 DNS 结果与原始 IP 不同的合法 GET 仍生成带域名的 HTTP 事务；TLS ClientHello 使用 SNI 生成 HTTPS 隧道身份。
- HTTP、SOCKS5、直连与二级代理路径均无递归捕获。
- 停止服务后 `trackedFlows` 归零，新连接恢复原始路径。
- 管理员 PowerShell 使用以下系统临时构建目录命令验证真实驱动加载、目标子进程重定向及停止清理；普通测试保持无特权且不会运行该用例：

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
