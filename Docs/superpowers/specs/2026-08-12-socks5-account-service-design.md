# SOCKS5 独立账号服务设计

## 1. 文档状态

- 日期：2026-08-12
- 状态：已实现
- 适用仓库：SprakCapture
- 影响边界：`proxyService`、`socks5-core`、Web 设置页、新增独立 `accountService`
- 不涉及：Android 客户端实现、APK 打包功能、节点下发协议

## 2. 背景

SprakCapture 当前在 `Socks5Config.users` 中保存认证用户，协议层已经支持 RFC 1929
用户名密码认证，但控制接口和持久化只按单账号工作：`ConfigurationUpdate.credentials`
只接收一个账号，保存配置时也只取 `users.iter().next()`。这套结构适合当前单账号模式，
不适合账号独立增删改查、在线 IP 限制、账号级共享带宽和远程管理。

本设计引入一个独立进程、独立数据库、独立管理页面和独立管理 API 的账号服务。
账号服务随 SprakCapture 生命周期运行，但数据库只能由账号服务访问。SOCKS5 数据面通过
内部服务接口完成认证和租约操作，不读取账号数据库。

## 3. 已确认需求

### 3.1 运行模式

SprakCapture 的 SOCKS5 设置增加唯一开关“多账号管理”。

- 关闭时：账号服务和远程管理端口都不运行，SOCKS5 使用当前单账号认证模式。
- 开启时：启动独立账号服务、远程管理页面和外部管理 API，SOCKS5 使用多账号认证模式。
- 两种模式互斥。
- 开启多账号后，数据面的有效认证模式固定为外部账号认证；当前 `NoAuth`、
  `UsernamePassword` 或 `Plugin` 模式只作为关闭多账号后要恢复的原模式保存，不与多账号组合。
- 第一次开启多账号管理时不导入当前单账号。
- 关闭多账号管理不删除账号数据库；再次开启继续使用原数据。
- 单账号配置始终保留，不被多账号配置覆盖。

### 3.2 账号字段

每个 SOCKS5 账号包含以下业务字段：

| 字段 | 类型 | 语义 |
|---|---:|---|
| `username` | UTF-8 字符串 | 唯一账号，长度为 1 至 255 字节 |
| `password` | 可空 UTF-8 字符串 | 空值表示服务端接受该账号提交的任意非空 RFC 1929 密码 |
| `maxUploadBytesPerSecond` | `i64` | `-1` 不限制，`0` 禁用，正数为账号共享上行字节率 |
| `maxDownloadBytesPerSecond` | `i64` | `-1` 不限制，`0` 禁用，正数为账号共享下行字节率 |
| `maxConnections` | `i64` | `-1` 不限制，`0` 禁用，正数为账号最大活动连接数 |
| `maxOnlineIps` | `i64` | `-1` 不限制，`0` 禁用，正数为账号最大同时在线来源 IP 数 |
| `expiresAt` | `i64` | `-1` 永不过期，`0` 禁用，正数为 UTC Unix 毫秒时间戳 |

正数 `expiresAt` 小于账号服务当前时间时，账号视为过期。所有限制的权威时间均来自账号
服务进程，不接受 SOCKS5 节点或浏览器提交的当前时间。

### 3.3 带宽范围

上行、下行限制是账号全部活动连接共享的总带宽，不是每条连接分别限速：

- 上行：SOCKS5 客户端发往目标服务器的有效载荷。
- 下行：目标服务器返回 SOCKS5 客户端的有效载荷。
- TCP、BIND 和 UDP ASSOCIATE 的有效载荷均进入同一账号级方向预算。
- 协议握手字节和账号服务管理流量不计入代理流量。

### 3.4 管理入口

- 管理页面与自动化 API 使用同一个独立 HTTP 端口。
- 默认监听：`0.0.0.0:19090`。
- 默认页面：`http://HOST:19090/`。
- 外部 API 根路径：`http://HOST:19090/api/v1/`。
- 管理员默认账号为 `Admin`，默认密码为 `Admin123`。
- 管理员账号或密码变化时，自动化 API Key 必须同步变化，旧 Key 和旧浏览器会话失效。
- 管理员账号体系与 SOCKS5 账号体系完全隔离。

### 3.5 数据库

账号服务使用独占的 SQLite 数据库：

```text
data/
└── accountService/
    └── accounts.db
```

数据库启用 WAL、外键约束和忙等待。运行期间 SQLite 可生成标准的 `accounts.db-wal` 和
`accounts.db-shm` 辅助文件；它们与主数据库属于同一持久化单元。

## 4. 目标与非目标

### 4.1 目标

1. 保持当前单账号模式完全可用。
2. 提供独立、可测试、可替换存储实现的账号服务。
3. 原子执行认证、连接上限和在线 IP 上限判定。
4. 对同一账号的全部连接执行共享上下行限速。
5. 支持浏览器远程管理和自动化 API。
6. 支持账号创建、修改、删除、密码设置、强制下线、连接查看和流量统计。
7. 账号限制变更可作用于现有连接，不要求重启整个 SprakCapture。
8. 账号服务失联时数据面明确失败，不回退到单账号或无认证模式。

### 4.2 非目标

