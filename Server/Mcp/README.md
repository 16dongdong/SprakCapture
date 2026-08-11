# Sprak Capture MCP Server

`capture-mcp` 使用官方 `rmcp 3.1`，同时提供工具内置的 Streamable HTTP 传输和独立
stdio 入口，协议内 server 名为 `capture`。它只适配
`proxyService` 已公开的 HTTP 控制契约，所有操作与 Web/Desktop UI 等价，不增加权限
中间件或隐藏路由。

## 构建与运行

### 工具内置服务

打开“设置 → MCP 集成”，选择端口并启用。配置会写入安装目录 `data/configuration.json`，
运行时只监听 `127.0.0.1`，界面显示的 `http://127.0.0.1:PORT/mcp` 即客户端连接地址。
切换开关或端口不会重启代理、WinDivert 或录制服务。

### 独立 stdio 入口

先启动控制服务：

```powershell
cargo run -p proxy-backend
```

另一个终端启动 MCP：

```powershell
cargo run -p capture-mcp
```

环境变量：

| 变量 | 默认值 | 作用 |
|---|---|---|
| `CAPTURE_CONTROL_BASE` | `http://127.0.0.1:17890` | `proxyService` HTTP 控制 API 基址 |
| `CAPTURE_LOCALE` | `en` | tool 描述和自身错误的默认语言 |

每个 tool 还接受可选 `locale`，优先级高于 `CAPTURE_LOCALE`。支持
`en`、`zh-Hans`、`zh-Hant`、`ja`、`ko`、`es`、`fr`、`de`、`pt-BR`、`ru`。

## Tools

| Tool | 控制 API |
|---|---|
| `capture_service_get_snapshot` | `GET /api/v1/snapshot` |
| `capture_service_start` | `POST /api/v1/service/start` |
| `capture_service_stop` | `POST /api/v1/service/stop` |
| `capture_config_get` | `GET /api/v1/snapshot` 的 `configuration` |
| `capture_config_update` | `PUT /api/v1/configuration` |
| `capture_sessions_clear_finished` | `DELETE /api/v1/sessions` |
| `capture_recording_get` | `GET /api/v1/recording` |
| `capture_recording_update` | `PUT /api/v1/recording` |
| `capture_recording_clear` | `POST /api/v1/recording/clear` |
| `capture_transaction_list` | `GET /api/v1/transactions?offset=&limit=&collectionToken=` |
| `capture_transaction_get` | `GET /api/v1/transactions/{transactionId}` |
| `capture_transaction_get_body` | `GET /api/v1/transactions/{transactionId}/{side}/body` |

控制 API 的结构化非成功响应会成为 MCP tool error，并原样保留允许的 `code`、
`messageKey`、本地化 `message` 与 `params`。无效响应只返回状态码、内容类型、
内容长度和完整正文摘要等有界元数据，原始正文不会进入 MCP 结果。控制基址当前仅接受
本机回环主机上的 HTTP；该边界与 `proxyService` 的现有控制协议保持一致。

`capture_transaction_list` 原样返回有界 `TransactionPage`。第一页省略
`collectionToken`，保存响应中的不透明令牌，并在同一次分页遍历的后续请求中原样携带。
录制中的尾部追加不会改变令牌或既有 offset；FIFO 淘汰或 clear 才会使分页代际失效。
若控制 API 返回 HTTP 409、`code=transactionsCollectionChanged`，丢弃已经累积的旧页，
重新请求不带令牌的第一页并保存新令牌。

需要完整顺序遍历时，第一页显式传 `offset=0`，后续只使用响应的 `nextOffset`；
4 MiB 序列化预算可能使实际返回条数小于请求 `limit`，因此不能用 `offset + limit`
推算下一页。

`truncated` 表示当前集合未覆盖完整事务范围或受序列化预算影响；`itemsTruncated`
只表示一条或多条摘要中的自由文本字段为满足集合预算而缩短。录制快照中的
`totalMetadataBytes` 不应超过 `metadataMemoryBudgetBytes`；事务的
`flags.headersTruncated=true` 表示已保存的请求头或响应头因元数据预算而不完整。
这两种状态都不表示正文被截断，正文状态应读取 `capture_transaction_get_body`
返回的 `meta.truncated`。

## Claude Desktop

先构建发布二进制：

```powershell
cargo build -p capture-mcp --release
```

将实际绝对路径写入 Claude Desktop 配置：

```json
{
  "mcpServers": {
    "capture": {
      "command": "D:\\path\\to\\Sprak Capture\\target\\release\\captureMcp.exe",
      "args": [],
      "env": {
        "CAPTURE_CONTROL_BASE": "http://127.0.0.1:17890",
        "CAPTURE_LOCALE": "zh-Hans"
      }
    }
  }
}
```

## Cursor

`.cursor/mcp.json` 使用同一 stdio 配置：

```json
{
  "mcpServers": {
    "capture": {
      "command": "D:\\path\\to\\Sprak Capture\\target\\release\\captureMcp.exe",
      "env": {
        "CAPTURE_CONTROL_BASE": "http://127.0.0.1:17890",
        "CAPTURE_LOCALE": "en"
      }
    }
  }
}
```

## Inspector

```powershell
npx -y @modelcontextprotocol/inspector target\debug\captureMcp.exe
```

Inspector 的 `tools/list` 应只返回上表十二项。清除已结束会话和清空录制均属于破坏性
操作，确认要求分别位于 `Server/Skill/references/service-and-config.md` 与
`recording-and-transactions.md`，MCP Server 本身不设置拒绝围栏。
