# 录制规则集

## 目标

“工具 → 录制规则集”提供有序、可视化且可热更新的流量接纳规则。配置与其它工具一起写入安装目录中的 `data/configuration.json`，关闭窗口、停止服务或重启控制进程后继续恢复。

## 匹配与动作

运行时按“规则集顺序 → 集内规则顺序”执行首条命中：

- 条件：完整域名、域名后缀、域名关键字、目标 IP/CIDR、客户端 IP/CIDR、目标端口/范围、进程名、协议、HTTP 方法、全部流量。
- `record`：正常转发并录制完整事务。
- `doNotRecord`：正常转发但不创建录制事务。
- `reject`：HTTP、HTTPS、CONNECT 和 WinDivert 透明 TCP 在出站前阻断并留下 blocked 事务；WinDivert UDP 已在统一封包数据面完成 WPE 裁决与主动回注，录制规则只决定是否把最终写线数据报投影为事务，不会二次修改或重复发送原包。

规则更新会原子编译并替换共享匹配器，不重启融合监听、WinDivert 或现有连接。配置非法时整份更新被拒绝，旧配置保持生效。

## 配置结构

```json
{
  "enabled": true,
  "defaultAction": "record",
  "ruleSets": [
    {
      "id": "default",
      "name": "默认规则",
      "enabled": true,
      "rules": [
        {
          "id": "rejectAds",
          "enabled": true,
          "kind": "domainSuffix",
          "value": "ads.example",
          "action": "reject"
        }
      ]
    }
  ]
}
```

控制 API：`GET/PUT /api/v1/tools/recordingRules`。