1. 不支持多个 SprakCapture 节点共享同一账号数据库或全局额度。
2. 不引入 Redis、PostgreSQL 或外部数据库服务。
3. 不实现多管理员、角色权限或租户体系。
4. 不把管理账号作为 SOCKS5 账号使用。
5. 不修改 Android 客户端、APK 模板或打包流程。
6. 不把多账号记录写回当前 `configuration.json`。
7. 不让 SOCKS5 服务直接读取 SQLite。

## 5. 总体架构

```text
┌──────────────────────── SprakCapture Desktop ────────────────────────┐
│                                                                      │
│  Web 设置页 ─────► proxyService 控制 API                             │
│                         │                                            │
│                         ├─ 启停并监督 accountService 独立进程         │
│                         ├─ 启停 SOCKS5 融合监听                       │
│                         └─ 保存单账号与多账号开关配置                 │
│                                                                      │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ 本机内部 HTTP + 进程级随机令牌
                               ▼
┌──────────────────────── accountService ──────────────────────────────┐
│                                                                      │
│  公共 HTTP 管理端 ── 管理页面、登录、账号 API、统计、审计             │
│  内部运行接口 ────── 认证、租约、心跳、流量上报、释放                 │
│  AccountStore ────── 唯一 SQLite 访问方                              │
│  LeaseRegistry ───── 内存活动租约、连接数、在线 IP                    │
│                                                                      │
└──────────────────────────────┬───────────────────────────────────────┘
                               │
                               ▼
                      data/accountService/accounts.db

SOCKS5 客户端 ─► socks5-core ─► AccountAuthenticationProvider
                                  │
                                  ├─ 内部认证和租约 API
                                  └─ 本地 AccountRateLimiterRegistry
```

## 6. 组件职责

### 6.1 `accountService` 独立进程

建议新增 Cargo crate `AccountService`，生成独立二进制 `accountService`。它只负责：

- 独占打开、迁移和访问 `accounts.db`。
- 提供公共管理页面和 `/api/v1` 管理 API。
- 提供仅回环绑定的 `/internal/v1` 运行接口。
- 验证 SOCKS5 账号密码。
- 原子创建、更新、撤销和回收活动租约。
- 维护账号连接数与在线 IP 集合。
- 聚合并持久化流量统计。
- 维护管理身份、浏览器会话、API Key 和审计日志。

账号服务不读取 SOCKS5 流内容、不连接目标服务器、不执行虚拟时间限速等待，也不读取
SprakCapture 的录制数据库。

### 6.2 `proxyService` 监督器

`proxyService` 根据“多账号管理”开关管理账号服务：

- 开启时启动子进程，并等待内部健康检查成功。
- 将数据库目录、公共监听地址和内部握手管道传给子进程。
- 为每次子进程启动生成新的 256 位内部访问令牌，通过匿名管道传递，不写入配置、参数或日志。
- 账号服务异常退出后停止接受新的多账号 SOCKS5 连接，并取消现有多账号会话。
- 按有界退避重新启动账号服务；健康检查恢复后再恢复 SOCKS5 数据面。
- SprakCapture 完整退出时先停止 SOCKS5 数据面，再停止账号服务。
- 桌面关闭到托盘时保持两个服务运行。

账号服务作为 `proxyService` 的子进程运行，继续受到现有桌面进程作业的统一回收约束。

### 6.3 SOCKS5 认证提供器

不要把多账号逻辑继续塞入 `Socks5Config.users`，也不要借用插件认证语义。`socks5-core`
增加独立的认证提供器抽象：

```text
authenticate(username, password, sourceIp, connectionId)
    -> AuthenticatedAccount(accountId, leaseId, policy, policyRevision)
```

- 单账号模式继续使用当前内存认证路径。
- 插件模式继续使用当前 `PluginHost` 路径。
- 多账号模式注入 `AccountAuthenticationProvider`。
- 协议层仍只负责 RFC 1929 帧读取和成功/失败响应。
- 账号服务拒绝原因只进入内部诊断；远端始终收到标准认证失败。

有效认证模式矩阵如下：

| `multiAccount.enabled` | 保存的现有模式 | 数据面有效模式 | 关闭多账号后的模式 |
|---|---|---|---|
| `false` | `NoAuth` | `NoAuth` | `NoAuth` |
| `false` | `UsernamePassword` | `UsernamePassword` | `UsernamePassword` |
| `false` | `Plugin` | `Plugin` | `Plugin` |
| `true` | 任意上述模式 | `ExternalAccount` | 原样恢复保存模式 |

`ExternalAccount` 只存在于运行时认证提供器选择，不写入 `AuthenticationMode`、
`PublicAuthenticationMode` 或 `configuration.json`，也不复用 `Plugin`。多账号开启时配置
校验要求账号服务健康，UI 隐藏但保留原认证模式和单账号凭据；插件认证钩子不参与账号判定。

### 6.4 账号级限速注册表

`AccountRateLimiterRegistry` 位于 SOCKS5 数据面进程中，并以稳定 `accountId` 为键：

- 每个账号维护一个共享上行虚拟时间调度器和一个共享下行虚拟时间调度器。
- 所有 TCP 和 UDP 会话进入同一方向调度器消费额度。
- 策略修订号变化时原子替换速度和突发容量。
- 最后一个连接释放后回收该账号的限速器。
- 不用可修改的 `username` 作为限速键。

