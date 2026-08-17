import { z } from "zod";

import {
  listenersSnapshotSchema,
  locationPatternSchema,
  metricsSchema,
  processCaptureSnapshotSchema,
  publicConfigurationSchema,
  serverInstanceIdSchema,
  serviceStateSchema,
  sessionSnapshotSchema,
  sslPublicStateSchema,
} from "./protocolCore";
import {
  headerFieldSchema,
  pluginSnapshotSchema,
  recordingSnapshotSchema,
  suspendedBreakpointSchema,
  toolsPublicStateSchema,
} from "./protocolTools";
import {
  maximumDescriptorEncodedCharacters,
  maximumEncodedBodyCharacters,
  maximumRecordingBodyBytes,
  maximumTransactionCollectionTokenCharacters,
  safeUnsignedIntegerSchema,
} from "./protocolShared";

/** 定义事务正文、Protobuf、校验、重复请求与实时事件协议。 */
export const transactionProtocolSchema = z.enum([
  "http",
  "https",
  "ws",
  "wss",
  "tunnel",
  "socks",
]);

export const transactionStatusSchema = z.enum([
  "pending",
  "complete",
  "failed",
  "blocked",
  "cancelled",
]);

export const transactionTimingsSchema = z
  .object({
    startAtMilliseconds: safeUnsignedIntegerSchema,
    dnsEndAtMilliseconds: safeUnsignedIntegerSchema.nullable(),
    connectEndAtMilliseconds: safeUnsignedIntegerSchema.nullable(),
    tlsEndAtMilliseconds: safeUnsignedIntegerSchema.nullable(),
    requestSentAtMilliseconds: safeUnsignedIntegerSchema.nullable(),
    responseStartAtMilliseconds: safeUnsignedIntegerSchema.nullable(),
    endAtMilliseconds: safeUnsignedIntegerSchema.nullable(),
  })
  .strict();

export const transactionSizesSchema = z
  .object({
    requestHeaderBytes: safeUnsignedIntegerSchema,
    requestBodyBytes: safeUnsignedIntegerSchema,
    responseHeaderBytes: safeUnsignedIntegerSchema,
    responseBodyBytes: safeUnsignedIntegerSchema,
  })
  .strict();

export const transactionFlagsSchema = z
  .object({
    mappedLocal: z.boolean(),
    mappedRemote: z.boolean(),
    rewritten: z.boolean(),
    breakpointHit: z.boolean(),
    throttled: z.boolean(),
    mitmDecrypted: z.boolean(),
    bodyTruncated: z.boolean(),
    headersTruncated: z.boolean(),
    fromCache: z.boolean(),
  })
  .strict();

export const transactionErrorSchema = z
  .object({
    code: z.string().min(1),
    messageKey: z.string().min(1),
    params: z.record(z.string(), z.string()),
  })
  .strict();

export const transactionSummarySchema = z
  .object({
    transactionId: z.string().min(1),
    recordingSessionId: z.string().min(1),
    sequence: safeUnsignedIntegerSchema,
    protocol: transactionProtocolSchema,
    method: z.string(),
    host: z.string(),
    port: z.number().int().nonnegative().max(65535),
    path: z.string(),
    query: z.string(),
    urlDisplay: z.string(),
    status: transactionStatusSchema,
    statusCode: z.number().int().nonnegative().max(65535).nullable(),
    clientAddress: z.string(),
    clientProcessName: z.string().nullable(),
    clientProcessId: safeUnsignedIntegerSchema.nullable(),
    contentType: z.string(),
    timings: transactionTimingsSchema,
    sizes: transactionSizesSchema,
    flags: transactionFlagsSchema,
    error: transactionErrorSchema.nullable(),
    notes: z.string(),
    tags: z.array(z.string()),
    appliedTools: z.array(z.string()),
  })
  .strict();

