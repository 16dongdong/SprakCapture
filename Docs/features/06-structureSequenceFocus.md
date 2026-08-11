# 06 结构导航与聚焦

## Charles / 浏览器网络面板对照

连接工作台保留 Charles 的端点结构感，同时采用浏览器开发者工具的报文识别方式：

- 一级按真实 `host:port` 分组。
- 二级直接显示报文，不生成没有协议含义的“根路径”目录。
- 报文行显示资源类型图标、方法、真实位置、大小和状态。
- 新事务或已有事务字节变化时亮起，随后自动消散。
- Focus 只过滤当前视图，不删除录制数据。

## 目标

- Web 连接工作台只保留一个结构视图，数据源绑定 `TransactionSummary`。
- 删除序列视图、切换状态和对应虚拟列表。
- Focus 为客户端过滤，与服务端录制忽略分离。
- 保持无翻页、分隔条和响应式断点行为。

## 结构模型

```typescript
interface EndpointGroup {
  /** host 与实际端口组成的稳定键 */
  key: string;
  endpoint: string;
  transactions: TransactionSummary[];
}

interface FocusState {
  /** 空表示显示全部 */
  pinnedHosts: string[];
}
```

## 行为

1. 默认 HTTP/WS 端口 `80` 和 HTTPS/WSS 端口 `443` 只在展示时隐藏；分组键始终包含实际端口。
2. 每个端点默认展开；端点下有新流量时自动展开，使报文高亮可见。
3. HTTP 位置显示 `path?query`；SOCKS 显示 `socks5://host:port`，不伪造 URL path。
4. 资源类型优先依据 MIME，再使用扩展名；SOCKS 显示“原始流”。
5. 流量签名覆盖两侧头/正文字节、状态和终态时间；变化后高亮 `2.8s`，重复变化重置计时。
6. 搜索、状态和聚焦均只过滤当前有界摘要页。

## UI 要点

- 左栏顶部为只读“结构”标题，不再使用单项页签伪装切换。
- 报文位置不使用省略号；宽度不足时由结构区水平滚动保留全文。
- 图标和类型文本同时存在，不只依靠颜色表达资源类别。
- `prefers-reduced-motion` 下保持静态亮起至计时结束，不执行渐变动画。

## 验收标准

- [ ] 页面不存在“序列”切换入口和“根路径”节点。
- [ ] 同一主机不同端口不合并。
- [ ] 图片、脚本、样式、JSON、字体、媒体和原始流具有不同图标或类型标识。
- [ ] 新增或发生字节变化的 URL 报文亮起并自动消散。
- [ ] Focus 关闭后恢复全部事务。
- [ ] 窄屏上下分栏仍可完整浏览结构与检查器。

## 交叉链接

- [05](05-transactionModel.md) · [03](03-locationMatching.md) · [07](07-requestResponseViewers.md)
- [08](08-chartsAndOverview.md) · [frontendArchitecture](../frontendArchitecture.md)