账号服务不参与每个数据块的转发，避免内部 API 成为数据面瓶颈。

## 7. 生命周期与模式切换

### 7.1 首次开启多账号管理

1. 校验公共管理监听地址和独立端口。
2. 创建 `data/accountService/`。
3. 启动账号服务并创建空数据库。
4. `proxyService` 使用本次进程内部令牌调用 `/internal/v1/management/bootstrap`，由账号服务
   在同一 SQLite 事务中初始化管理身份 `Admin / Admin123` 和首次 API Key。
5. 账号服务持久化完整 Key 的校验摘要；完整值仅在管理员主动获取时通过直接响应返回。
6. 等待账号服务内部健康检查成功。
7. 以多账号认证提供器重启 SOCKS5 数据面。
8. 发布新的控制快照。

首次数据库没有任何 SOCKS5 账号，因此在管理员创建账号前，所有 SOCKS5 认证均被拒绝。
当前单账号不会自动导入。

如果账号服务已经完成首次初始化，但内部响应丢失或后续 SOCKS5 数据面启动失败，则数据库
初始化不回滚，运行模式仍回滚为单账号。API Key 由当前管理账号、不可逆密码摘要和数据库中的
派生参数确定；已授权的 SprakCapture 控制面或管理会话可以直接恢复同一个当前 Key，不需要再次
提交管理密码。API Key 只随管理账号或密码修改而变化，不提供独立轮换操作。

### 7.2 再次开启

- 严格校验现有数据库并执行版本迁移。
- 恢复管理身份、账号、统计和审计记录。
- 不重建默认管理员，不重置 API Key，不清空账号。
- 活动租约从空状态开始；旧进程中的连接已在上次关闭时结束。

### 7.2.1 从 SprakCapture 修改管理身份

设置页把新管理账号和新管理密码作为一次性字段提交给 `proxyService` 控制 API。当前控制面会话
和内部令牌已经完成授权，不再要求当前账号或密码。账号服务在同一 SQLite 事务中更新密码摘要、
递增凭据和会话修订号、生成对应的新 API Key 并写入审计记录；完整新 Key 只通过本次响应返回。
`proxyService` 不读取 SQLite，也不把这些明文字段写入 `configuration.json`。

### 7.3 关闭多账号管理

1. 控制面进入配置更新状态并拒绝并发模式切换。
2. 停止 SOCKS5 接受循环并取消现有多账号连接。
3. 刷新尚未持久化的流量增量。
4. 停止公共管理页面、外部 API 和内部运行接口。
5. 关闭 SQLite 连接并停止账号服务进程。
6. 使用原先保留的单账号配置重启 SOCKS5 数据面。
7. 保留 `accounts.db`，不修改其中账号或管理身份。

### 7.4 切换失败

- 开启时任一步骤失败，配置事务回滚为单账号模式，原单账号数据面恢复。
- 关闭时账号服务停止失败，监督器终止其进程树，再启动单账号数据面。
- 端口冲突、数据库损坏或迁移失败必须返回具体配置错误，不发布“已开启”状态。
- 多账号运行中账号服务失联时执行失败关闭，不自动回退到单账号模式。

## 8. 管理身份与 API Key

### 8.1 管理员

第一版只存在一个管理员。数据库保存：

- 管理员账号。
- Argon2id 密码摘要和随机盐。
- 单调递增的 `credentialRevision`。
- API Key 摘要、前缀和生成时间。
- 浏览器会话修订号。

管理员账号匹配采用精确 UTF-8 字节语义；前后空白属于账号内容，不做静默裁剪。管理员账号和
密码都必须是 1 至 255 个 UTF-8 字节。浏览器会话使用管理凭据摘要派生材料签名，不建立易失
会话表；数据库中的会话修订号用于身份修改时一次性撤销全部旧 Cookie。

### 8.2 API Key 生成

API Key 在初始化管理身份或修改管理员账号/密码时生成：

```text
credentialMaterial = Argon2id(passwordHash, apiKeySalt)
keySecret = HMAC-SHA256(
    credentialMaterial,
    normalizedUsername || credentialRevision || databaseInstanceId
)
apiKey = "sak_v1_" || keyId || "_" || base64url(keySecret)
```

- `apiKeySalt`、`keyId` 和 `databaseInstanceId` 使用安全随机值。
- `normalizedUsername` 在本设计中就是管理员账号的原始 UTF-8 字节，不进行大小写、Unicode
  或空白规范化；名称保留在公式中只表示该字节序列已经通过长度校验。
- 完整 API Key 不持久化；初始化和身份修改时生成，已授权入口可按当前摘要恢复并直接返回。
- 数据库只保存 API Key 摘要和用于展示的短指纹。
- 管理员账号或密码变化必须递增 `credentialRevision`，因此新 Key 与旧 Key 不同。
- 旧 API Key 摘要和全部旧浏览器会话在同一事务中失效。
- 用户未保存完整 Key 时，可由现有管理会话或 SprakCapture 控制面直接获取同一个当前 Key；
  获取操作不接收密码、不改变 `credentialRevision`，也不产生新的 Key。

