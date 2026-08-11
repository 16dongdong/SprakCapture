# 29 W3C 验证（Validate）

## Charles 对照

Charles Validate 可将响应提交到 W3C 校验服务（HTML/CSS 等）或本地规则，辅助前端质量检查。

## 目标

- P3：对选中事务的响应体执行可插拔校验器。
- 内置：HTML 基础良构检查（可选调用外部 W3C API，需用户显式启用网络）。
- 结果挂到事务 `tags` / 独立校验报告面板。

## 非目标

- 不做完整浏览器一致性测试套件。
- 默认不向第三方上传用户流量（必须显式同意）。

## 领域模型

```typescript
type ValidatorId = "htmlWellFormed" | "jsonSchema" | "w3cHtmlOnline";

interface ValidateConfiguration {
  enabled: boolean;
  validators: Array<{
    id: ValidatorId;
    enabled: boolean;
  }>;
  /** 在线校验需用户确认 */
  allowOnlineValidators: boolean;
  w3cEndpoint: string; // 默认官方或自建
}

interface ValidationIssue {
  severity: "info" | "warning" | "error";
  message: string;
  line?: number;
  column?: number;
}

interface ValidationReport {
  transactionId: string;
  validatorId: ValidatorId;
  issues: ValidationIssue[];
  validatedAtMilliseconds: number;
}
```

## 行为

1. 用户对某事务执行「验证」或批量对过滤结果验证。
2. 仅文本类 Content-Type 进入校验。
3. 在线校验：先弹确认；请求体最小化（可仅 HTML 片段策略）。
4. 报告不进入列表快照正文，按需 `GET`。

## 控制 API

```http
GET  /api/v1/tools/validate
PUT  /api/v1/tools/validate
POST /api/v1/transactions/{id}/validate
GET  /api/v1/transactions/{id}/validation
```

## UI 要点

- 检查器「验证」页签。
- 问题列表可点击定位（若有行号）。
- 设置中关闭在线校验默认关。

## UI 操作指南

### 界面位置

右键 **验证响应…** → L3；结果可在对话框或 L2 检查器附件区。

### 如何打开

选中响应事务 → 验证响应…。

### 操作步骤

1. L4/对话框确认是否上传。
2. 查看错误列表；点行号聚焦 L2 响应 Text 视图。

### 预期行为

外传必须确认；取消则完全不请求外网。


## 验收标准

- [ ] 非法 HTML 片段产生 error 级问题。
- [ ] allowOnlineValidators=false 时拒绝在线校验。
- [ ] 二进制响应提示不支持。

## 交叉链接

- [07](07-requestResponseViewers.md) · [05](05-transactionModel.md) · [34](34-implementationRoadmap.md)
