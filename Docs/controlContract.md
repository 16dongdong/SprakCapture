# 前后端控制契约

## 插件控制

插件包从 `{dataDirectory}/plugins/` 发现并由统一宿主管理。当前端点覆盖包、配置、完整 Native Mod、Sidecar、Native Worker 和 legacy 运行时生命周期；后续运行时继续复用本节路由族扩展订阅顺序、诊断、回滚和命令，不建立能力授权或平行控制服务。

| 方法 | 路径 | 请求 | 成功响应 | 错误 |
|---|---|---|---|---|
| `GET` | `/api/v1/plugins` | 无 | `PluginSnapshot[]` | 无 |
| `POST` | `/api/v1/plugins/packages` | `.tplugin.zip` 二进制 | `201 PluginSnapshot` | `error.pluginOperationFailed` |
| `GET` | `/api/v1/plugins/{pluginId}` | 无 | `PluginDetails` | `error.pluginNotFound` |
| `DELETE` | `/api/v1/plugins/{pluginId}` | 无 | `204` | `error.pluginNotFound`、`error.pluginOperationFailed` |
| `PUT` | `/api/v1/plugins/{pluginId}/enabled` | `{ "enabled": boolean }` | `PluginSnapshot` | `error.pluginNotFound`、`error.pluginOperationFailed` |
| `PUT` | `/api/v1/plugins/{pluginId}/configuration` | `{ "configuration": object }` | `PluginDetails` | `error.pluginNotFound`、`error.pluginOperationFailed` |
| `POST` | `/api/v1/plugins/{pluginId}/reload` | 无 | `PluginSnapshot` | `error.pluginNotFound`、`error.pluginOperationFailed` |

`PluginSnapshot` 使用 camelCase 字段：`id`、`name`、`version`、`apiVersion`、`runtime`、`hooks`、`enabled`、`state`、`activeConnections` 和可选 `errorCode`。`PluginDetails` 另含 `configSchema`、脱敏后的 `configuration` 与 `configuredSecretFields`。`state` 为 `disabled`、`enabled`、`failed` 或 `incompatible`。

完整阶段、动作、能力和模块 API 见 [pluginHookApi.md](pluginHookApi.md)。当前 legacy Native 兼容范围见 [pluginSystemAudit.md](pluginSystemAudit.md)。

## Android 客户端生成

客户端记录查询和 APK 下载严格分离。节点来自当前融合 SOCKS5 监听配置；公开 `/client` 页面提交
SOCKS5 账号和密码后，服务端先向账号数据库执行无租约认证，再通过独立打包器同步生成并流式返回
本次随机 APK。请求可选提交 `applicationId`、`applicationName` 和不超过 1 MiB 的 PNG/JPEG/WebP
`iconBase64`；空值使用随机 3–6 字母身份和模板图标。APK 仅在认证密文中携带节点、SOCKS5 凭据和
规则集地址，不包含管理员身份或自动化 API Key，也不在 UI、DEX、资源字符串或日志中显示连接资料。
规则地址固定使用 `client-rules.internal.invalid`；SOCKS5 数据面把该保留域名直接映射到本机账号服务，
因此不依赖部署网络支持公网 NAT 回流，也不会让客户端绕过已认证代理直连管理端口。

| 方法 | 路径 | 成功响应 | 错误 |
|---|---|---|---|
| `GET` | `/api/v1/clientPackages` | `ClientPackageSnapshot` | 无 |
| `POST` | `/api/v1/clientPackages/download` | 已签名 APK | `error.clientPackageAuthenticationFailed`、`error.clientPackageBusy`、`error.clientNodeUnavailable`、`error.clientPackageServiceUnavailable`、`error.clientPackageOperationFailed` |
| `GET` | `/client` | 公开下载页面 | 无 |
| `GET` | `/api/v1/client/routing.txt` | 当前唯一启用规则集正文 | 未认证、无启用规则集 |
| `GET` | `/api/v1/client/ca.cer` | 当前公开根证书 DER | SOCKS5 账号无效、禁用或过期 |

`POST /api/v1/clientPackages/download` 的 JSON 正文为
`{ "username": string, "password": string, "applicationId"?: string, "applicationName"?: string, "iconBase64"?: string }`。
账号和密码均须非空；数据库中未设置固定密码的账号也必须为本次 APK 提交任意非空密码。
自定义包名只接受规范小写 Android applicationId，自定义软件名为 1–32 个无首尾空白的非控制字符；
空字符串按未填写处理，非空值不得静默裁剪。
账号验证不创建 SOCKS5 连接租约；响应结束或断流后不保留可再次下载的带凭据产物。
`ClientPackageSnapshot` 最多包含最近 10 条生成记录；每条记录公开唯一包名、随机软件名、节点、
字节数和 SHA-256，不公开账号、密码、规则 URL、文件系统路径、签名材料或下载 URL。构建状态为
`preparing`、`building`、`verifying`、`ready` 或 `failed`，失败原因只保留有界且脱敏的诊断。