自动化请求使用：

```http
Authorization: Bearer API_KEY
```

### 8.3 浏览器会话

- 登录成功后生成包含随机 nonce 和身份修订号的签名会话标识。
- Cookie 使用 `HttpOnly`、`SameSite=Strict` 和根路径作用域。
- Cookie 使用十年持久期；刷新页面和账号服务重启不会结束会话，用户主动退出时由浏览器删除。
- 修改管理身份或更换账号数据库时签名材料或修订号变化，现有会话全部失效。
- SprakCapture 概览可以签发 30 秒内单次有效的随机入口票据；首次访问把票据换成普通 HttpOnly
  会话并立即重定向清除 URL。直接输入管理页 URL 仍显示账号密码登录页。
- 登录和 API Key 校验使用统一失败响应，不区分账号不存在、密码错误或 Key 已失效。

## 9. SQLite 数据模型

所有业务标识使用 UUID 文本。所有时间保存 UTC Unix 毫秒。自有 JSON 字段和 Rust 模型使用
camelCase；SQL 列名保持同样语义，避免转换层产生两套命名。

### 9.1 `schemaMigrations`

| 列 | 说明 |
|---|---|
| `version` | 单调递增版本 |
| `appliedAt` | 应用时间 |
| `checksum` | 迁移内容摘要 |

### 9.2 `managementIdentity`

单行表，主键固定为 `1`：

| 列 | 说明 |
|---|---|
| `username` | 管理员账号 |
| `passwordHash` | Argon2id 摘要 |
| `passwordSalt` | 随机盐 |
| `credentialRevision` | 管理身份版本 |
| `apiKeyHash` | API Key 摘要 |
| `apiKeyPrefix` | 脱敏展示前缀 |
| `apiKeySalt` | API Key 派生随机盐 |
| `apiKeyId` | API Key 公开短标识 |
| `apiKeyCreatedAt` | Key 生成时间 |
| `databaseInstanceId` | 数据库实例随机标识 |
| `browserSessionRevision` | 浏览器会话撤销版本 |
| `updatedAt` | 修改时间 |

### 9.3 `accounts`

| 列 | 约束 |
|---|---|
| `accountId` | 主键 UUID |
| `username` | 唯一、非空、1 至 255 UTF-8 字节 |
| `passwordHash` | 可空；空值表示任意非空密码 |
| `passwordSalt` | 与 `passwordHash` 同时为空或同时非空 |
| `remark` | 可空管理备注，最多 512 个 UTF-8 字节 |
| `maxUploadBytesPerSecond` | `>= -1` |
| `maxDownloadBytesPerSecond` | `>= -1` |
| `maxConnections` | `>= -1` |
| `maxOnlineIps` | `>= -1` |
| `expiresAt` | `>= -1` |
| `policyRevision` | 单账号策略修订号 |
| `createdAt` | 创建时间 |
| `updatedAt` | 修改时间 |

`username` 创建后不可修改。这样可避免远端客户端配置、审计记录和历史统计出现身份漂移；
需要更换用户名时创建新账号并删除旧账号。固定密码必须为 1 至 255 个 UTF-8 字节；创建或
设置密码时，JSON `null` 表示任意非空密码模式，空字符串属于非法固定密码而不是 `null`。

### 9.4 `usageCounters`

每个账号一行：

| 列 | 说明 |
|---|---|
| `accountId` | 外键 |
| `uploadedBytes` | 累计上行字节 |
| `downloadedBytes` | 累计下行字节 |
| `acceptedConnections` | 累计成功租约数 |
| `rejectedAuthentications` | 累计认证拒绝数 |
| `lastConnectedAt` | 最近成功连接时间，可空 |
| `updatedAt` | 最后聚合时间 |

### 9.5 `usageDaily`

以 `(accountId, utcDate)` 为联合主键，保存每日上行、下行、成功连接和拒绝次数。它只保存
聚合结果，不保存密码或请求目标。

### 9.6 `auditLogs`

记录管理写操作：

- `auditId`
- `occurredAt`
- `actorType`：`browserSession` 或 `apiKey`
- `actorFingerprint`
- `action`
- `accountId`，可空
- `result`
- 不含密码、API Key、密码摘要或内部访问令牌的结构化详情

活动租约不写入 SQLite。它们属于当前账号服务进程的易失运行状态；服务重启后 SOCKS5
数据面会终止旧连接并重新认证，不需要从数据库猜测连接是否仍然存活。

## 10. 账号状态判定

账号服务按固定顺序判断：

1. 查询精确匹配的 `username`。
2. 判断 `expiresAt == 0`。
3. 判断正数 `expiresAt < serverTime`。
4. 判断任一限制字段是否为 `0`。
5. 校验密码；`passwordHash` 为空时接受任意非空密码。
6. 清理超时租约。
7. 判断活动连接数是否达到 `maxConnections`。
8. 规范化来源 IP，并判断加入后不同 IP 数是否达到 `maxOnlineIps`。
9. 在同一账号锁内创建租约并增加计数。
10. 返回账号 ID、租约 ID、策略和策略修订号。

