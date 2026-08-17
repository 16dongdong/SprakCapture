# Android SOCKS5 客户端

`Client` 是独立 Android 工程。APK 只依赖 `Contracts/clientServerContract.md` 中的公开
SOCKS5、规则下载和模板密文协议，不引用 Server 源码、数据库或构建环境。静态连接资料由
服务端按包使用独立密钥认证加密；客户端不显示或提供本地节点、凭据及应用选择入口。

## 数据面

- `libroutesocks.so` 是 VPN 与 Root 共用的 C++17 路由核心，负责规则决策、SOCKS5
  TCP/UDP、DIRECT/REJECT、指定 DNS、HTTP Host/TLS SNI 嗅探、连接统计和生命周期。
- VPN 使用静态链接进 `libroutesocks.so` 的固定版 `hev-socks5-tunnel`，把 TUN 流量送入全局或
  选中应用入口。Android 10（API 29）及以上通过原始五元组查询 UID，完整支持混合应用范围；
  API 26–28 对混合规则明确停止，只保留纯全局或纯选中应用模式。回环 SOCKS 入口每次启动
  使用独立随机凭据，HEV 配置只经匿名管道传入且不落盘，其他 Android UID 不能借用远端账号。
- Root 由 `su` 启动只加载同一个 Native SO 的特权伴随进程：IPv4 TCP 使用 `iptables nat REDIRECT`；
  UDP 先由 `mangle OUTPUT NFQUEUE` 保存原始五元组，再经 `nat OUTPUT REDIRECT` 进入 Native。配置、规则和内部认证只经匿名管道传递，
  不进入文件或命令行；Root 捕获范围的 IPv6 当前由 `ip6tables filter` 明确拒绝。
- 设置中的“证书信任”仅在 Root 可用时开启。代理通道建立后，客户端使用内置 SOCKS5 账号鉴权下载
  当前公开根证书，完成 X.509 CA、自签名和有效期校验后同步到系统信任；每五分钟复核一次以跟随服务端轮换。
- Root 伴随进程持续监听应用控制管道。用户关闭连接、划掉任务或应用进程异常退出时，服务回调与伴随
  进程 EOF 清理会共同幂等删除自有 iptables 链，避免残留规则影响设备正常网络。
- `[RoutingRule]` 只作用于 `[proxy_app]` 中的应用，`[GRoutingRule]` 只作用于其余应用。
  两个作用域可同时存在，且都执行 `PROXY`、`DIRECT`、`REJECT` 和 `FINAL`。
- 传统 DNS TCP/UDP 53 只直连 `[DNS]` 指定的 `PRIMARY`/`SECONDARY`，不经过 Sprak
  SOCKS，也不回退系统 DNS；DoT 853 被拒绝。DoH 与普通 HTTPS 无法在线路层区分，由域名
  规则决定。
- UDP 使用长期异步 relay，允许无响应请求、多响应和迟到包，不用同步请求等待阻塞同一应用。
- 规则更新和 VPN/Root 热切换均先有序关闭全部旧连接、TUN、透明链与 Native 工作线程，确认
  资源释放后再启动目标数据面。
- 规则正文与 ETag 编码在同一个有界快照中，每次更新只做一次原子替换，崩溃后不会拼出
  新正文与旧 ETag 的半提交状态。

## 工程结构

```text
Client/
├─ app/src/main/
│  ├─ assets/bootstrap/profile.bin         # 打包器覆盖的认证密文资料
│  ├─ cpp/
│  │  ├─ include/ 与 src/                 # libroutesocks.so 自有实现
│  │  └─ vendor/hev-socks5-tunnel/        # 固定上游源码、许可证与来源记录
│  ├─ java/app/proxy/client/
│  │  ├─ config/                           # 只读内置连接资料与模式偏好
│  │  ├─ routing/                          # 经 SOCKS 下载、校验和原子缓存规则
│  │  ├─ runtime/                          # Native 生命周期、热切换和流归属
│  │  ├─ service/                          # VpnService 与 Root iptables 控制
│  │  └─ ui/                               # Compose 页面、组件与主题
│  └─ java/hev/sockstun/                   # 固定 HEV JNI 桥接类名
├─ app/src/test/                           # JVM 协议和生命周期测试
├─ docs/design/架构设计说明.md
├─ docs/plan/真机联调Checklist.md
├─ testFixtures/                           # 不含秘密的规则夹具
└─ thirdParty/                              # 第三方许可证说明
```

## 构建

需要 JDK 17、Android SDK 36 与 NDK 25。Gradle 通过 `externalNativeBuild` 为
`arm64-v8a` 和 `armeabi-v7a` 生成包含 HEV 的单一 `libroutesocks.so`：

```powershell
$env:ANDROID_HOME = "D:\Android"
$env:ANDROID_NDK_HOME = "D:\Android\ndk\25.1.8937393"
./gradlew.bat :app:testDebugUnitTest :app:lintDebug :app:assembleRelease
```

发布流水线先生成预编译模板；安装后的 Sprak Capture 只调用独立
`clientPackager.exe` 做随机包名、软件名和加密连接资料替换，再执行 APK v2 签名与验签，
不需要 JDK、Gradle、Android SDK 或 NDK。

## 运行边界

- VPN 首次启动由系统显示授权对话框；Root 模式由设备 Root 管理器授权 `su`。
- 用户主动停止、系统撤销 VPN、Native/HEV 异常退出和服务销毁进入同一资源回收路径。
- 本地 SOCKS 的数值目标在域名规则存在且 DNS 缓存未命中时，需要先让 TUN 发送首个负载，
  再按 Host/SNI 决策；该受限路径的后续失败表现为 EOF。其他 CONNECT 都先真实建链再返回
  SOCKS 结果，具体边界以共享契约为准。
- 目标自身、同作用域 DNS 观察或 Host/SNI 提供域名后，代理出口始终发送 SOCKS5 DOMAIN；
  客户端只为 DIRECT 解析地址，禁止把已经恢复的域名再次降级成 IP。
- 完整真机验收必须连接真实 Sprak Capture Server，步骤见
  `docs/plan/真机联调Checklist.md`。