写入 APK 的节点必须是设备可达的外部地址。固定部署可通过 `CAPTURE_CLIENT_PUBLIC_HOST` 提供公网 IP；
显式公网监听直接复用该地址，通配或回环监听则通过 HTTPS 查询当前公网 IPv4。私网、回环、链路本地、
文档和保留地址一律拒绝，公网地址解析失败时终止生成，不把局域网接口伪装成公网节点。仅调试构建提供
`CAPTURE_CLIENT_TEST_HOST` 供隔离局域网端到端验收，发布构建不接受该覆盖值。

预编译模板的 `assets/bootstrap/profile.bin` 必须为空，双 ABI `libroutesocks.so` 各含唯一 32 字节零密钥槽。
打包器为每个 APK 生成随机 XChaCha20-Poly1305 密钥和 nonce，把节点、凭据和规则 URL 写为认证密文，
密钥仅补入 Native 固定槽。HEV 内部随机凭据通过匿名管道传递，不落盘。签名前后必须校验双 ABI 单 SO，
并扫描完整 APK 与每个解压条目，拒绝任何连接资料明文或 Base64 残留。

规则集由账号管理页面维护，创建、编辑、删除、批量删除和启用操作使用 `/api/v1/ruleSets` 路由族。
服务端事务保证最多一个规则集启用。客户端以 SOCKS5 Basic 凭据请求
`GET /api/v1/client/routing.txt`；响应包含 `ETag`、`X-Rule-Set-Id` 与
`X-Rule-Set-Revision`，匹配 `If-None-Match` 时返回 `304`。

客户端证书信任不复用无认证的控制导出入口。Android 在代理通道启动后使用相同 SOCKS5 账号通过
HTTP Basic 请求 `GET /api/v1/client/ca.cer`；账号服务只在凭据有效时从回环控制 API 读取当前 DER，
并返回 `Cache-Control: private, no-store`。证书私钥始终保留在控制服务证书目录。

`routing.txt` 必须同时包含且不得重复 `[DNS]`、`[RoutingRule]`、`[GRoutingRule]` 和
`[proxy_app]` 四个段。`[DNS]` 段的线上格式为：

```text
[DNS]
PRIMARY,223.5.5.5
SECONDARY,1.1.1.1

[RoutingRule]

[GRoutingRule]
FINAL,PROXY

[proxy_app]
```

`PRIMARY` 必需且只允许一条，`SECONDARY` 可选且最多一条；值必须是
IPv4 或 IPv6 字面量，不接受主机名、重复键或未知键。客户端对传统 TCP/UDP 53 查询和
Native 内部域名解析统一使用指定 DNS 直连，不通过 SOCKS5 节点，也不隐式回退到系统 DNS；
DoT 853 明确拒绝，DoH 按普通 HTTPS 域名规则处理。新建规则
默认使用 `223.5.5.5`、备用 `1.1.1.1` 和 `FINAL,PROXY`；VPN 与 Root 数据面执行同一份
路由规则和 DNS 策略。

规则允许混合应用范围：`[RoutingRule]` 只作用于 `[proxy_app]` 列出的应用，
`[GRoutingRule]` 只作用于其他应用。例如可让 APP A 对 `abc.com` 使用代理，同时让
其他应用仅对 `aaa.com` 使用代理。客户端必须在 TUN/透明入口保留应用身份，禁止把
普通规则误用于其他应用。

账号数据库 v3 迁移会在单一 SQLite 事务内为缺少 `[DNS]` 的 v2 规则前置上述
默认 DNS，并把旧 `[proxy app]` 段改为 `[proxy_app]`；改写会递增 `revision` 和
`updatedAt` 使旧 ETag 失效。已经使用当前格式的正文不改写。迁移后每条规则都使用
与管理端保存相同的 validator 复验，任一旧正文仍无效时整个迁移回滚并阻止服务启动。

## 1. 边界与版本

