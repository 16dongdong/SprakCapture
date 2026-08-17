# 术语

| 术语 | 定义 | 代码边界 |
|---|---|---|
| SOCKS5 控制连接 | 客户端完成方法协商、认证并提交一个命令的 TCP 连接 | `Backend/Socks5` |
| 数据中继 | `CONNECT`、`BIND` 或 `UDP ASSOCIATE` 建立的双向负载传输 | `Backend/Socks5` |
| 唯一服务入口 | 配置中唯一公开的 SOCKS5 TCP 监听地址 | 后台服务配置 |
| 控制接口 | 仅绑定回环地址的 HTTP 与 WebSocket 管理端点 | `Backend/src` |
| 权威快照 | 后台在指定修订号下发布的服务、指标、配置和会话状态 | 前后端控制契约 |
| 状态动作 | 根据权威服务状态执行启动或停止的单一界面动作 | `Frontend/Web` |
| 悬浮窗口 | 显示精简状态与状态动作的第二个 Tauri 窗口 | `Frontend/Desktop` |
| 有序关闭 | 先停止接收新连接，再释放活动会话和监听资源的关闭过程 | 后台运行时 |
| Structure／结构 | 唯一事务导航视图；按 `host:port` 分组并直接展示真实报文 | Web 事务工作区 |
| 录制会话 RecordingSession | 后续网络分析功能中的抓包容器，与当前 SOCKS5 会话分离 | 路线图 `features/04-sessionAndRecording.md` |
| 事务 Transaction | 后续 HTTP 分析功能中的请求响应单元 | 路线图 `features/05-transactionModel.md` |
| 位置 Location | 后续规则系统共用的协议、主机、端口和路径匹配条件 | 路线图 `features/03-locationMatching.md` |
| 工具流水线 | 后续请求响应路径上顺序固定的可启用工具集合 | 路线图 `features/02-platformArchitecture.md` |