任一带宽、连接数、在线 IP 或到期字段为 `0` 时，账号整体禁用。把现有账号字段修改为
`0`、把到期时间修改为过去时间或删除账号时，账号服务撤销其全部租约；SOCKS5 数据面在
下一次内部同步中关闭对应连接。

来源 IP 由受信任的 SOCKS5 数据面从 TCP 接受循环的远端套接字地址提取，再通过仅限内部调用者
访问的认证接口提交；账号服务不接受公共管理请求提供来源 IP，也不读取或信任转发 HTTP 头。
IPv4-mapped IPv6 规范化为 IPv4；普通 IPv6 保留完整 128 位地址。

## 11. 连接租约

每个成功认证的 SOCKS5 控制连接创建一个租约：

```text
leaseId
accountId
connectionId
sourceIp
createdAt
lastHeartbeatAt
policyRevision
uploadedBytes
downloadedBytes
revoked
```

### 11.1 心跳与流量上报

- SOCKS5 数据面每 2 秒批量上报所有活动租约的心跳和单调累计流量。
- `batchId` 在本次账号服务进程的 `serviceInstanceId` 内唯一；同一批次重试必须携带完全相同
  的租约累计值。账号服务在租约内保存已确认累计值，以差值入账；批次重复不重复累计。
- 账号服务返回最新策略修订号和被撤销的租约列表。
- 账号服务超过 8 秒未收到租约心跳时回收租约。
- 正常断开立即调用批量释放接口，不等待超时。
- 流量聚合每 5 秒或单账号待写增量达到 1 MiB 时提交 SQLite，以先满足任一条件为准。
- 累计流量是运行统计而非计费账本。账号服务进程异常终止时，允许每个活跃账号丢失不足
  5 秒且不足 1 MiB 的尚未提交增量；服务重启后旧租约作废，旧数据面不补记这些增量。

### 11.2 强制下线

管理端对账号执行强制下线时：

1. 账号服务把当前租约标记为撤销。
2. 内部同步响应立即返回全部被撤销的 `leaseId`。
3. SOCKS5 数据面取消对应连接及 UDP 关联。
4. 数据面提交最后一次流量增量并释放本地限速器引用。
5. 强制下线本身不禁用账号，后续新认证仍可成功。

## 12. 账号共享限速

### 12.1 算法

每个账号每个方向使用带公平等待队列的虚拟时间调度器：

- 调度速率为账号配置的每秒字节数，并允许最多一秒额度的突发信用。
- 每次预留根据字节数推进该账号方向的 `nextAvailableAt`；发送方必须等待预留时间到达。
- 该算法允许任意合法 UDP 数据报预留多个补充周期，不依赖令牌桶积累到大于固定容量。
- 等待队列按连接轮转，避免单条大流长期占满额度。
- 每次读取前先申请最大可读预算，再执行网络读取，避免读取后长期占用内存等待令牌。
- 插件修改流量时以最终写出字节数计费，录制镜像不重复计费。
- 策略从有限值变为 `-1` 时唤醒全部等待者并移除限速等待。
- 策略变为 `0` 时取消账号全部连接。

### 12.2 TCP 与 UDP

- TCP 两个方向分别进入对应的共享虚拟时间调度器。
- UDP 每个数据报按有效载荷字节数消费对应方向额度。
- 单个 UDP 数据报可预留多个补充周期；到达调度时间后完整发送，不拆分或截断数据报。
- 空闲超时不计算限速等待时间，避免低带宽配置被误判为空闲连接。

## 13. 内部运行接口