控制服务默认绑定 `127.0.0.1:17890`。它只负责本机 HTTP/JSON、SSE 与 WebSocket 控制，
不构成代理入口。HTTP 正向代理与 SOCKS5 在 `configuration.listenHost`、
`configuration.listenPort` 指定的同一 TCP 端口按首包识别协议；默认端点为
`127.0.0.1:1080`。SOCKS5 支持 `CONNECT`、`BIND` 和 `UDP ASSOCIATE`。公开客户端只连接
该融合端口；WinDivert 使用服务启动时分配的同地址族随机回环入口，该内部端点不进入控制契约。

M2 在既有事务契约上增加独立 SSL 配置和公开根证书生命周期。界面草稿、对话框状态和
下载对象 URL 仍属于前端，不进入后台权威快照。

所有 JSON 字段使用 `camelCase`。请求模型拒绝未知字段和缺失的必填字段。
公共响应不包含认证口令、正文 spill 路径或其他本机存储句柄。

## 2. 传输与来源校验

浏览器请求只接受以下精确 Origin：

```text
http://127.0.0.1:5173
http://localhost:5173
http://tauri.localhost
```

HTTP、SSE 与 WebSocket 升级共用该校验。未携带 Origin 的本机 CLI/MCP 请求允许访问；
重复、无效或其他 Origin 返回 `403`。跨域方法只开放 `GET`、`POST`、`PUT`、`DELETE`，
请求头只开放 `Content-Type` 与 `Accept-Language`。

全部控制 API 响应均携带 `Cache-Control: no-store`，避免浏览器缓存已经变更的配置、
监听状态，以及被用户显式 clear 的事务内容。

## 3. HTTP 路由

| 方法 | 路径 | 成功响应 | 作用 |
|---|---|---|---|
| `GET` | `/api/v1/health` | `HealthResponse` | 探测本地控制服务存活，不修改 revision |
| `GET` | `/api/v1/version` | `VersionResponse` | 返回后端协议版本 |
| `GET` | `/api/v1/snapshot` | `ControlSnapshot` | 获取完整权威快照 |
| `GET` / `PUT` | `/api/v1/ui/context` | `UiContextSnapshot` | 读取或续期当前页面、页签、焦点和稳定资源选择 |
| `POST` | `/api/v1/service/start` | `ControlSnapshot` | 启动 HTTP/SOCKS5 融合监听及已启用的进程捕获 |
| `POST` | `/api/v1/service/stop` | `ControlSnapshot` | 停止进程捕获并排空融合数据面 |
| `PUT` | `/api/v1/configuration` | `ControlSnapshot` | 运行中先强制断开全部代理连接，再以新配置重启数据面；停止态只替换配置 |
| `GET` | `/api/v1/processes` | `ProcessSelectionSnapshot` | 返回实时进程清单、已保存路径和当前解析出的 PID |
| `PUT` | `/api/v1/processes` | `ProcessSelectionSnapshot` | 按可执行路径替换捕获选择并在运行中重启数据面 |
| `DELETE` | `/api/v1/sessions` | `ControlSnapshot` | 只清除已结束的 SOCKS5 会话 |
| `GET` | `/api/v1/recording` | `RecordingResponse` | 获取当前录制状态 |
| `PUT` | `/api/v1/recording` | `RecordingResponse` | 部分更新录制状态、限额和忽略规则 |
| `POST` | `/api/v1/recording/clear` | `RecordingResponse` | 清空当前录制会话的事务、头和正文 |
| `GET` | `/api/v1/ssl` | `SslPublicState` | 获取 SSL 主机规则、公开 CA 元数据与握手计数 |
| `PUT` | `/api/v1/ssl` | `SslPublicState` | 原子替换完整 SSL 配置 |
| `POST` | `/api/v1/ssl/ca/generate` | `SslPublicState` | 更换根 CA 并清空叶证书缓存 |
| `GET` | `/api/v1/ssl/ca/export?format=pem\|cer` | PEM 或 DER | 导出公开根证书 |
| `GET` | `/api/v1/transactions` | `TransactionPage` | 获取有界事务摘要页 |
| `GET` | `/api/v1/transactions/{transactionId}` | `TransactionDetail` | 获取摘要、请求头、响应头和正文元信息 |
| `GET` | `/api/v1/transactions/{transactionId}/request/body` | `EncodedBodyResponse` | 按需读取请求正文 |
| `GET` | `/api/v1/transactions/{transactionId}/response/body` | `EncodedBodyResponse` | 按需读取响应正文 |
| `POST` | `/api/v1/compose` | `ComposeResult` | 异步发送编辑后的 HTTP 请求，并创建新事务 |
| `POST` | `/api/v1/transactions/{transactionId}/repeat` | `ComposeResult` | 从只读事务派生原样或带覆盖字段的重复请求 |
| `GET` / `POST` | `/api/v1/loadTests` | `AdvancedRepeatJob` / `AdvancedRepeatJob[]` | 列出或以明确确认启动有界高级重复作业 |
| `GET` | `/api/v1/loadTests/{jobId}` | `AdvancedRepeatJob` | 查询高级重复进度和延迟统计 |
| `POST` | `/api/v1/loadTests/{jobId}/cancel` | `AdvancedRepeatJob` | 协作式取消高级重复，停止新迭代调度 |
| `GET` / `PUT` | `/api/v1/tools/protobuf` | `ProtobufConfiguration` | 读取或替换解码开关与路由 |
| `POST` | `/api/v1/tools/protobuf/schemas` | `ProtobufConfiguration` | 上传并登记 FileDescriptorSet |
| `GET` | `/api/v1/transactions/{transactionId}/decode/protobuf?side=request\|response` | `DecodedProtobufView` | 按描述符解码录制正文，失败以 `decodeError` 返回 |
| `GET` / `PUT` | `/api/v1/tools/validate` | `ValidateConfiguration` | 读取或替换响应校验配置 |
| `GET` / `PUT` | `/api/v1/tools/packetFilters` | `PacketFilterConfiguration` | 读取或热替换最终写线封包滤镜 |
| `POST` | `/api/v1/transactions/{transactionId}/validate` | `ValidationReport` | 校验响应正文；在线校验需本次请求确认上传 |
| `GET` | `/api/v1/transactions/{transactionId}/validation` | `ValidationReport[]` | 读取按需校验报告 |
| `GET` | `/api/v1/events` | WebSocket | 订阅初始快照和后续增量事件 |
| `GET` | `/api/v1/events/sse` | Server-Sent Events | 浏览器订阅同一初始快照和后续增量事件 |