export const transactionPageSchema = z
  .object({
    revision: safeUnsignedIntegerSchema,
    recordingSessionId: z.string().min(1),
    collectionToken: z
      .string()
      .min(1)
      .max(maximumTransactionCollectionTokenCharacters),
    total: safeUnsignedIntegerSchema,
    offset: safeUnsignedIntegerSchema,
    limit: z.number().int().positive().max(1_000),
    hasPrevious: z.boolean(),
    hasMore: z.boolean(),
    nextOffset: safeUnsignedIntegerSchema.nullable(),
    truncated: z.boolean(),
    itemsTruncated: z.boolean(),
    items: z.array(transactionSummarySchema),
  })
  .strict()
  // 分页边界由响应给出，调用方不得根据请求 limit 猜测下一页位置。
  .superRefine((page, context) => {
    const pageFitsCollection =
      page.offset <= page.total &&
      page.items.length <= page.total - page.offset;
    const returnedEnd = pageFitsCollection
      ? page.offset + page.items.length
      : page.total;
    const expectedHasMore = returnedEnd < page.total;
    const expectedNextOffset = expectedHasMore ? returnedEnd : null;
    if (
      !pageFitsCollection ||
      page.items.length > page.limit ||
      page.hasPrevious !== page.offset > 0 ||
      page.hasMore !== expectedHasMore ||
      page.nextOffset !== expectedNextOffset ||
      page.truncated !==
        (page.hasPrevious || page.hasMore || page.itemsTruncated)
    ) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["nextOffset"],
        message: "transaction page boundaries are inconsistent",
      });
    }
    page.items.forEach((transaction, itemIndex) => {
      if (transaction.recordingSessionId !== page.recordingSessionId) {
        context.addIssue({
          code: z.ZodIssueCode.custom,
          path: ["items", itemIndex, "recordingSessionId"],
          message: "transaction belongs to another recording session",
        });
      }
    });
  });

export const messageSideSchema = z.enum(["request", "response"]);

export const bodyHandleMetaSchema = z
  .object({
    transactionId: z.string().min(1),
    side: messageSideSchema,
    contentType: z.string(),
    encoding: z.string(),
    storedBytes: z.number().int().nonnegative().max(maximumRecordingBodyBytes),
    originalBytes: safeUnsignedIntegerSchema,
    truncated: z.boolean(),
  })
  .strict()
  // 原始长度不得小于已存长度，截断标记必须和正文存储事实保持一致。
  .superRefine((meta, context) => {
    if (meta.originalBytes < meta.storedBytes) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["originalBytes"],
        message: "originalBytes is less than storedBytes",
      });
    }
    if (meta.truncated !== meta.originalBytes > meta.storedBytes) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["truncated"],
        message: "truncated does not match body lengths",
      });
    }
  });

/** 描述流中一个可独立查看的有界片段；范围始终指向同侧聚合正文，避免协议重复传输片段正文。 */
export const streamPacketModificationSchema = z
  .object({
    offsetBytes: z.number().int().nonnegative().max(maximumRecordingBodyBytes),
    originalBytes: z.array(z.number().int().min(0).max(255)),
    modifiedBytes: z.array(z.number().int().min(0).max(255)),
  })
  .strict();

export const streamPacketSchema = z
  .object({
    sequence: safeUnsignedIntegerSchema,
    capturedAtMilliseconds: safeUnsignedIntegerSchema,
    storedOffsetBytes: z
      .number()
      .int()
      .nonnegative()
      .max(maximumRecordingBodyBytes),
    storedBytes: z.number().int().positive().max(maximumRecordingBodyBytes),
    originalBytes: safeUnsignedIntegerSchema,
    truncated: z.boolean(),
    action: z.enum(["forward", "replace", "drop", "close"]).default("forward"),
    modifications: z.array(streamPacketModificationSchema).default([]),
  })
  .strict()
  .superRefine((packet, context) => {
    if (
      packet.originalBytes < packet.storedBytes ||
      packet.truncated !== packet.originalBytes > packet.storedBytes
    ) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["originalBytes"],
        message: "stream packet lengths are inconsistent",
      });
    }
    for (const [index, modification] of packet.modifications.entries()) {
      if (
        modification.offsetBytes + modification.modifiedBytes.length >
        packet.storedBytes
      ) {
        context.addIssue({
          code: z.ZodIssueCode.custom,
          path: ["modifications", index],
          message: "stream packet modification is outside stored bytes",
        });
      }
    }
  });