内部接口仅绑定 `127.0.0.1` 的系统分配端口。账号服务就绪后通过匿名握手管道把实际端点
返回给 `proxyService`。所有请求必须携带每次进程启动重新生成的内部令牌。

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` | `/internal/v1/health` | 返回数据库和租约注册表状态 |
| `POST` | `/internal/v1/leases/authenticate` | 认证并原子创建租约 |
| `POST` | `/internal/v1/leases/synchronize` | 批量心跳、流量增量、策略更新和撤销结果 |
| `POST` | `/internal/v1/leases/release` | 幂等批量释放租约 |
| `POST` | `/internal/v1/management/bootstrap` | 首次创建默认管理身份并返回派生 API Key |
| `PUT` | `/internal/v1/management/identity` | SprakCapture 设置页修改管理身份并生成对应 Key |
| `GET` | `/internal/v1/management/apiKey` | 内部令牌授权后恢复当前完整 Key，无请求正文 |
| `POST` | `/internal/v1/management/session` | 签发 30 秒内单次有效的管理登录入口 |
| `GET` | `/internal/v1/statistics` | 返回在线账号、连接数和实时上下行字节率的脱敏摘要 |
| `POST` | `/internal/v1/shutdown` | 有序刷新统计并停止服务 |

认证请求：

```json
{
  "connectionId": "CONNECTION_ID",
  "username": "ACCOUNT",
  "password": "PASSWORD",
  "sourceIp": "203.0.113.10"
}
```

成功响应：

```json
{
  "serviceInstanceId": "ACCOUNT_SERVICE_INSTANCE_ID",
  "accountId": "ACCOUNT_ID",
  "leaseId": "LEASE_ID",
  "username": "ACCOUNT",
  "policyRevision": 3,
  "maxUploadBytesPerSecond": -1,
  "maxDownloadBytesPerSecond": 10485760
}
```

内部健康响应和进程启动握手都返回本次随机 `serviceInstanceId`。认证成功响应也返回相同
标识，供数据面检测并拒绝跨账号服务实例的租约操作。内部响应不返回账号密码摘要、管理身份
或数据库路径。

`/leases/synchronize` 请求必须包含：

```json
{
  "serviceInstanceId": "ACCOUNT_SERVICE_INSTANCE_ID",
  "batchId": "BATCH_ID",
  "leases": [
    {
      "leaseId": "LEASE_ID",
      "connectionId": "CONNECTION_ID",
      "uploadedBytes": 1024,
      "downloadedBytes": 2048,
      "final": false
    }
  ]
}
```

其中字节数是租约生命周期内的单调累计值。账号服务响应每条租约的确认累计值、当前策略、
`policyRevision`、`revoked` 和可选稳定错误代码。未知、已超时或实例不匹配的租约按撤销处理；
同批次不同载荷返回 `409`，网络失败允许原样重试。`release` 使用相同累计字段并设置
`final=true`，重复释放返回最后一次确认结果。账号服务重启会改变 `serviceInstanceId`；旧实例
批次不再接收，SOCKS5 数据面关闭旧连接。

`proxyService` 把内部端点、内部令牌和 `serviceInstanceId` 作为一个不可拆分的运行快照
原子发布给认证提供器。账号服务重启时先取消使用旧快照的全部连接，完成新握手并替换完整
快照后，才启动新的多账号 SOCKS5 接受循环。

管理初始化和身份修改的明文只存在于对应内部请求内存中。内部管理响应可以携带完整 API Key，
但不得进入控制快照、日志或持久化配置。

## 14. 外部管理 API

### 14.1 认证接口

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/api/v1/auth/login` | 管理员账号密码登录 |
| `GET` | `/api/v1/auth/local?ticket=...` | 消费控制面一次性票据、设置会话 Cookie 并重定向到首页 |
| `POST` | `/api/v1/auth/logout` | 删除当前浏览器会话 |
| `GET` | `/api/v1/auth/session` | 查询当前会话 |
| `PUT` | `/api/v1/management/identity` | 修改管理账号/密码并返回新 API Key |
| `GET` | `/api/v1/management/apiKey` | 当前会话授权后返回当前完整 Key，无请求正文 |

### 14.2 账号接口

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/v1/accounts` | 分页、搜索、筛选和排序 |
| `POST` | `/api/v1/accounts` | 创建账号 |
| `PATCH` | `/api/v1/accounts/batch` | 原子批量修改在线 IP、连接数、共享带宽并按原到期时间加时 |
| `DELETE` | `/api/v1/accounts/batch` | 按选择快照原子批量删除账号 |
| `GET` | `/api/v1/accounts/{accountId}` | 读取账号详情和实时摘要 |
| `PATCH` | `/api/v1/accounts/{accountId}` | 更新限制、到期时间和备注 |
| `DELETE` | `/api/v1/accounts/{accountId}` | 删除账号并撤销全部租约 |
| `PUT` | `/api/v1/accounts/{accountId}/password` | 设置指定密码 |
| `DELETE` | `/api/v1/accounts/{accountId}/password` | 切换为任意非空密码模式 |
| `GET` | `/api/v1/accounts/{accountId}/connections` | 查询活动连接和在线 IP |
| `POST` | `/api/v1/accounts/{accountId}/disconnect` | 强制下线全部活动连接 |
| `GET` | `/api/v1/accounts/{accountId}/usage` | 查询累计和每日流量 |

创建请求示例：

```json
{
  "username": "ACCOUNT",
  "password": null,
  "maxUploadBytesPerSecond": -1,
  "maxDownloadBytesPerSecond": -1,
  "maxConnections": -1,
  "maxOnlineIps": 1,
  "expiresAt": -1,
  "remark": "移动客户端"
}
```

查询响应使用 `passwordMode: "any" | "fixed"`，不返回密码、盐或摘要。更新账号需要提交
`policyRevision`；版本落后返回 `409 Conflict`，防止多个管理页面覆盖彼此修改。

### 14.3 统计和审计

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/api/v1/statistics` | 账号总数、在线账号、在线 IP、实时流量 |
| `GET` | `/api/v1/connections` | 分页查询全部活动连接 |
| `GET` | `/api/v1/auditLogs` | 分页查询管理操作 |
| `GET` | `/api/v1/health` | 公共服务状态，不返回数据库细节 |
| `GET` | `/api/v1/openapi.json` | OpenAPI 3.1 文档 |

### 14.4 HTTP 状态和错误体

- `400`：字段、数值范围或时间格式非法。
- `401`：管理会话或 API Key 无效。
- `404`：账号不存在。
- `409`：用户名冲突、策略版本冲突或状态转换冲突。
- `422`：请求结构正确但违反账号业务约束。
- `429`：管理登录尝试超过窗口限制。
- `500`：数据库事务或内部一致性错误。
- `503`：数据库迁移中或服务未就绪。

错误体使用稳定代码：

```json
{
  "code": "accountPolicyRevisionConflict",
  "message": "账号策略已被其他管理端修改。",
  "params": {
    "currentRevision": 4
  }
}
```