正文侧使用两个固定路径，不接受旧式 `?side=request|response` 查询参数。

## 4. 完整快照

`ControlSnapshot` 的顶层结构为：

```json
{
  "serverInstanceId": "00000000-0000-4000-8000-000000000001",
  "revision": 42,
  "serviceState": "running",
  "metrics": {},
  "sessions": [],
  "configuration": {},
  "listeners": {},
  "ssl": {},
  "recording": {},
  "transactions": {},
  "advancedRepeats": [],
  "plugins": []
}
```

### 4.1 服务状态

`serviceState` 取值为：

```text
stopped
starting
running
stopping
faulted
```

`starting` 与 `stopping` 是真实状态，过渡状态下拒绝重复启停。`start` 会启动唯一的
HTTP/SOCKS5 融合监听，再按配置启动 WinDivert 进程捕获。反向代理或端口转发等任一数据面
监听成功时整体状态为 `running`，各组件的启动错误仍由对应快照独立公开；所有数据面监听
均失败时才进入 `faulted`。因此统一状态不能替代组件级状态。

`listeners` 的结构为：

```json
{
  "socks5": {
    "enabled": true,
    "state": "running",
    "boundEndpoint": "127.0.0.1:1080",
    "error": null
  },
  "httpProxy": {
    "enabled": true,
    "state": "failed",
    "boundEndpoint": null,
    "error": {
      "code": "httpProxyStartFailed",
      "messageKey": "error.httpProxyListenerFailed",
      "params": {}
    }
  }
}
```

`listeners.socks5` 与 `listeners.httpProxy` 是为兼容现有控制客户端保留的协议视图，二者指向
同一个绑定端点和生命周期，不表示存在两个 TCP 监听器。`state` 取值为 `disabled`、
`stopped`、`running`、`failed`；`error` 是唯一的监听失败契约。WinDivert 状态由顶层
`processCapture` 快照独立公开。

### 4.2 SOCKS5 会话与指标

`sessions` 仍是 SOCKS5 连接生命周期，不是事务列表的数据源。控制面会把命令、真实目标、
绝对流量计数和终态原生写入 RecordingSession，生成 `protocol=socks` 的事务；Web 只消费
统一事务模型，不建立旧会话 UI 兼容层。每个会话包含：

```text
sessionId, clientAddress, username, command, targetAddress, state,
bytesUp, bytesDown, createdAtMilliseconds, updatedAtMilliseconds,
closedAtMilliseconds, errorMessage
```

`metrics` 仍是 SOCKS5 数据面累计指标：

```text
acceptedConnections, activeConnections, failedConnections, bytesUp, bytesDown,
udpPacketsUp, udpPacketsDown, droppedUdpPackets
```