export const transactionDetailSchema = z
  .object({
    revision: safeUnsignedIntegerSchema,
    transaction: transactionSummarySchema,
    requestHeaders: z.array(headerFieldSchema),
    responseHeaders: z.array(headerFieldSchema),
    requestBody: bodyHandleMetaSchema.nullable(),
    responseBody: bodyHandleMetaSchema.nullable(),
    requestPackets: z.array(streamPacketSchema).default([]),
    responsePackets: z.array(streamPacketSchema).default([]),
  })
  .strict()
  // 详情中的正文句柄必须属于当前事务和对应方向，避免选择切换时把旧正文挂到新事务。
  .superRefine((detail, context) => {
    const expectedTransactionId = detail.transaction.transactionId;
    const invalidRequestBody =
      detail.requestBody !== null &&
      (detail.requestBody.transactionId !== expectedTransactionId ||
        detail.requestBody.side !== "request");
    const invalidResponseBody =
      detail.responseBody !== null &&
      (detail.responseBody.transactionId !== expectedTransactionId ||
        detail.responseBody.side !== "response");
    if (invalidRequestBody || invalidResponseBody) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: [invalidRequestBody ? "requestBody" : "responseBody"],
        message: "body handle does not belong to transaction side",
      });
    }
    for (const [side, packets, body] of [
      ["request", detail.requestPackets, detail.requestBody],
      ["response", detail.responsePackets, detail.responseBody],
    ] as const) {
      const invalidPacket = packets.find(
        (packet) =>
          body === null ||
          packet.storedOffsetBytes + packet.storedBytes > body.storedBytes,
      );
      if (invalidPacket !== undefined) {
        context.addIssue({
          code: z.ZodIssueCode.custom,
          path: [side === "request" ? "requestPackets" : "responsePackets"],
          message: "stream packet range is outside body",
        });
      }
    }
  });

/** 描述服务端严格识别得到的应用层派生正文；原始正文仍由外层 base64 字段完整保留。 */
export const decodedBodyResponseSchema = z
  .object({
    algorithm: z.string().min(1).max(128),
    contentType: z.string().min(1).max(512),
    decodedBytes: z.number().int().nonnegative().safe(),
    base64: z
      .string()
      .max(maximumEncodedBodyCharacters)
      .regex(
        /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/,
      ),
  })
  .strict()
  .superRefine((body, context) => {
    const paddingBytes = body.base64.endsWith("==")
      ? 2
      : body.base64.endsWith("=")
        ? 1
        : 0;
    const decodedBytes = (body.base64.length / 4) * 3 - paddingBytes;
    if (decodedBytes !== body.decodedBytes) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["base64"],
        message: "decoded base64 length does not match decodedBytes",
      });
    }
  });

// 正文保持标准 base64 字符串直到查看器明确解码；派生正文可选以兼容不命中识别器的普通事务。
export const encodedBodyResponseSchema = z
  .object({
    revision: safeUnsignedIntegerSchema,
    meta: bodyHandleMetaSchema,
    base64: z
      .string()
      .max(maximumEncodedBodyCharacters)
      .regex(
        /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/,
      ),
    decoded: decodedBodyResponseSchema.nullable().optional(),
  })
  .strict()
  // 只用编码长度验证字节数，不提前解码正文，也不创建与正文等大的第二份缓冲区。
  .superRefine((body, context) => {
    const paddingBytes = body.base64.endsWith("==")
      ? 2
      : body.base64.endsWith("=")
        ? 1
        : 0;
    const decodedBytes = (body.base64.length / 4) * 3 - paddingBytes;
    if (decodedBytes !== body.meta.storedBytes) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["base64"],
        message: "base64 length does not match storedBytes",
      });
    }
  });

/** 描述本机用户目录中已登记的 FileDescriptorSet；字节内容从不进入浏览器状态。 */
export const protobufSchemaEntrySchema = z
  .object({
    id: z.string().uuid(),
    name: z.string().min(1).max(256),
    descriptorPath: z.string().min(1).max(4_096),
    defaultMessageType: z.string().min(1).max(512),
  })
  .strict();

/** 将统一 Location 匹配规则绑定到 Protobuf 请求与响应消息类型。 */
export const protobufRouteSchema = z
  .object({
    id: z.string().min(1).max(128),
    location: locationPatternSchema,
    messageType: z.string().min(1).max(512),
    responseMessageType: z.string().min(1).max(512).nullable(),
    schemaId: z.string().uuid(),
  })
  .strict();

/** 描述 Protobuf 查看器完整配置；描述符上传仍使用专用端点以隔离大字节载荷。 */
export const protobufConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    schemas: z.array(protobufSchemaEntrySchema).max(1_024),
    routes: z.array(protobufRouteSchema).max(2_000),
  })
  .strict();

