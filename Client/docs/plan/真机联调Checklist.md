# 真机联调清单

所有网络验收连接真实 Sprak Capture Server，不用手写 SOCKS 服务替代。测试夹具、APK、日志和
构建目录放在系统临时目录，结束后停止全部进程并删除。

## 1. 构建与安装

- [ ] `:app:testDebugUnitTest`、`:app:lintDebug`、`:app:assembleRelease` 全部通过。
- [ ] APK 同时包含 `arm64-v8a`、`armeabi-v7a` 的源码构建 `libroutesocks.so`，HEV 已静态链接且
  不存在旧预编译重复项。
- [ ] 经 `clientPackager.exe` 注入真实局域网节点、测试账号与规则 URL，签名验签通过。
- [ ] `adb install -r` 成功，清理历史应用数据后首次启动不要求输入节点、账号或密码。

## 2. 规则与应用范围

启用一份混合规则：选中测试应用的 `[RoutingRule]` 代理 `abc.com`，其余测试应用的
`[GRoutingRule]` 代理 `aaa.com`，并给两个作用域配置互不相同的 DIRECT/REJECT 边界。

- [ ] 选中应用只执行 `[RoutingRule]`，不会命中 `[GRoutingRule]`。
- [ ] 其他应用只执行 `[GRoutingRule]`，不会命中 `[RoutingRule]`。
- [ ] API 29 及以上 VPN 能用五元组归属 UID；归属失败流量被拒绝而非错误归类。
- [ ] 任意其他 UID 直连 12580/12581 且不提供本次随机凭据时，SOCKS5 协商被拒绝且 Sprak 无新连接。
- [ ] Root 的双 UID 链进入不同透明端口，停止后所有 jump 和链均被删除。
- [ ] 云规则 ETag 更新后原子切换；坏更新继续使用最后有效规则，首次坏规则明确失败。
- [ ] 规则快照截断或损坏后不发送旧 ETag，整体重新下载并一次原子提交。

## 3. TCP 与域名嗅探

- [ ] HTTP Host、单 TLS record SNI、TCP 分段 ClientHello、跨 TLS record ClientHello 均按域名命中。
- [ ] HTTP body 中伪造的 `Host:` 不参与匹配。
- [ ] 域名目标、缓存命中和 IP/PORT 规则返回真实 SOCKS 建链结果。
- [ ] VPN 数值目标缓存缺失的受限嗅探路径可以改变当前连接出口，失败以 EOF 结束且统计为失败。
- [ ] PROXY、DIRECT、REJECT、FINAL 在 VPN 与 Root 的 TCP 数据面得到相同结果。

## 4. UDP

- [ ] VPN 中 DNS 之外的 UDP 通过真实 Sprak Capture SOCKS5 UDP ASSOCIATE 收发。
- [ ] 同一目标连续请求、不同目标并发请求、无响应目标和迟到响应互不阻塞或错配。
- [ ] 支持一个请求多个响应及服务端延迟主动响应，满足 QUIC/游戏 UDP 的长期会话语义。
- [ ] 停止、热更新和模式切换会唤醒并等待 UDP 接收线程，不残留套接字。

## 5. DNS

- [ ] IPv4/IPv6 A、AAAA 查询只发送到 `[DNS]` PRIMARY，主服务器失败才使用 SECONDARY。
- [ ] TCP DNS 长度帧完整，多次查询同一连接不会截断或漏观察。
- [ ] Native 对上游节点域名、DIRECT 域名和 UDP 域名的内部解析也只使用指定 DNS。
- [ ] Sprak Capture 连接记录中没有目标端口 53；系统 DNS 抓包计数为零。
- [ ] TCP/UDP 853 被拒绝且不进入 Sprak；DoH 按普通 HTTPS 域名规则处理。
- [ ] DNS 缓存按选中/其他应用作用域隔离，共享 CDN 地址不会跨作用域污染。

## 6. 生命周期与地址族

- [ ] VPN 和 Root 均在 Native/HEV 真正就绪后发布 RUNNING，工作线程异常退出会进入 FAILED。
- [ ] VPN→Root、Root→VPN 热切换先到 STOPPED，再到目标 RUNNING；旧连接全部关闭。
- [ ] VPN 验证 IPv4/IPv6 TCP、UDP、DNS；Root 验证 IPv4 TCP/UDP/DNS 双上下文、原目标保留与 IPv6 明确拒绝。
- [ ] Root 启动后核验没有 TPROXY/策略路由对象，且捕获范围 IPv6 不能降级为直连。
- [ ] Root 核验全局/选中应用分别绑定 NFQUEUE 6100/6101，UDP verdict、REDIRECT、SOCKS5 UDP ASSOCIATE 和响应回填均有真实字节证据。
- [ ] 用户停止、系统撤销 VPN、应用强制停止后无 TUN、iptables 链、监听端口或后台线程残留。

## 7. 收尾

- [ ] 卸载临时测试应用，恢复设备网络与 DNS。
- [ ] 停止 proxyService、accountService、Gradle daemon 和所有测试辅助进程。
- [ ] 删除系统临时构建、APK、日志、PID、规则和测试数据库；仓库无 build/tmp/logs 产物。
