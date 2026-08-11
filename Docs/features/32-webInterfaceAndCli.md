# 32 网页控制与 CLI

## Charles 对照

Charles 提供 Web Interface（远程控制/查看）与命令行启动参数；便于自动化与无 UI 场景。

## 目标

- P3：强化现有回环控制 API 为「无头可用」的完整 CLI。
- 可选：控制面绑定配置化（仍默认回环；非回环需显式危险开关 + token）。
- CLI 子命令：start/stop、export、tool 开关、ssl 导出。
- 唯一 Web UI 仍服务本机；不做第二套管理 UI。

## 非目标

- 不做多租户 SaaS 控制面。
- 不在公网默认暴露未认证控制口。

## 领域模型

```typescript
interface ControlPlaneConfiguration {
  bindHost: string; // 默认 127.0.0.1
  bindPort: number; // 默认 17890
  /** 非回环时必填 */
  authToken?: string;
  allowNonLoopback: boolean;
  corsExactOrigins: string[];
}

interface CliCommandExamples {
  start: "capture service start";
  stop: "capture service stop";
  snapshot: "capture snapshot --json";
  exportHar: "capture export --format har --out session.har";
  setTool: "capture tools rewrite --enable";
}
```

## 行为

1. CLI 通过 HTTP 调控制 API（与 UI 同源契约），camelCase JSON。
2. 退出码：0 成功；非 0 + stderr 中文错误。
3. `--json` 稳定输出便于脚本。
4. 非回环：必须 `allowNonLoopback=true` 且 Bearer token；否则拒绝启动控制面。
5. Web UI 静态资源可由 backend 在 production 中托管（可选），路径不与 `/api` 冲突。

## 控制 API

沿用并文档化全部 `/api/v1/*`；新增：

```http
GET /api/v1/health
GET /api/v1/version
```

## UI 要点

- 设置显示控制地址与「仅本机」状态。
- 帮助页：CLI 示例。

## UI 操作指南

### 界面位置

无 GUI 主路径；CLI/HTTP API 为 L0 外自动化。可选 **帮助 → 关于** 中链到本机控制说明（L3 只读）。

### 如何打开

终端或 HTTP 客户端；不与 L2 工作台抢层级。

### 操作步骤

见文档命令示例；与 GUI 对话框改的是同一后端状态。

### 预期行为

headless 不创建菜单，但配置语义与对话框「应用」一致。


## 验收标准

- [ ] 无 Desktop 时 CLI 可 start HTTP 代理并 export HAR。
- [ ] 默认非回环连接 403。
- [ ] token 错误 401。
- [ ] Origin 校验规则与 [controlContract](../controlContract.md) 一致。

## 交叉链接

- [02](02-platformArchitecture.md) · [controlContract](../controlContract.md) · [30](30-importExport.md)
- [00](00-productVision.md)
