# 客户端—服务端协作契约

## 目的

本文是 `Client` 与 `Server` 之间唯一的共享工程边界。两个项目不得直接引用对方的源码、内部类型、数据库结构或构建目录；运行时协作只使用本文规定的线协议和预编译模板协议。

## 依赖方向

```text
Client APK -- SOCKS5 TCP/UDP --> Server 融合代理端点
Client APK -- 经上述 SOCKS5 的 HTTP --> Server 当前启用规则集
Client 预编译 APK -- 固定槽位协议 --> Server/clientPackager.exe
```

`Client` 只依赖服务端公开协议。`Server` 发布流程依赖一份已编译且经过模板规范化的 Client APK，不依赖 Client 源码、Gradle、JDK 或 Android SDK；安装后的运行环境只调用独立 `clientPackager.exe`。

## SOCKS5 线协议

- 基础协议：RFC 1928 SOCKS5。
- 用户名密码认证：RFC 1929。
- TCP：必须支持 `CONNECT`。
- UDP：VPN 模式必须支持 `UDP ASSOCIATE`，并保持标准 SOCKS5 UDP 报文头、地址类型和端口字节序。
- 地址类型：IPv4、IPv6和域名均按 RFC 1928 编码。
- 认证失败、命令不支持和目标连接失败必须返回标准 SOCKS5 响应，不得用 HTTP 状态或私有帧替代。
- VPN 的数值目标在“对应作用域存在域名规则且 DNS 域名缓存没有唯一可信命中”时，需要先让 TUN
  侧继续发送首个应用数据包，再根据 HTTP Host 或 TLS SNI 选择出口。该受限路径会先返回
  SOCKS 成功；后续嗅探或建链失败通过 EOF 结束。除此之外的 CONNECT 必须先完成真实建链，
  再返回与结果一致的 SOCKS 响应，禁止扩大提前成功范围。
- 目标原本携带域名，或客户端通过同作用域 DNS 观察、HTTP Host、TLS SNI 恢复出域名时，
  `PROXY` 出口必须原样编码为 SOCKS5 DOMAIN。代理路径禁止在客户端重新解析为数值 IP；
  只有 `DIRECT` 出口允许使用 `[DNS]` 指定服务器解析。服务端事务因此必须保留可观察到的
  原始域名身份，不能因 HEV 传入数值目标而退化成 IP。

## 认证与规则语义

- 每个生成的 APK 在认证密文中携带当前融合 SOCKS5 节点、该次下载提交的 SOCKS5 账号密码和绝对规则 URL；
  UI、DEX、资源字符串、日志及磁盘临时文件不得出现这些字段的明文或 Base64 表示。
- 用户端不提供节点、账号、密码或应用选择输入；所有代理应用与路由正文都来自服务端唯一启用规则集。
- 账号无固定密码时，下载页面仍要求用户输入任意非空密码；客户端始终发送合法 RFC 1929 认证报文。
- 管理员凭据、管理会话和自动化 API Key 不进入 APK。
- VPN 的 HEV 与统一 Native 核心之间使用每次数据面启动重新生成的 192 位随机 RFC 1929
  内部凭据；该凭据不得复用远端 SOCKS5 账号。HEV 配置通过匿名管道传入，不写文件系统，停机后只从
  进程内存释放。两个回环 SOCKS5 入口都必须强制认证，避免其他本机应用借用 APK 内置账号。
- 客户端只允许通过内置 SOCKS5 节点请求 `client-rules.internal.invalid` 规则 URL，HTTP Basic 与 SOCKS5 使用同一组内置凭据；该保留域名仅由服务端进程内覆盖解析到本机账号服务，禁止系统 DNS、NAT 公网回流或失败后直连规则服务。
- 规则响应使用 UTF-8 `text/plain`，携带 `ETag`、`X-Rule-Set-Id` 和 `X-Rule-Set-Revision`；客户端用 `If-None-Match` 获取 `304`，并只原子提交完整验证通过的新正文。
- 规则正文必须包含且不得重复 `[DNS]`、`[RoutingRule]`、`[GRoutingRule]`、
  `[proxy_app]`。段名 `[proxy app]` 仅用于数据库升级识别，新建和下发一律使用
  `[proxy_app]`。
- `[RoutingRule]` 只作用于 `[proxy_app]` 中的应用；`[GRoutingRule]` 只作用于其余应用。
  两种作用域允许同时存在且互不继承。例如选中应用可仅代理 `abc.com`，其余应用可仅代理
  `aaa.com`。VPN 和 Root 必须保留每条流的作用域身份后再执行相同的
  `PROXY`、`DIRECT`、`REJECT`、`FINAL` 语义。
- VPN 混合模式使用原始五元组查询连接 UID；Android API 29 及以上支持该能力。API 26–28
  只能执行纯全局或纯选中应用规则，混合规则必须明确阻止启动，禁止把未知 UID 归到任一作用域。
- `[DNS]` 必须包含唯一 `PRIMARY,<IPv4或IPv6>`，可包含唯一
  `SECONDARY,<IPv4或IPv6>`。所有传统 DNS TCP/UDP 53 查询和 Native 内部域名解析只直连
  这些服务器，不通过 Sprak SOCKS，也不回退系统 DNS。DoT 853 明确拒绝，避免被当作普通代理
  流量；DoH 与普通 HTTPS 在线路上不可区分，由域名规则控制。
- 首次启动没有有效规则时必须明确失败；后续更新失败保留最后一个已验证版本。Root 与 VPN 切换必须先完整停止当前数据面和所有连接，再启动目标模式。

