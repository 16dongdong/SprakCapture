# 12 带宽限制（Throttling）

## Charles 对照

Charles Throttling 模拟带宽、延迟、稳定性、MTU 等，可整体启用并按预设（3G/4G 等）或自定义。可对选中 Location 生效。

## 目标

- 请求/响应方向可配置速率（字节/秒）、固定延迟、抖动、随机丢包（可选）。
- 全局开关 + 可选 Location 条件。
- 提供常用预设；自定义持久化到用户配置。

## 非目标

- 不模拟完整无线链路层。
- 不做多队列 QoS 优先级。

## 领域模型

```typescript
interface ThrottlePreset {
  id: string;
  name: string; // 如 "4G"、"3G"、"56kbps Modem"
  downloadBytesPerSecond: number;
  uploadBytesPerSecond: number;
  latencyMilliseconds: number;
  latencyJitterMilliseconds: number;
  reliabilityPercent: number; // 100 = 不丢包
  mtu: number;
}

interface ThrottlingConfiguration {
  enabled: boolean;
  /** null = 使用 custom */
  activePresetId: string | null;
  custom: Omit<ThrottlePreset, "id" | "name">;
  /** 空 = 全部流量 */
  locations: Location[];
}

interface ThrottlingPublicState extends ThrottlingConfiguration {
  presets: ThrottlePreset[]; // 内置 + 用户
}
```

内置预设示例：

| id | download | upload | latency |
|---|---|---|---|
| `lte` | 12 MB/s | 3 MB/s | 50ms |
| `3g` | 400 KB/s | 100 KB/s | 200ms |
| `edge` | 40 KB/s | 20 KB/s | 400ms |

## 行为

1. 流水线 `throttling` 钩子在实际转发读写路径上对拷贝令牌桶限速。
2. 延迟：在首包或连接建立后 sleep（实现：每方向调度器）。
3. reliability&lt;100：随机失败写或丢弃代理缓冲（需记录 flags.throttled）。
4. 未命中 locations 时跳过。
5. 对 Map Local 合成响应同样可限速，便于测 UI 加载。

## 控制 API

```http
GET /api/v1/tools/throttling
PUT /api/v1/tools/throttling
```

运行中可更新；立即影响新读写。

## UI 要点

- 工具栏闪电图标开关。
- 设置页：预设下拉、自定义滑条、Location 表。
- 事务标志「限速」。

## UI 操作指南

### 界面位置

| 总开关 | L1 顶栏乌龟 |
| 详细配置 | L3：**代理 → 带宽限制设置…** 或 **工具** 等价项 |
| 快捷 | 顶栏节流按钮 **右键** → 打开设置对话框 |

### 如何打开

菜单打开 L3；日常只用顶栏开关。

### 操作步骤

1. **带宽限制设置** 对话框中选预设或自定义上下行/延迟。
2. 应用/确定。
3. 顶栏打开节流开关。
4. L2 观察耗时变长。

### 预期行为

改速率必须进对话框；顶栏不提供滑块，保持壳层干净。


## 验收标准

- [ ] 启用极低速率时大文件下载耗时显著增加。
- [ ] 关闭后吞吐恢复。
- [ ] Location 过滤只影响匹配主机。
- [x] 配置写入统一 `configuration.json` 并在重启后恢复。

## 交叉链接

- [02](02-platformArchitecture.md) · [03](03-locationMatching.md) · [09](09-httpHttpsProxy.md)