密码、API Key、内部令牌和数据库语句不得进入错误体、日志或审计详情。

## 15. 统一远程管理页面

远程管理不再提供独立账号服务 URL。账号服务在唯一公开监听上托管编译后的 Sprak Capture Web，账号工作区固定映射到主应用的 `/account-management` 路由；桌面 WebView 和 Vite 开发环境也通过同一路由进入账号工作区，不创建系统浏览器弹窗或第二个用户入口。

### 15.1 登录与会话

- 仅远程生产入口要求登录；桌面 WebView 与 Vite 开发入口明确免认证。
- 登录账号和密码与 SOCKS5 账号管理的管理员身份完全共用。
- Cookie 持久保存，刷新和账号服务重启后继续有效；只有主动退出或管理员凭据改变才失效。
- 未登录时只加载登录门禁，不启动主控制 API 或 SSE。

### 15.2 账号工作区

- 左侧导航只包含“概览”和“账号管理”，默认进入概览。
- 概览显示在线账号、在线 IP、连接数、实时上下行字节率和活动连接明细。
- 账号表格支持搜索、筛选、排序、新建、选择后批量编辑和批量删除。
- 批量加时以每个账号原到期时间为基准；已过期账号不改用当前时间，`-1/0` 保持不变。
- 账号工作区不重复提供登录、退出、流量统计、审计日志或管理员设置；统一退出位于主工作台工具栏。

## 16. Sprak Capture 设置集成

`设置 → 远程管理` 是唯一配置入口：

- `启用远程管理` 同时控制公开 Web 监听与 SOCKS5 独立账号认证，避免再增加多账号开关。
- 远程监听地址默认 `0.0.0.0`，端口默认 `19090`。
- 管理员账号密码、API Key 指纹和主动获取 Key 均留在这一页。
- 关闭远程管理时账号服务只在随机回环端口运行，用于维护数据库和共享管理员身份；不会公开 Web。
- 开启后隐藏单账号输入框但保留已有值，关闭后恢复单账号认证。

账号管理按钮只在 Sprak Capture 概览显示，并在当前路由内进入 `/account-management`；不显示、复制或打开账号服务监听 URL。概览继续显示在线账号、连接数和实时总上下行字节率。

公共控制快照只返回：

```json
{
  "multiAccount": {
    "enabled": true,
    "remoteHost": "0.0.0.0",
    "remotePort": 19090,
    "state": "running",
    "apiKeyPrefix": "sak_v1_ab12_••••",
    "apiKeyCreatedAt": 1786473600000,
    "summary": {
      "onlineAccounts": 2,
      "activeConnections": 3,
      "uploadBytesPerSecond": 1024,
      "downloadBytesPerSecond": 2048
    },
    "error": null
  }
}
```

完整 API Key 只允许出现在身份修改或主动获取操作的直接响应中，不进入长期快照或事件。

## 17. 配置和兼容性

`data/configuration.json` 只保存远程监听配置：

```json
{
  "multiAccount": {
    "enabled": false,
    "remoteHost": "0.0.0.0",
    "remotePort": 19090
  }
}
```

- 单账号 `authenticationMode` 和 `credentials` 契约继续保留。
- 开启远程管理时数据面使用独立账号认证，但不删除单账号凭据。
- 旧 `managementHost/managementPort` 字段只在反序列化时迁移为 `remoteHost/remotePort`，不再对外输出旧字段。
- 旧配置缺少该段时默认关闭公开远程监听；Vite 开发服务器仍默认监听所有接口且免认证。
- 账号服务随主服务生命周期运行；远程关闭时绑定 `127.0.0.1:0`，远程开启时绑定配置地址。
- 控制 API 完整配置更新继续使用先校验、原子持久化、再切换运行状态的事务语义。
- `/api/v1/service/start` 和 `/stop` 只控制代理数据面，不关闭账号数据库服务。
- `GET /api/v1/multiAccount/apiKey` 读取当前 Key；`POST /api/v1/multiAccount/managementSession` 只返回 `/account-management` 下的一次性相对路径，不返回子服务 URL。
## 18. 并发和一致性

- 每个账号拥有唯一运行状态锁。认证、连接数、在线 IP、租约创建、策略更新和删除都先取得
  该锁；取得账号锁后才能开启该账号的 SQLite 写事务，不允许反向持锁，避免锁顺序循环。
- 不持有 SQLite 事务等待网络 I/O。
- 账号策略更新在同一 SQLite 事务中校验请求携带的旧 `policyRevision` 并递增数据库修订号；
  事务提交后、释放账号锁前，把携带相同新修订号的策略发布到内存快照并撤销不再合法的租约。
- 认证持有同一个账号锁读取当前内存策略并创建租约，因此不能在策略提交和内存发布之间根据
  旧策略创建新租约。进程若在数据库提交后、内存发布前退出，重启时从数据库加载新策略。
- 租约同步使用批次 ID 幂等处理；重复批次不重复累计流量。
- 删除账号在账号锁内提交删除和审计事务，随后标记内存租约撤销，再释放账号锁。
- 数据库写入失败时不发布内存成功状态。
- 统计增量写入失败时保留在内存待重试；账号服务有序退出必须在时限内刷新。
- 账号服务进程退出意味着全部租约失效，数据面必须关闭对应连接，不能沿用旧授权结果。

