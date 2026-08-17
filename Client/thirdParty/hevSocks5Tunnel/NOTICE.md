# HEV SOCKS5 Tunnel 来源

- 上游：https://github.com/heiher/hev-socks5-tunnel
- 固定版本：2.9.3
- 固定提交：`b9b9b7b9b0febe32bb5d8cdb9ffa414d94242b75`
- 许可：MIT

项目把固定上游源码和依赖放在 `app/src/main/cpp/vendor/hev-socks5-tunnel`，构建期间不访问
网络。本地修改在 lwIP 会话仍保留完整五元组的位置增加应用作用域分类，并增加确定的启动与
运行错误状态；最终 APK 从该源码构建双 ABI，不再携带历史预编译 SO。