服务停止或重新启动不会清除已结束会话与累计指标。`DELETE /api/v1/sessions` 只清除
已结束会话；活动会话继续保留，累计指标不变。

顶层 `processCapture` 独立公开当前 `trackedFlows`、`acceptedConnections`、
`redirectedPackets`、`restoredPackets`、`bytesUp`、`bytesDown` 和已解析 PID；运行期间
每秒发布一次实时事件。工作台把其中连接数和字节数并入现有六个服务指标，但数据包计数
只保留在快照诊断中，不能冒充连接或会话。

### 4.3 公共配置

`configuration` 返回融合监听、二级代理与 WinDivert 进程捕获的完整公开配置：

```json
{
  "listenHost": "127.0.0.1",
  "listenPort": 1080,
  "authenticationMode": "none",
  "authenticationUsernames": [],
  "maxConnections": 1024,
  "connectTimeout": 10.0,
  "bindTimeout": 30.0,
  "idleTimeout": 300.0,
  "shutdownTimeout": 5.0,
  "readTimeout": 10.0,
  "relayBufferSize": 65536,
  "udpBindHost": "",
  "udpMaxPacketSize": 65507,
  "httpProxy": {
    "enabled": true,
    "listenHost": "127.0.0.1",
    "listenPort": 1080,
    "maxConnections": 512,
    "maxHeaderBytes": 65536,
    "maxCaptureBodyBytes": 262144,
    "connectTimeoutMilliseconds": 10000,
    "requestTimeoutMilliseconds": 60000,
    "headerReadTimeoutMilliseconds": 15000,
    "shutdownTimeoutMilliseconds": 5000
  },
  "upstreamProxy": {
    "enabled": false,
    "protocol": "socks5",
    "host": "127.0.0.1",
    "port": 1081,
    "username": "",
    "hasPassword": false
  },
  "processCapture": {
    "enabled": false,
    "processIds": [],
    "proxyPort": 1080
  }
}
```

`PUT /api/v1/configuration` 使用相同的监听字段，但以 `credentials` 替代
`authenticationUsernames`。`credentials` 是必填且可为 `null` 的字段；密码模式可用
`null` 保留现有凭据。`httpProxy`、`upstreamProxy` 与 `processCapture` 始终必填且必须
提交完整对象。二级代理请求中的 `password=null` 保留已有口令，空字符串明确清除；
公共快照只通过 `hasPassword` 表示是否已保存口令。`processCapture.proxyPort` 的公开值必须等于
融合监听端口，运行时会改用服务分配的内部回环端点以隔离显式客户端四元组；进程编号为去重的正整数数组。

运行中更新配置会先断开现有连接、停止 WinDivert 捕获与融合监听，再用新配置重启；
任一步失败均通过监听状态和结构化错误公开。

### 4.5 SSL 配置与公开证书状态

`ssl` 与 `GET /api/v1/ssl` 使用同一结构：

```json
{
  "enabled": true,
  "includeLocations": [
    {
      "protocol": "https",
      "host": "*.example.com",
      "port": "",
      "path": "",
      "query": null
    }
  ],
  "excludeLocations": [],
  "maxCachedCertificates": 256,
  "useClientSni": true,
  "ca": {
    "installed": true,
    "subject": "CN=Sprak Capture Local Root CA",
    "validFromMilliseconds": 1720000000000,
    "validToMilliseconds": 2035360000000,
    "fingerprintSha256": "SHA256_FINGERPRINT",
    "pemPath": "USER_DATA/certs/rootCA.pem"
  },
  "cachedLeafCount": 0,
  "handshakeSuccessTotal": 0,
  "handshakeFailureTotal": 0,
  "clientCertificates": [],
  "supportedHttpVersions": ["HTTP/1.0", "HTTP/1.1", "HTTP/2"]
}
```

`PUT /api/v1/ssl` 只提交前五个配置字段，拒绝未知字段。`excludeLocations` 始终优先；
`enabled=false` 或空 `includeLocations` 都不会解密 CONNECT。匹配项使用客户端 SNI
或 CONNECT 主机签发 SAN 叶证书。下游与上游通过 ALPN 协商 HTTP/2 或 HTTP/1.1，
明文连接同时接受 HTTP/1.0；上游 TLS 使用系统信任根并验证主机。未匹配项仍按裸隧道转发。

