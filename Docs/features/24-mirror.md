# 24 镜像（Mirror）

## Charles 对照

Mirror 将请求/响应内容实时写入磁盘目录树（按 host/path 组织），便于离线对比或给其它工具消费。

## 目标

- 启用后将匹配事务的请求与/或响应写入用户指定根目录。
- 目录结构：`{root}/{host}/{path...}` 或扁平 `timestamp_id_side`。
- 异步写盘，不阻塞转发热路径（有界队列）。

## 非目标

- 不做双向同步云盘。
- 不做差分 UI。

## 领域模型

```typescript
interface MirrorConfiguration {
  enabled: boolean;
  rootDirectory: string;
  locations: Location[];
  mirrorRequest: boolean;
  mirrorResponse: boolean;
  /** hierarchical | flat */
  layout: "hierarchical" | "flat";
  /** 队列满时 drop 或 block；默认 drop 并计数 */
  onOverflow: "drop" | "block";
  maxQueueLength: number;
}

interface MirrorPublicState extends MirrorConfiguration {
  writtenFiles: number;
  droppedWrites: number;
  lastError: string | null;
}
```

## 行为

1. 响应钩子末段（capture 前后）投递写任务。
2. hierarchical：安全化 path 段，禁止穿越。
3. 文件内容：可选 raw 或「headers + body」合并文本。
4. 服务停止时 flush 队列（有超时）。

## 控制 API

```http
GET /api/v1/tools/mirror
PUT /api/v1/tools/mirror
```

## UI 要点

- 目录选择（Desktop）、开关、布局、Location。
- 显示写入计数与错误。

## UI 操作指南

### 界面位置

**工具 → 镜像…** L3。

### 如何打开

菜单。

### 操作步骤

1. 启用 → 选择根目录（L4 文件夹对话框）→ Location 可选。
2. 确定后浏览站点；磁盘出现镜像树。

### 预期行为

目录选择用系统 L4，不嵌在主窗口侧栏。


## 验收标准

- [ ] 启用后磁盘出现对应文件且内容匹配。
- [ ] 路径穿越被拒绝。
- [ ] 高并发下代理延迟不明显（drop 策略）。
- [ ] 关闭后停止新增文件。

## 交叉链接

- [25](25-autoSave.md) · [05](05-transactionModel.md) · [02](02-platformArchitecture.md)
