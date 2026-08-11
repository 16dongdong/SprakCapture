# 30 导入导出

## Charles 对照

Charles 支持导出/导入会话（`.chls` 等）、HAR、CSV 等，便于分享与二次分析。

## 目标

- P1：导出 **HAR 1.2** 与 **Native JSON**（Sprak Capture 会话格式）。
- P1：导入 HAR 与 Native 为只读或合并入当前录制（策略可选）。
- 导出可选是否包含正文；大会话流式写文件。
- P3：Charles XML / chls 兼容（尽力）。

## 非目标

- 不保证与 Charles 专有二进制格式 100% 兼容。
- 不导出认证口令或 CA 私钥。

## 领域模型

```typescript
type ExportFormat = "har" | "native" | "csvMetadata";

interface ExportRequest {
  format: ExportFormat;
  includeBodies: boolean;
  /** 空 = 当前全部事务 */
  transactionIds?: string[];
  /** 服务端写用户目录或返回下载流 */
  destination: "download" | "path";
  path?: string;
}

interface ExportResult {
  format: ExportFormat;
  bytesWritten: number;
  transactionCount: number;
  path?: string;
  /** download 时可用一次性 token */
  downloadToken?: string;
}

interface NativeSessionFile {
  format: "capture-session";
  version: 1;
  exportedAtMilliseconds: number;
  recording: RecordingSnapshot;
  transactions: TransactionSummary[];
  /** 正文 map；key = `${transactionId}:request|response` */
  bodies?: Record<string, { contentType: string; base64: string; truncated: boolean }>;
}

interface ImportRequest {
  format: ExportFormat | "har";
  mode: "replace" | "merge";
  /** upload 或 path */
  source: "upload" | "path";
  path?: string;
}
```

### HAR 映射要点

| HAR | Sprak Capture |
|---|---|
| `log.entries[].request` | method/url/headers/body |
| `log.entries[].response` | status/headers/body |
| `timings` | 由 TransactionTimings 换算 |
| `_capture` 扩展字段 | flags、notes 等 |

## 行为

1. 导出 HAR：符合 HAR 1.2；二进制 body 用 base64 text + encoding。
2. Native：完整往返可恢复 notes/flags。
3. 导入 replace：清空当前录制再载入；merge：生成新 ID 避免冲突。
4. CSV 仅元数据列，无 body。
5. 路径必须在用户允许目录内（防穿越）。

## 控制 API

```http
POST /api/v1/recording/export
POST /api/v1/recording/import
GET  /api/v1/recording/export/download/{token}
```

## UI 要点

- 菜单：导出 HAR、导出会话、导入。
- 进度条（大文件）。
- 导入前确认 replace 破坏性。

## UI 操作指南

### 界面位置

| 导出 | **文件 → 导出会话…**；顶栏 **导出…** → L3 |
| 导入 | **文件 → 导入会话…** → L3 |
| 选中导出 | 结构报文多选右键 **导出选中…** |

### 如何打开

文件菜单或顶栏带 `…` 的导出按钮（明示对话框）。

### 操作步骤

1. 导出对话框：范围、格式 → 系统保存路径（L4）→ 完成 Toast。
2. 导入对话框：选文件 → 替换或只读 → 确定后 L2 展示。

### 预期行为

导入导出不占用 L2 整页向导（除非文件极大需进度对话框）。


## 验收标准

- [ ] 导出 HAR 可被 Chrome DevTools / 在线 HAR 查看器打开。
- [ ] Native 导出再导入后事务数与 notes 一致。
- [ ] includeBodies=false 时 HAR 无 postData/content text。
- [ ] 非法文件返回中文错误且不损坏当前会话。

## 交叉链接

- [04](04-sessionAndRecording.md) · [05](05-transactionModel.md) · [25](25-autoSave.md)
- [34](34-implementationRoadmap.md)
