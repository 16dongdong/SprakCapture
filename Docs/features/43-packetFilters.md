# 封包滤镜

## 目标

“工具 → 封包滤镜”提供 WPE 风格的有序字节规则：按连接元数据与十六进制模式匹配最终线上块，
然后替换命中字节段、丢弃当前块或关闭连接。规则与其它工具一起写入数据目录的
`configuration.json`，保存后原子热更新，现有连接从下一块字节开始使用新快照。

## 数据面位置

执行顺序固定为：

```text
SOCKS5 TCP/UDP → legacy hook → 完整 Mod 阶段 ┐
                                               ├→ PluginHost 最终写线入口 → 封包滤镜 → 物理适配器
WinDivert TCP 透明代理 / UDP 主动拦截 ────────┘
```

滤镜处理的是即将写入客户端或服务器的真实字节。WinDivert 的透明 TCP 进入融合代理；WinDivert
UDP 由主动 NETWORK 句柄拦截，解析完整数据报后也调用同一个 `PluginHost::processFinalWireBytes`。
SOCKS5 与 WinDivert 不维护两份规则解释器：两者读取同一份原子热更新快照，只在最终物理发送处
分别使用 `UdpSocket` 或 WinDivert 回注。WinDivert 修改可保持或缩短正文，IPv4/IPv6 与 UDP 长度、
分片末片标志和校验和会在唯一回注前重算；分片数据报会先有界重组，再把修改映射回原分片边界。
`drop`/UDP `close` 不回注当前
数据报，未命中与处理器未启用时按原字节回注。

TCP 读取块不是应用协议帧；本滤镜只修改当前已收到的块，不跨块缓存。跨块分包、解密、协议校验和
与有状态重封包仍使用插件/Mod 阶段。

## 规则契约

- 范围：`transport`（any/tcp/udp）、`direction`（any/up/down）、目标 `host`、`port`、
  `minimumLength`、`maximumLength`。
- 搜索：空格分隔的十六进制字节；网格空洞持久化为 `??`，匹配任意单字节。
- 替换：只对 `modify` 有效，长度独立；空网格不输出字节，显式 `??` 才保留原位置字节。
  实际匹配跨度取搜索跨度与替换长度的较大值，因此较长替换会把搜索尾部自动视为通配条件。
- 动作：`modify`、`drop`、`close`。
- 顺序：`replaceAll` 处理当前块内全部不重叠命中；`continueMatching` 让修改后的字节继续进入
  下一条规则，否则首条命中结束本轮处理。

示例：

```json
{
  "enabled": true,
  "rules": [
    {
      "id": "replaceHandshakeMarker",
      "name": "替换握手标记",
      "enabled": true,
      "transport": "tcp",
      "direction": "up",
      "host": "*.example.com",
      "port": 443,
      "minimumLength": 4,
      "maximumLength": 4096,
      "pattern": "01 00 ?? 03 00",
      "replacement": "01 00 06 03 00 03 03",
      "action": "modify",
      "replaceAll": true,
      "continueMatching": false
    }
  ]
}
```

配置上限为 256 条规则、搜索与替换各 512 个字节令牌、单块长度条件最大 16 MiB。网格固定提供
`0000–01FF` 共 512 个可编辑偏移；任一规则非法会拒绝
整份更新，磁盘配置与旧运行快照均保持不变。

网格中的未填写单元格始终显示为空白。位于两个已填写字节之间的空格会在持久化时编码为内部
通配占位，用于保留搜索或替换偏移；该内部表示不会显示给规则作者，也不会把后续字节向前压缩。

## UI 操作指南

1. 在顶栏打开“工具 → 封包滤镜”，进入独立 L3 工具窗口。
2. 打开总开关，点击“添加滤镜”，在 L4 对话框填写传输、方向、目标、长度、搜索模式和动作。
3. 对修改动作独立填写替换行；未填写的格子不会生成 `??`，按需要开启“替换全部”或“继续后续规则”。
4. 在列表中启停、编辑、删除或上下移动规则；点击外层“应用”后立即热更新，不重启服务。

控制 API：`GET/PUT /api/v1/tools/packetFilters`。

MCP：`capture_tool_packet_filters_get`、`capture_tool_packet_filters_update`。