/** 描述描述符路由的可提交部分；已登记 schema 只能经专用上传接口追加，不允许表单伪造条目。 */
export const protobufConfigurationUpdateSchema = z
  .object({
    enabled: z.boolean(),
    routes: z.array(protobufRouteSchema).max(2_000),
  })
  .strict();

/** 承载用户通过文件选择器提供的 FileDescriptorSet；Base64 仅在提交期间驻留浏览器内存。 */
export const protobufDescriptorUploadSchema = z
  .object({
    name: z.string().trim().min(1).max(256),
    defaultMessageType: z.string().trim().min(1).max(512),
    base64: z
      .string()
      .min(1)
      .max(maximumDescriptorEncodedCharacters)
      .regex(
        /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/,
      ),
  })
  .strict();

/** 表示解码器的稳定回退结果；失败只提供机器错误码，正文继续由 Hex 查看器展示。 */
export const decodedProtobufViewSchema = z
  .object({
    messageType: z.string().min(1).max(512).nullable(),
    json: z.unknown().nullable(),
    decodeError: z.string().min(1).max(128).nullable(),
  })
  .strict();

/** 限定响应正文校验器，在线校验器必须由调用方单次确认上传。 */
export const validatorIdSchema = z.enum([
  "htmlWellFormed",
  "jsonSchema",
  "w3cHtmlOnline",
]);

/** 描述单个校验器启用状态；缺失校验器不等价于启用。 */
export const validatorConfigurationSchema = z
  .object({
    id: validatorIdSchema,
    enabled: z.boolean(),
  })
  .strict();

/** 描述 Validate 配置；外部 endpoint 的实际调用还需本次请求 onlineUploadConfirmed。 */
export const validateConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    validators: z.array(validatorConfigurationSchema).min(1).max(16),
    allowOnlineValidators: z.boolean(),
    w3cEndpoint: z.string().url().max(2_048),
  })
  .strict();

/** 描述用户请求运行的校验器；确认字段为 false 时线上请求不会从浏览器触发。 */
export const validateRequestSchema = z
  .object({
    validatorId: validatorIdSchema,
    onlineUploadConfirmed: z.boolean(),
  })
  .strict();

/** 表示校验器标注的位置；行列均为 1 基以匹配文本查看器的定位习惯。 */
export const validationIssueSchema = z
  .object({
    severity: z.enum(["info", "warning", "error"]),
    messageKey: z.string().min(1).max(256),
    line: z.number().int().positive().nullable(),
    column: z.number().int().positive().nullable(),
  })
  .strict();

/** 保存不含正文的按需校验报告，避免检查器状态复制录制内容。 */
export const validationReportSchema = z
  .object({
    transactionId: z.string().min(1),
    validatorId: validatorIdSchema,
    issues: z.array(validationIssueSchema).max(4_096),
    validatedAtMilliseconds: safeUnsignedIntegerSchema,
  })
  .strict();

/** 当前 M3 导出仅提供标准 HAR 1.2，其他会话格式留待后续阶段扩展。 */
export const exportFormatSchema = z.enum(["har"]);

/** 描述一次导出请求；空 transactionIds 表示导出当前录制会话全部事务。 */
export const exportRequestSchema = z
  .object({
    format: exportFormatSchema,
    includeBodies: z.boolean(),
    transactionIds: z.array(z.string().min(1)).max(100_000).optional(),
  })
  .strict();

/** 描述编辑、重复与高级重复共用的绝对 HTTP 请求；正文保持 Base64，避免文本编辑器破坏二进制字节。 */
export const composeRequestSchema = z
  .object({
    method: z.string().min(1).max(32),
    url: z.string().min(1).max(8_192),
    headers: z.array(headerFieldSchema).max(256),
    bodyBase64: z.string().max(maximumEncodedBodyCharacters),
    viaProxy: z.boolean(),
  })
  .strict();

/** 仅允许覆盖明确提供的字段，缺失字段由后端从原始只读事务继承。 */
export const composeRequestOverridesSchema = z
  .object({
    method: z.string().min(1).max(32).optional(),
    url: z.string().min(1).max(8_192).optional(),
    headers: z.array(headerFieldSchema).max(256).optional(),
    bodyBase64: z.string().max(maximumEncodedBodyCharacters).optional(),
    viaProxy: z.boolean().optional(),
  })
  .strict();