`POST /api/v1/ssl/client-certificates` 以 multipart 导入 PKCS#12/PFX、PEM 或 DER 客户端
身份，`PUT/DELETE /api/v1/ssl/client-certificates/{id}` 更新规则或删除材料。每条身份必须
配置 HTTPS Location；命中目标请求客户端证书时才附加该身份。控制响应和事件只返回证书
主题、签发者、有效期、指纹与规则，容器口令和私钥原文不进入快照或日志。

公开结构、API、事件和日志都不含根私钥。`format=pem` 返回
`application/x-pem-file`，`format=cer` 返回 `application/pkix-cert`。更换根 CA 后
指纹变化、叶证书缓存清空，已信任旧根的客户端必须重新导入新根。

## 5. 录制契约

`GET /api/v1/recording`、`PUT /api/v1/recording` 和
`POST /api/v1/recording/clear` 都返回：

```json
{
  "serverInstanceId": "00000000-0000-4000-8000-000000000001",
  "revision": 43,
  "recording": {
    "recordingSessionId": "SESSION_ID",
    "state": "recording",
    "startedAtMilliseconds": 0,
    "transactionCount": 0,
    "droppedCount": 0,
    "totalBodyBytes": 0,
    "totalMetadataBytes": 0,
    "metadataMemoryBudgetBytes": 9007199254740991,
    "pendingCleanupCount": 0,
    "limits": {
      "maxTransactions": 9007199254740991,
      "maxBodyBytes": 9007199254740991,
      "maxTotalBodyBytes": 9007199254740991
    },
    "ignoreLocations": [],
    "recordTunnelMetadata": true
  }
}
```

`state` 为 `recording` 或 `paused`。暂停只阻止新事务进入录制层，不停止数据面。
`PUT` 是部分更新，允许字段如下：

```json
{
  "state": "paused",
  "ignoreLocations": [
    {
      "protocol": "http",
      "host": "*.example.test",
      "port": "*",
      "path": "/health",
      "query": null
    }
  ],
  "recordTunnelMetadata": true
}
```

`limits` 内部也是部分更新。`ignoreLocations` 的字段为 `protocol`、`host`、`port`、
`path`、`query`；空字符串按 Location 规则表示通配。设置更新先整体校验，再在同一
录制写锁内提交。

`clear` 会清除事务元数据、两侧头和正文引用，保留当前 `recordingSessionId` 以及累计
`droppedCount`。它与 `DELETE /api/v1/sessions` 相互独立。

## 6. 事务契约

### 6.1 摘要分页

`GET /api/v1/transactions` 接受可选 `offset`、`limit`、`collectionToken`：

- 默认 `limit=200`；允许 `1..=1000`。
- 未传 `offset` 时选择最新一页，但 `items` 始终按 `sequence` 升序。
- `offset` 大于 `total`、`limit=0` 或超过上限返回 `400`。
- 完整正向遍历必须从 `offset=0` 开始。若响应的 `nextOffset` 非空，下一次请求只使用该
  值作为 `offset`；禁止用当前 `offset + 请求 limit` 推算，因为序列化预算可能令本页
  实际返回数少于请求上限。`nextOffset=null` 表示遍历结束。
- 第一页省略 `collectionToken`，后续页原样携带响应令牌；录制期间集合成员或顺序变化时
  旧令牌返回 `409 transactionsCollectionChanged`，调用方必须丢弃旧页并重新读取第一页。
- 快照和 `transactions` 事件至多读取最近 500 条摘要；序列化集合预算为 4 MiB，
  达到预算时可返回更少记录，并通过分页字段明确是否截断。

响应结构为：

```json
{
  "revision": 44,
  "recordingSessionId": "SESSION_ID",
  "collectionToken": "SESSION_ID:7",
  "total": 1250,
  "offset": 1050,
  "limit": 200,
  "hasPrevious": true,
  "hasMore": false,
  "nextOffset": null,
  "truncated": true,
  "itemsTruncated": false,
  "items": []
}
```

`hasMore=true` 时 `nextOffset` 必须是下一页的权威起点；`hasMore=false` 时
`nextOffset` 必须为 `null`。`truncated` 表示当前集合未覆盖全部事务或摘要字段受预算限制；
`itemsTruncated` 专门表示至少一条摘要的自由文本字段被有界投影缩短。
`flags.headersTruncated` 表示详情中的请求头或响应头受持久元数据预算裁剪；正文是否
截断仍以详情或正文响应中的 `meta.truncated` 为准。

`items` 每项只包含摘要字段：

```text
transactionId, recordingSessionId, sequence, protocol, method, host, port,
path, query, urlDisplay, status, statusCode, clientAddress,
clientProcessName, clientProcessId, contentType, timings, sizes, flags,
error, notes, tags, appliedTools
```

