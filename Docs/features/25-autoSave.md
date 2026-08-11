# 25 自动保存（Auto Save）

## Charles 对照

Auto Save 按时间间隔或会话大小自动把当前录制会话保存到文件，防止崩溃丢数据。

## 目标

- 定时与/或按事务数阈值将 active RecordingSession 导出到目录。
- 文件名含时间戳；可限制保留份数（轮转）。
- 格式默认 Native JSON（见 [30](30-importExport.md)），可选 HAR。

## 非目标

- 不做实时协作云同步。
- 不在源码目录保存。

## 领域模型

```typescript
interface AutoSaveConfiguration {
  enabled: boolean;
  directory: string;
  intervalSeconds: number; // 0 = 仅按阈值
  everyNTransactions: number; // 0 = 仅按时间
  format: "native" | "har";
  maxFiles: number; // 轮转
  includeBodies: boolean;
}

interface AutoSavePublicState extends AutoSaveConfiguration {
  lastSavedAtMilliseconds: number | null;
  lastSavedPath: string | null;
  lastError: string | null;
}
```

## 行为

1. 后台任务满足间隔或计数触发导出。
2. 导出中不阻塞数据面；使用快照一致性 revision。
3. 超过 maxFiles 删除最旧。
4. 与 Mirror 独立（Mirror 按请求文件；Auto Save 整会话）。

## 控制 API

```http
GET /api/v1/tools/autoSave
PUT /api/v1/tools/autoSave
POST /api/v1/tools/autoSave/saveNow
```

## UI 要点

- 目录、间隔、格式、保留数。
- 最近保存路径可点击打开目录（Desktop）。

## UI 操作指南

### 界面位置

**工具 → 自动保存…** L3。

### 如何打开

菜单。

### 操作步骤

1. 启用 → 间隔、目录、格式、对齐整点。
2. 确定；到点后 Toast，L2 会话按配置清空。

### 预期行为

仅对话框配置；无顶栏常驻入口（避免壳层拥挤）。


## 验收标准

- [ ] 达间隔后目录新增文件。
- [ ] 轮转删除超额旧文件。
- [ ] includeBodies=false 时文件无正文。
- [ ] 失败写入 lastError 且不崩溃服务。

## 交叉链接

- [04](04-sessionAndRecording.md) · [30](30-importExport.md) · [24](24-mirror.md)
