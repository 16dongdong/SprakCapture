# 31 Protobuf / AMF

## Charles 对照

Charles 可在配置 `.proto` 描述后解码 gRPC/Protobuf 体；旧版支持 AMF（Flash）视图。

## 目标

- P3：Protobuf 查看器——用户注册文件描述符集（FileDescriptorSet）或 `.proto` 编译产物。
- 按 Content-Type / 路径规则选择 message 类型解码为 JSON 树。
- AMF0/AMF3 为可选、低优先级；无业务需求时可仅占位开关默认关。

## 非目标

- 不做在线从未知二进制自动推断完整 schema。
- 不实现完整 gRPC 流式多消息编辑器一版（先 unary body）。

## 领域模型

```typescript
interface ProtobufSchemaEntry {
  id: string;
  name: string;
  /** 用户目录内描述符路径 */
  descriptorPath: string;
  /** 包名.Message */
  defaultMessageType: string;
}

interface ProtobufRoute {
  id: string;
  location: Location;
  messageType: string; // 请求
  responseMessageType?: string;
  schemaId: string;
}

interface ProtobufConfiguration {
  enabled: boolean;
  schemas: ProtobufSchemaEntry[];
  routes: ProtobufRoute[];
}

interface AmfConfiguration {
  enabled: boolean;
  locations: Location[];
}

interface DecodedProtobufView {
  messageType: string;
  json: unknown;
  decodeError?: string;
}
```

## 行为

1. Contents 页签在命中 route 或 `application/x-protobuf` / `application/grpc` 时尝试解码。
2. gRPC 帧：剥离 5 字节长度前缀与压缩标志（支持 identity；gzip 可选）。
3. 解码失败显示 Hex + 错误。
4. 断点编辑 P3 后期：JSON → 重新编码（需 schema）。

## 控制 API

```http
GET  /api/v1/tools/protobuf
PUT  /api/v1/tools/protobuf
POST /api/v1/tools/protobuf/schemas  // 上传 descriptor
GET  /api/v1/transactions/{id}/decode/protobuf
```

## UI 要点

- 设置：schema 列表、路由表。
- 查看器 Protobuf 页签：JSON 树。
- AMF 页签仅 enabled 时出现。

## UI 操作指南

### 界面位置

L2 检查器正文子视图；描述符注册可用 L3 小对话框 **选择描述符…**。

### 如何打开

选中 protobuf 事务自动出现子视图；缺描述符时点按钮打开文件对话框。

### 操作步骤

绑定 .desc → 树状查看；编辑并重复时在编辑对话框内用结构化编辑器。

### 预期行为

解码在检查器；配置描述符不进代理设置大杂烩。


## 验收标准

- [ ] 已知 proto 的 unary 响应可显示字段树。
- [ ] 无 schema 时不崩溃，回退 Hex。
- [ ] schema 文件不进 git；仅用户目录。

## 交叉链接

- [07](07-requestResponseViewers.md) · [05](05-transactionModel.md) · [19](19-breakpoints.md)