枚举值：

- `protocol`：`http`、`https`、`ws`、`wss`、`tunnel`、`socks`
- `status`：`pending`、`complete`、`failed`、`blocked`、`cancelled`

`timings` 保存 `startAtMilliseconds`、`dnsEndAtMilliseconds`、
`connectEndAtMilliseconds`、`tlsEndAtMilliseconds`、
`requestSentAtMilliseconds`、`responseStartAtMilliseconds`、
`endAtMilliseconds`。`sizes` 保存线上完整的请求/响应头体字节数。摘要不包含头字段和
正文实际字节。

### 6.2 详情

`GET /api/v1/transactions/{transactionId}` 返回：

```json
{
  "revision": 45,
  "transaction": {},
  "requestHeaders": [{"name": "content-type", "value": "text/plain"}],
  "responseHeaders": [],
  "requestBody": {
    "transactionId": "TRANSACTION_ID",
    "side": "request",
    "contentType": "text/plain",
    "encoding": "identity",
    "storedBytes": 4,
    "originalBytes": 4,
    "truncated": false
  },
  "responseBody": null
}
```

头字段使用有序数组保留重复字段及原始顺序。正文元信息不包含存储路径。事务不存在时
返回 `404 transactionNotFound`。

### 6.3 正文

请求或响应正文端点返回：

```json
{
  "revision": 46,
  "meta": {
    "transactionId": "TRANSACTION_ID",
    "side": "response",
    "contentType": "application/octet-stream",
    "encoding": "identity",
    "storedBytes": 3,
    "originalBytes": 3,
    "truncated": false
  },
  "base64": "AAEC"
}
```

`base64` 使用标准 Base64 字母表。事务存在但对应侧没有正文时返回
`404 bodyNotFound`；事务不存在时返回 `404 transactionNotFound`。

## 7. 实时事件

浏览器默认连接 `/api/v1/events/sse`；双向或兼容客户端可连接 WebSocket
`/api/v1/events`。两个入口共享同一事件源，连接后均先发送：

```json
{
  "type": "snapshot",
  "serverInstanceId": "00000000-0000-4000-8000-000000000001",
  "snapshot": {
    "serverInstanceId": "00000000-0000-4000-8000-000000000001"
  }
}
```

后续判别联合为：

```text
serviceState
metrics
processCapture
sessions
configuration
recording
transactions
advancedRepeats
plugins
```

每个事件在顶层携带当前进程随机生成的 `serverInstanceId`；`revision` 只在相同
实例标识内单调递增，后台重启后允许从低值重新计数。`snapshot` 事件的内外实例标识
必须相同。`serviceState` 事件只携带 `serviceState` 与 `listeners`；
`processCapture` 携带完整 `ProcessCaptureSnapshot`；
`recording` 携带完整 `RecordingSnapshot`；`transactions` 携带当前分页的
`TransactionPage`。只有用户显式 clear 才会让事务标识从新页面中消失，不使用只增不删的 upsert 语义。

高频 SOCKS/HTTP 会话、指标、高级重复进度与插件运行态按 50 毫秒窗口合并。接收方若落后，服务端重新发送完整
`snapshot`；前端应忽略同实例内早于当前修订号的事件和旧实例跨窗口消息，重连后以
新的初始快照绑定后台实例。若事件流首帧的实例标识与当前 HTTP 快照冲突，前端必须
重新读取 HTTP 快照仲裁：控制面确认同一新实例后才切换，仍返回旧实例时不得回退。

清空录制在正文存储提交成功后推进 SOCKS 捕获代际。投影器会拒绝清空前已经排队但
尚未消费的会话事件，避免被删除的事务或正文因广播延迟重新出现。

## 8. 资源上限

### 8.1 HTTP 正向代理

| 字段/预算 | 允许范围 |
|---|---|
| `listenPort` | 控制配置为 `1..=65535`；端口 `0` 只用于核心库测试 |
| `maxConnections` | `1..=16384` |
| `maxHeaderBytes` | `8192..=1048576` |
| 总头缓冲预算 | `maxHeaderBytes * maxConnections <= 268435456` |
| `maxCaptureBodyBytes` | `1..=67108864` |
| 总正文捕获缓冲预算 | `maxCaptureBodyBytes * 2 * maxConnections <= 536870912` |
| 四个 `*TimeoutMilliseconds` | `1..=300000` |

`maxCaptureBodyBytes` 是兼容既有配置的字段名，只限制必须随机访问正文的工具物化缓冲；
普通转发和录制镜像都不读取该值，任何大小的正文都会完整写入录制存储。