export const repeatRequestSchema = z
  .object({
    transactionId: z.string().min(1),
    overrides: composeRequestOverridesSchema.optional(),
  })
  .strict();

export const composeResultSchema = z
  .object({
    transactionId: z.string().min(1),
    revision: safeUnsignedIntegerSchema,
  })
  .strict();

export const advancedRepeatPlanSchema = z
  .object({
    name: z.string().min(1).max(128),
    base: composeRequestSchema,
    concurrency: z.number().int().min(1).max(256),
    totalIterations: z.number().int().min(1).max(10_000),
    intervalMilliseconds: z.number().int().min(0).max(60_000),
    recordEach: z.boolean(),
    stopOnError: z.boolean(),
  })
  .strict();

export const advancedRepeatStartRequestSchema = advancedRepeatPlanSchema
  .extend({ confirmed: z.boolean() })
  .strict();

export const advancedRepeatStateSchema = z.enum([
  "queued",
  "running",
  "completed",
  "cancelled",
  "failed",
]);

export const latencyStatisticsSchema = z
  .object({
    min: safeUnsignedIntegerSchema,
    max: safeUnsignedIntegerSchema,
    p50: safeUnsignedIntegerSchema,
    p95: safeUnsignedIntegerSchema,
    p99: safeUnsignedIntegerSchema,
  })
  .strict();

export const advancedRepeatJobSchema = z
  .object({
    jobId: z.string().uuid(),
    state: advancedRepeatStateSchema,
    plan: advancedRepeatPlanSchema,
    startedAtMilliseconds: safeUnsignedIntegerSchema,
    finishedAtMilliseconds: safeUnsignedIntegerSchema.nullable(),
    completedIterations: safeUnsignedIntegerSchema,
    successCount: safeUnsignedIntegerSchema,
    failureCount: safeUnsignedIntegerSchema,
    latencyMilliseconds: latencyStatisticsSchema,
    lastError: z.string().nullable(),
  })
  .strict();

export const mcpPublicStateSchema = z
  .object({
    configuration: z
      .object({ enabled: z.boolean(), port: z.number().int().min(1).max(65535) })
      .strict(),
    running: z.boolean(),
    endpoint: z.string().url().nullable(),
    lastError: z.string().nullable(),
  })
  .strict();

const disabledMcpPublicState = {
  configuration: { enabled: false, port: 17_891 },
  running: false,
  endpoint: null,
  lastError: null,
} as const;

export const serviceSnapshotSchema = z
  .object({
    serverInstanceId: serverInstanceIdSchema,
    revision: safeUnsignedIntegerSchema,
    serviceState: serviceStateSchema,
    metrics: metricsSchema,
    sessions: z.array(sessionSnapshotSchema),
    configuration: publicConfigurationSchema,
    processCapture: processCaptureSnapshotSchema,
    listeners: listenersSnapshotSchema,
    ssl: sslPublicStateSchema,
    tools: toolsPublicStateSchema,
    recording: recordingSnapshotSchema,
    transactions: transactionPageSchema,
    // 插件列表是小型控制面状态，随首帧快照建立基线；连接计数变化由增量事件更新。
    plugins: z.array(pluginSnapshotSchema),
    // 高级重复作业是小型有界控制状态，直接进入实时快照，避免前端定时查询作业端点。
    advancedRepeats: z.array(advancedRepeatJobSchema).max(64),
    // 桌面更新时新前端可能先连接仍在退出的旧后端；缺少 MCP 字段只表示该旧进程尚未提供集成服务，
    // 不应让整个权威快照失效并连带阻断事务详情。新后端仍始终显式返回该字段。
    mcp: mcpPublicStateSchema.default(disabledMcpPublicState),
  })
  .strict()
  // 快照 revision 与录制会话必须覆盖同一原子视图，禁止接受跨时刻拼接的数据。
  .superRefine((snapshot, context) => {
    if (snapshot.transactions.revision !== snapshot.revision) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["transactions", "revision"],
        message: "transaction page revision does not match snapshot",
      });
    }
    if (
      snapshot.transactions.recordingSessionId !==
      snapshot.recording.recordingSessionId
    ) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["transactions", "recordingSessionId"],
        message: "transaction page belongs to another recording session",
      });
    }
  });

