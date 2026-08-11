# 配置

## 服务配置

```json
{
  "listenHost": "127.0.0.1",
  "listenPort": 1080,
  "authenticationMode": "none",
  "credentials": null,
  "maxConnections": 1024,
  "connectTimeout": 10,
  "bindTimeout": 30,
  "idleTimeout": 300,
  "shutdownTimeout": 5,
  "readTimeout": 10,
  "relayBufferSize": 65536,
  "udpBindHost": "",
  "udpMaxPacketSize": 65507
}
```

## 约束

- `listenHost` 必须解析为本机可绑定地址。
- `listenPort` 范围为 `1..65535`；测试可直接向库传入端口零。
- `authenticationMode` 为 `none` 或 `password`。
- 密码认证必须同时提供非空用户名和口令，单字段最大 255 字节。
- `maxConnections` 范围为 `1..16384`。
- 所有超时必须是有限正数；`shutdownTimeout` 最大为 30 秒。
- `relayBufferSize` 范围为 `1024..1048576` 字节。
- 自动 SOCKS5 正文镜像保留每个方向的完整字节流和分片索引，不提供截断配置。
- `maxConnections × relayBufferSize × 2` 不得超过 448 MiB；这是转发工作缓冲预算，不限制录制正文。
- `udpMaxPacketSize` 范围为 `512..65507`。
- 非空 `udpBindHost` 必须是与 `listenHost` 同地址族的 IP 地址。
- 数据面内部将单关联远端地址数限制为 4096、跨周期会话历史限制为 10000。
- 未知字段和越界数值直接导致配置更新失败。

运行配置在每次更新后原子写入安装目录下的 `data/configuration.json`，后台重启时自动恢复；安装到非系统盘时不会再回写 C 盘用户目录。首次升级且安装目录尚无配置时，后台会把旧版用户数据目录整体迁入 `data`。`CAPTURE_USER_DATA_DIR` 仅用于开发、便携部署或隔离测试时显式覆盖数据根。该文件同时保存代理监听与认证、二级代理、进程路径、录制暂停状态与忽略规则、SSL 主机范围、封包滤镜、映射/重写/断点/限速等全部工具规则、反向代理、端口转发和协议查看器设置。证书私钥、插件包、插件自定义配置、映射文件与 Protobuf 描述符原始字节仍保存在安装目录 `data` 下各自受控子目录；统一配置文件只保存适合集中管理的非敏感索引。服务运行时应用监听配置会先有序停止旧数据面，再以新配置重新启动；录制、进程选择和规则型设置直接热更新。

录制事务、正文、运行计数、当前连接、负载测试任务和编辑中的请求草稿属于运行数据，不作为设置恢复。界面语言、工具栏顺序和分栏尺寸属于单个桌面窗口的视图偏好，由前端本地存储持久化，不进入后台配置文件。

## SSL 代理配置

SSL 配置通过 `GET/PUT /api/v1/ssl` 独立管理，可在数据面运行时更新。包含与排除规则
使用统一 Location 对象；排除优先，空包含列表表示不解密任何主机。叶证书缓存上限为
`1..4096`，默认使用客户端 SNI 生成 SAN。

根证书和私钥保存在安装目录 `data/certs` 子目录。根证书跨进程重启保持唯一，私钥不
进入控制响应、日志或 MCP。公开根证书使用 `/api/v1/ssl/ca/export` 导出；更换根证书
必须通过 `/api/v1/ssl/ca/generate`，并重新在客户端建立信任。

## 控制地址

后台控制接口默认使用 `127.0.0.1:17890`。可通过环境变量
`PROXY_CONTROL_ADDRESS` 覆盖完整地址，覆盖值仍必须是回环地址；当前不提供控制地址
命令行参数。