## 19. 可观测性

账号服务公开脱敏运行指标：

- 管理请求数、失败数和延迟。
- 认证成功、失败和拒绝原因计数。
- 活动账号、活动连接和在线 IP 数。
- 心跳批次延迟、超时租约回收数。
- SQLite 提交延迟和待写流量增量。
- 上行、下行实际字节和限速等待时间。

日志使用账号 ID、租约 ID、连接 ID 和脱敏来源 IP；不记录账号密码、API Key、内部令牌、
密码摘要或完整请求体。

## 20. 测试与验收

### 20.1 数据库

- 首次创建、重复启动和逐版本迁移。
- WAL 恢复、事务回滚、唯一用户名和所有 `CHECK` 约束。
- 密码为空与固定密码的持久化语义。
- 数据库损坏时拒绝进入多账号运行状态。

### 20.2 管理身份

- 默认 `Admin / Admin123` 可登录。
- 错误账号和错误密码返回相同结果。
- 修改管理账号或密码都会产生不同 API Key；不提供独立轮换操作。
- bootstrap 或身份修改响应丢失时，已授权入口可不提交密码直接恢复同一个当前 Key。
- 旧 API Key 和旧浏览器会话立即失效。
- 完整 API Key 不进入快照、日志、审计和数据库明文字段。
- 一次性管理入口 30 秒内有效且只能消费一次，成功后建立 HttpOnly 会话并从 URL 清除票据。

### 20.3 账号认证

- 固定密码正确、错误和空字段。
- `passwordHash=NULL` 接受任意非空密码。
- 所有限制的 `-1/0/正数` 边界。
- 正数到期时间在服务器时间前后的行为。
- 不存在账号、删除账号和策略并发更新。

### 20.4 连接和 IP

- 同一 IP 多连接只占一个在线 IP。
- 不同 IP 并发到达时不能穿透 `maxOnlineIps`。
- 并发认证不能穿透 `maxConnections`。
- IPv4-mapped IPv6 与对应 IPv4 只计一个 IP。
- 正常释放、异常断开、心跳超时和强制下线都正确回收计数。

### 20.5 共享限速

- 单连接与多连接总上行不超过账号额度。
- 单连接与多连接总下行不超过账号额度。
- 上下行额度互不串用。
- TCP 和 UDP 共享同一账号方向额度。
- 一秒突发边界、长时间平均速率和公平轮转。
- 单个 UDP 数据报大于一秒额度时，预留多个补充周期后仍能完整发送。
- 运行中从有限改为不限、从不限改为有限、改为零和账号过期。
- 限速等待不触发错误的空闲超时。

### 20.6 生命周期

- 开启多账号时不导入单账号。
- 空账号数据库拒绝全部 SOCKS5 认证。
- 关闭后恢复原单账号配置。
- 再次开启恢复原多账号数据库。
- 公共管理端口冲突时原子回滚。
- 账号服务异常退出后关闭现有连接且不回退认证模式。
- 账号服务重启后原子替换内部端点、令牌和 `serviceInstanceId`，旧实例批次全部拒绝。
- SprakCapture 退出、托盘驻留和后端重启无遗留进程。

### 20.7 外部接口和页面

- 浏览器登录、刷新、服务重启持久化、主动退出、一次性入口和身份变更失效。
- API Key 调用、分页、搜索、筛选、版本冲突和幂等写入。
- 页面不展示密码、摘要或完整历史 API Key。
- 页面左侧导航只有概览和账号管理；概览显示活动连接与实时上下行，主工作台按钮可免登录进入。
- 管理页按 `web/index.html`、`web/styles.css`、`web/app.js` 分离结构、样式和交互并随二进制嵌入。
- OpenAPI 3.1 文档与真实响应通过契约测试保持一致。
- 窄屏和桌面宽屏均可完成账号管理操作。

## 21. 交付顺序

1. 建立 `accountService` crate、SQLite 迁移和账号存储。
2. 完成管理身份、API Key、登录和外部账号 API。
3. 完成内部认证和租约接口。
4. 在 `socks5-core` 引入独立认证提供器与租约生命周期。
5. 实现账号级 TCP/UDP 共享限速。
6. 在 `proxyService` 中实现账号服务监督和模式切换。
7. 集成 SprakCapture 设置页。
8. 实现账号服务内嵌远程管理页面。
9. 完成跨进程、数据面和浏览器端到端验证。

## 22. 完成标准

设计实现完成必须同时满足：

- 多账号关闭时当前单账号功能和控制契约无回归。
- 多账号开启时账号数据库只有 `accountService` 访问。
- 管理页面、外部 API、内部接口和 SOCKS5 数据面边界清晰。
- 账号密码、管理密码和 API Key 不以明文形式持久化或出现在诊断输出中。
- 最大连接数、最大在线 IP、到期时间和零值禁用在并发情况下严格生效。
- 同一账号全部连接的上下行总速率符合配置。
- 服务异常、重启和模式切换没有遗留租约、进程或错误认证回退。
- 设计中的数据库、API、生命周期和限速测试全部通过。