export const eventMessageSchema = z
  .discriminatedUnion("type", [
    z
      .object({
        type: z.literal("snapshot"),
        serverInstanceId: serverInstanceIdSchema,
        snapshot: serviceSnapshotSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("serviceState"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        serviceState: serviceStateSchema,
        listeners: listenersSnapshotSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("mcp"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        mcp: mcpPublicStateSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("metrics"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        metrics: metricsSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("processCapture"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        processCapture: processCaptureSnapshotSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("sessions"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        sessions: z.array(sessionSnapshotSchema),
      })
      .strict(),
    z
      .object({
        type: z.literal("configuration"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        configuration: publicConfigurationSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("recording"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        recording: recordingSnapshotSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("ssl"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        ssl: sslPublicStateSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("tools"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        tools: toolsPublicStateSchema,
      })
      .strict(),
    z
      .object({
        type: z.literal("breakpoints"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        suspended: z.array(suspendedBreakpointSchema).max(1_024),
      })
      .strict(),
    z
      .object({
        type: z.literal("advancedRepeats"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        jobs: z.array(advancedRepeatJobSchema).max(64),
      })
      .strict(),
    z
      .object({
        type: z.literal("plugins"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        plugins: z.array(pluginSnapshotSchema),
      })
      .strict(),
    z
      .object({
        type: z.literal("transactions"),
        serverInstanceId: serverInstanceIdSchema,
        revision: safeUnsignedIntegerSchema,
        transactions: transactionPageSchema,
      })
      .strict(),
  ])
  .superRefine((message, context) => {
    if (
      message.type === "snapshot" &&
      message.serverInstanceId !== message.snapshot.serverInstanceId
    ) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["serverInstanceId"],
        message: "snapshot event belongs to another server instance",
      });
    }
    if (
      message.type === "transactions" &&
      message.transactions.revision !== message.revision
    ) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["transactions", "revision"],
        message: "transaction event revisions do not match",
      });
    }
  });


export type TransactionSummary = z.infer<typeof transactionSummarySchema>;

export type TransactionPage = z.infer<typeof transactionPageSchema>;

export type MessageSide = z.infer<typeof messageSideSchema>;

export type BodyHandleMeta = z.infer<typeof bodyHandleMetaSchema>;

export type TransactionDetail = z.infer<typeof transactionDetailSchema>;

export type EncodedBodyResponse = z.infer<typeof encodedBodyResponseSchema>;

export type ProtobufSchemaEntry = z.infer<typeof protobufSchemaEntrySchema>;

export type ProtobufRoute = z.infer<typeof protobufRouteSchema>;

export type ProtobufConfiguration = z.infer<typeof protobufConfigurationSchema>;

export type ProtobufConfigurationUpdate = z.infer<
  typeof protobufConfigurationUpdateSchema
>;

export type ProtobufDescriptorUpload = z.infer<
  typeof protobufDescriptorUploadSchema
>;

export type DecodedProtobufView = z.infer<typeof decodedProtobufViewSchema>;

export type ValidatorId = z.infer<typeof validatorIdSchema>;

export type ValidatorConfiguration = z.infer<
  typeof validatorConfigurationSchema
>;

export type ValidateConfiguration = z.infer<typeof validateConfigurationSchema>;

export type ValidateRequest = z.infer<typeof validateRequestSchema>;

export type ValidationIssue = z.infer<typeof validationIssueSchema>;

export type ValidationReport = z.infer<typeof validationReportSchema>;

export type ExportRequest = z.infer<typeof exportRequestSchema>;

export type ComposeRequest = z.infer<typeof composeRequestSchema>;

export type ComposeRequestOverrides = z.infer<
  typeof composeRequestOverridesSchema
>;

export type RepeatRequest = z.infer<typeof repeatRequestSchema>;

export type ComposeResult = z.infer<typeof composeResultSchema>;

export type AdvancedRepeatPlan = z.infer<typeof advancedRepeatPlanSchema>;

export type AdvancedRepeatStartRequest = z.infer<
  typeof advancedRepeatStartRequestSchema
>;

export type AdvancedRepeatState = z.infer<typeof advancedRepeatStateSchema>;

export type LatencyStatistics = z.infer<typeof latencyStatisticsSchema>;

export type AdvancedRepeatJob = z.infer<typeof advancedRepeatJobSchema>;

export type ServiceSnapshot = z.infer<typeof serviceSnapshotSchema>;

export type McpConfiguration = ServiceSnapshot["mcp"]["configuration"];

export type EventMessage = z.infer<typeof eventMessageSchema>;