### 8.2 RecordingSession

`limits` 为旧客户端兼容保留的只读字段，三个值固定为 JavaScript 安全整数上限
`9007199254740991`。`PUT /recording` 不接受 `limits`，防止运行时重新启用正文裁剪或自动删除事务。
正文超过内存阈值后写入会话 spill 文件；只有用户显式调用 `clear` 才删除
已经录制的事务与正文。磁盘或内存写入失败必须作为结构化录制错误暴露，不能生成前缀正文。

`totalMetadataBytes` 公开当前元数据与正文引用的逻辑计费占用。clear 的正文引用在进入
清理队列后仍计入该值；只有物理删除成功并移出 `pendingCleanupCount` 后才扣除，删除失败
或任务取消不会伪造已经释放的资源。

## 9. 已知 M1b 边界

HTTP/1.1 请求头超过 `configuration.httpProxy.maxHeaderBytes` 时，Hyper 在进入
`HttpProxy` 服务回调前拒绝请求。该请求不会创建 Capture 事务，因此不能从事务列表
追溯这类头超限事件。此项是 M1b 服务器解析边界，不应由 M1c 伪造事务兜底。

CONNECT 在 M1 只建立裸 TCP 隧道；可按 `recordTunnelMetadata` 记录隧道元数据，但
不会生成解密后的 HTTPS 请求/响应头体。HTTPS 解密属于 M2。

SOCKS5 数据面会自动镜像成功转发的双向完整原始载荷和分片索引，并写入现有
请求/响应正文端点。它不增加公开 `sessions[]` 字段，也不把正文放入事件快照；
`contentType=application/octet-stream`、`encoding=binary` 表明该正文未经应用层解码。终态镜像会保留到录制器
确认接管正文，使广播丢帧后的权威快照仍可恢复，确认后会话历史释放原始镜像。

## 10. 语言协商与错误

HTTP 请求通过 `Accept-Language` 选择错误文案，也可以使用
`?locale=<BCP47>` 显式覆盖：

```text
locale 查询参数 → Accept-Language → en
```

支持 `en`、`zh-Hans`、`zh-Hant`、`ja`、`ko`、`es`、`fr`、`de`、`pt-BR`、
`ru` 及可归一化的区域标签。浏览器 EventSource 与 WebSocket 均不支持自定义请求头，因此事件入口可用
`/api/v1/events?locale=<BCP47>`。

控制错误使用固定结构：

```json
{
  "code": "invalidConfiguration",
  "message": "Configuration is invalid.",
  "messageKey": "error.invalidConfiguration",
  "params": {}
}
```

`code` 是稳定机器码，HTTP 状态表达请求错误、冲突或服务失败。`messageKey` 是稳定
目录键，`message` 按请求 locale 渲染。底层诊断只允许作为脱敏的 `params` 返回。

## 11. 认证材料

配置更新请求可携带一次性用户名和口令，但公共配置、快照、事件、日志、MCP 结果和文档
示例只返回用户名列表，不返回口令。口令不得进入错误参数或诊断文本。

## 12. 配置持久化与进程选择

核心监听、认证、容量、超时、二级代理、进程路径选择、录制暂停状态、录制忽略规则、隧道元数据开关、
SSL 主机范围、全部工具规则、反向代理、端口转发以及协议查看器配置统一写入安装目录下的
`data/configuration.json`。后端首次启动即生成该文件；首次升级会在新位置尚无配置时整体迁移旧版
用户数据目录，后续不再向旧位置写入；
每次配置提交都先校验完整候选，再以同目录临时文件同步落盘并原子替换，成功后才发布内存状态。
后续启动先严格校验文件并恢复配置；旧版文件缺少新增配置域时按安全默认值迁移并立即写回完整结构。
进程选择保存可执行路径而不是 PID，每次后端启动和显式启动服务时重新枚举进程，将同一路径的全部
运行实例解析为当前 PID。Protobuf 描述符原始字节保存在受控子目录，配置文件只保存已校验的索引和路由。
插件包、插件秘密配置和证书材料各自在安装目录 `data` 的受控子目录持久化，不复制进统一配置文件；
录制事务、正文、连接与计数是运行数据，新控制进程不会把它们伪装成可恢复设置。

主工具栏的“进程选择器”打开独立进程管理窗口。`GET /api/v1/processes` 返回实时进程清单、按路径
去重的 PNG 图标表及已保存路径；
`PUT /api/v1/processes` 完整替换路径集合和启用状态。已保存但暂未运行的路径仍保留在响应中。