## 预编译模板协议

Client 发布模板只包含空的 `assets/bootstrap/profile.bin`。每次下载由独立打包器生成随机 32 字节密钥
和 24 字节 nonce，以 XChaCha20-Poly1305 认证加密节点、端口、账号、密码和规则 URL。容器头固定为：

```text
magic="SPRKPF01" | version=1 | algorithm=1 | reserved=0 | nonce[24] |
plaintextLength:u32be | ciphertext[plaintextLength] | tag[16]
```

完整 40 字节头作为 AAD。明文为 `version:u8`，随后依次是 `host`、`port:u16be`、`username`、
`password`、`rulesUrl`；字符串均使用 `length:u16be + UTF-8 bytes`。密钥只补入两个 ABI
`libroutesocks.so` 各自唯一的 `.sprk_profile_key` 零槽，模板零槽非零、标记缺失或重复都必须拒绝。
Android 只允许通过 JNI 在内存中解密，解析后立即覆盖密文、明文和字段缓冲区。

模板还必须包含固定长度的应用标识和软件名槽位。每个支持的 ABI 只能包含统一业务核心
`lib/<abi>/libroutesocks.so`；HEV 及其依赖必须静态链接进该文件，不得携带第二个业务 SO、旧可执行文件或
运行期 `dlopen` 路径。发布阶段先执行：

```text
clientPackager.exe prepare-template --source SOURCE_APK --output CLIENT_TEMPLATE
```

运行时主服务通过命令行传递模板、输出和签名目录；包名、软件名、图标、账号、密码、节点与规则 URL
只通过有界标准输入 JSON 传递。包名留空时生成三个随机小写英文段，每段 3–6 个字母；软件名留空时
生成 3–6 个英文字母且不得全大写；
图标留空时保留模板图标。打包器必须重写 AXML/ARSC/PNG、生成认证密文、补入随机 Native 密钥、
执行 APK v2 签名和独立验签，标准输出只返回长度与 SHA-256。发布前必须同时扫描完整 APK 与每个解压
ZIP 条目：节点、端口、规则 URL、标准 Base64、完整带长度前缀的资料明文及具有辨识度的凭据原文均不得
出现；短账号或密码不得使用容易与资源表、版本号自然碰撞的裸子串判定，但仍必须检查其 Base64 和完整
资料明文。每个 ABI 只能有一个 `libroutesocks.so`。

## Server 下载接口

| 方法 | 路径 | 语义 |
| --- | --- | --- |
| `GET` | `/client` | 无需管理员登录的客户端下载页面 |
| `POST` | `/api/v1/clientPackages/download` | 使用 `{username,password}` 做无租约账号校验，同步生成并流式下载一次性 APK |
| `GET` | `/api/v1/clientPackages` | 当前任务与最近 10 条脱敏生成记录，不提供历史下载能力 |
| `GET` | `/api/v1/client/routing.txt` | 使用 HTTP Basic 下载当前唯一启用规则集，支持 ETag |
| `GET` | `/api/v1/client/ca.cer` | 使用同一 HTTP Basic 凭据下载当前公开根证书，禁止缓存 |

每次下载必须生成新的完整 `applicationId` 和随机软件名。固定部署可通过 `CAPTURE_CLIENT_PUBLIC_HOST`
声明公网 IP；显式公网监听直接复用该地址，通配或回环监听通过 HTTPS 查询当前公网 IPv4。任何私网、
回环、链路本地、文档或保留地址都不得写入发布 APK；解析失败必须明确终止。局域网端到端夹具只能在
调试构建的隔离测试进程中显式设置 `CAPTURE_CLIENT_TEST_HOST`，发布构建不含该边界。

完整凭据只允许存在于当次请求、打包器匿名管道和当次临时 APK。任务快照、生成记录、日志、错误、MCP 结果和元数据不得包含凭据或规则 URL；响应完成、断流、失败或进程重启都必须删除残留 APK，历史记录不提供下载 URL。

客户端“证书信任”只在 Root 可用时开启。数据面建立后，客户端经自身 SOCKS5 通道访问
`/api/v1/client/ca.cer`，服务端先校验账号未禁用、未过期且密码正确，再从回环控制端导出当前 DER。
客户端必须验证证书当前有效、自签名且具备 CA 能力，随后以 Root 模块同步 Android 系统信任；服务端
轮换 CA 后客户端在下一次五分钟同步周期更新。私钥、控制地址和证书本机路径不得跨越本接口。

## 兼容性规则

- 只增加服务端内部实现或管理 UI 不改变本契约。
- 修改模板槽位、认证报文字段、空密码标记、规则格式、地址编码、UDP 封装或错误映射时，必须同时修改两个项目并完成端到端回归。
- 新协议能力先以向后兼容方式加入服务端，再由客户端启用；删除旧能力前必须完成已发布客户端的迁移。
- 端到端验收必须使用真实 Sprak Capture Server，不用手写替代服务器证明兼容性。

## 独立构建验收

```powershell
# 服务端：临时目录固定放在仓库 D 盘 tmp/ 下，测试后 cargo clean 并删除任务目录
cd Server
cargo check --workspace

# 客户端：Gradle/NDK 临时构建目录同样放在仓库 D 盘 tmp/ 下，测试后删除
cd ..\Client
.\gradlew.bat :app:testDebugUnitTest :app:lintDebug :app:assembleDebug
```

两组命令只能读取各自项目目录；发布流水线只把已编译 APK 交给模板准备命令，端到端运行测试才允许通过本文线协议连接另一个项目。
