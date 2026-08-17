import { z } from "zod";

import { locationPatternSchema, serverInstanceIdSchema } from "./protocolCore";
import {
  maximumEncodedBodyCharacters,
  maximumRecordingBodyBytes,
  maximumRecordingTotalBodyBytes,
  maximumRecordingTransactions,
  safePositiveIntegerSchema,
  safeUnsignedIntegerSchema,
} from "./protocolShared";

/** 定义改包工具、插件扩展与录制控制面的配置协议。 */
export const headerFieldSchema = z
  .object({
    name: z.string(),
    value: z.string(),
  })
  .strict();

/** 限定屏蔽列表的稳定工作模式；off 必须保持数据面完全透明。 */
export const blockModeSchema = z.enum(["off", "blockList", "allowList"]);

/** 描述屏蔽列表对命中请求生成的合成响应。 */
export const blockListConfigurationSchema = z
  .object({
    mode: blockModeSchema,
    locations: z.array(locationPatternSchema),
    statusCode: z.number().int().min(100).max(599),
    responseBody: z.string().max(64 * 1024),
    closeConnection: z.boolean(),
  })
  .strict();

/** 描述请求和响应两侧的缓存头处理策略。 */
export const noCachingConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    locations: z.array(locationPatternSchema),
    stripRequestHeaders: z.boolean(),
    stripResponseHeaders: z.boolean(),
    injectRequestNoCache: z.boolean(),
    injectResponseNoStore: z.boolean(),
  })
  .strict();

/** 描述 Cookie 与 Set-Cookie 两个独立方向的剥离行为。 */
export const blockCookiesConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    locations: z.array(locationPatternSchema),
    stripRequestCookie: z.boolean(),
    stripResponseSetCookie: z.boolean(),
  })
  .strict();

/** 描述单条代理进程 DNS 映射；主机支持通配符，目标必须是明确的 IPv4 或 IPv6。 */
export const dnsSpoofingRuleSchema = z
  .object({
    id: z.string().min(1).max(128),
    enabled: z.boolean(),
    hostPattern: z.string().min(1).max(255),
    ipAddress: z.string().ip(),
  })
  .strict();

/** 描述 DNS 映射总开关与有序规则，首条命中规则决定实际出站 IP。 */
export const dnsSpoofingConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    rules: z.array(dnsSpoofingRuleSchema).max(2_000),
  })
  .strict();

/** 描述单条本地文件或目录映射规则。 */
export const mapLocalRuleSchema = z
  .object({
    id: z.string().min(1).max(128),
    enabled: z.boolean(),
    location: locationPatternSchema,
    localPath: z.string().min(1).max(4_096),
    isDirectory: z.boolean(),
    statusCode: z.number().int().min(100).max(599),
    responseHeaders: z.array(headerFieldSchema).max(128),
    contentTypeOverride: z.string().max(512),
  })
  .strict();

/** 描述 Map Local 的总开关和按顺序生效的规则集合。 */
export const mapLocalConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    rules: z.array(mapLocalRuleSchema).max(2_000),
  })
  .strict();

/** 描述浏览器文件选择已落入后端受管目录的结果；localPath 可直接写入 Map Local 规则。 */
export const mapLocalImportResultSchema = z
  .object({
    localPath: z.string().min(1).max(4_096),
    fileCount: z.number().int().min(1).max(2_000),
    totalBytes: z.number().int().nonnegative().safe(),
  })
  .strict();

/** 描述远程目标的可选覆盖字段；空字符串表示保留原值。 */
export const mapRemoteTargetSchema = z
  .object({
    protocol: z.string().max(16),
    host: z.string().max(255),
    port: z.string().max(32),
    path: z.string().max(2_048),
  })
  .strict();

/** 描述单条远程目标映射，规则顺序就是匹配优先级。 */
export const mapRemoteRuleSchema = z
  .object({
    id: z.string().min(1).max(128),
    enabled: z.boolean(),
    from: locationPatternSchema,
    to: mapRemoteTargetSchema,
  })
  .strict();

/** 描述 Map Remote 的总开关和规则集合。 */
export const mapRemoteConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    rules: z.array(mapRemoteRuleSchema).max(2_000),
  })
  .strict();

/** 限定重写规则可安全作用的 HTTP 消息字段。 */
export const rewriteRuleTypeSchema = z.enum([
  "urlHost",
  "urlPath",
  "urlQuery",
  "requestHeader",
  "responseHeader",
  "requestBody",
  "responseBody",
  "responseStatus",
]);

/** 限定头规则的加、改、删操作，避免自由文本分支。 */
export const headerActionSchema = z.enum(["add", "modify", "remove"]);

/** 描述一条可验证的 Rewrite 规则。 */
export const rewriteRuleSchema = z
  .object({
    id: z.string().min(1).max(128),
    enabled: z.boolean(),
    type: rewriteRuleTypeSchema,
    matchRegex: z.string().max(4_096),
    replace: z.string().max(64 * 1024),
    headerName: z.string().max(256).nullable(),
    matchValueRegex: z.string().max(4_096).nullable(),
    headerAction: headerActionSchema.nullable(),
    caseSensitive: z.boolean(),
    matchAllOccurrences: z.boolean(),
  })
  .strict();

/** 描述按 Location 限定的一组重写规则。 */
export const rewriteSetSchema = z
  .object({
    id: z.string().min(1).max(128),
    name: z.string().min(1).max(256),
    enabled: z.boolean(),
    locations: z.array(locationPatternSchema),
    rules: z.array(rewriteRuleSchema).max(2_000),
  })
  .strict();

/** 描述 Rewrite 工具的总开关和规则集。 */
export const rewriteConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    sets: z.array(rewriteSetSchema).max(256),
  })
  .strict();

/** 描述单条请求/响应断点规则。 */
export const breakpointRuleSchema = z
  .object({
    id: z.string().min(1).max(128),
    enabled: z.boolean(),
    location: locationPatternSchema,
    onRequest: z.boolean(),
    onResponse: z.boolean(),
  })
  .strict();

/** 限定断点超时后的固定处理策略。 */
export const breakpointTimeoutActionSchema = z.enum(["continue", "abort"]);

/** 描述断点调度边界；maxSuspended 防止无界占用代理连接。 */
export const breakpointsConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    rules: z.array(breakpointRuleSchema).max(2_000),
    suspendTimeoutSeconds: z.number().int().min(1).max(3_600),
    maxSuspended: z.number().int().min(1).max(1_024),
    onTimeout: breakpointTimeoutActionSchema,
  })
  .strict();

/** 描述断点编辑器可安全回写到流水线的完整消息草稿。 */
export const editableHttpMessageSchema = z
  .object({
    method: z.string().max(32).nullable(),
    url: z.string().max(8_192).nullable(),
    statusCode: z.number().int().min(100).max(599).nullable(),
    reason: z.string().max(1_024).nullable(),
    headers: z.array(headerFieldSchema).max(256),
    bodyBase64: z.string().max(maximumEncodedBodyCharacters),
  })
  .strict();

/** 描述当前等待人工处理的断点项；事务正文不进入普通事务事件。 */
export const suspendedBreakpointSchema = z
  .object({
    breakpointId: z.string().min(1).max(128),
    transactionId: z.string().min(1),
    phase: z.enum(["request", "response"]),
    suspendedAtMilliseconds: safeUnsignedIntegerSchema,
    expiresAtMilliseconds: safeUnsignedIntegerSchema,
    draft: editableHttpMessageSchema,
  })
  .strict();

/** 描述一组可选择的带宽与延迟预设。 */
export const throttlePresetSchema = z
  .object({
    id: z.string().min(1).max(128),
    name: z.string().min(1).max(128),
    downloadBytesPerSecond: safePositiveIntegerSchema,
    uploadBytesPerSecond: safePositiveIntegerSchema,
    latencyMilliseconds: safeUnsignedIntegerSchema.max(300_000),
    latencyJitterMilliseconds: safeUnsignedIntegerSchema.max(300_000),
    reliabilityPercent: z.number().int().min(0).max(100),
    mtu: z.number().int().min(64).max(65_535),
  })
  .strict();

/** 描述节流配置；空 locations 表示对全部匹配协议流量生效。 */
export const throttlingConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    activePresetId: z.string().min(1).max(128).nullable(),
    custom: throttlePresetSchema.omit({ id: true, name: true }),
    locations: z.array(locationPatternSchema),
  })
  .strict();

/** 描述工具快照中的节流公开状态，预设只读而 custom 可编辑。 */
export const throttlingPublicStateSchema = throttlingConfigurationSchema
  .extend({ presets: z.array(throttlePresetSchema).min(1).max(64) })
  .strict();

/** 定义镜像报文的目录布局；层级布局按主机和路径归档，扁平布局适用于外部文件查看器。 */
export const mirrorLayoutSchema = z.enum(["hierarchical", "flat"]);

/** 定义镜像写入队列满时的确定策略；block 只在用户明确选择时向数据面施加背压。 */
export const mirrorOverflowPolicySchema = z.enum(["drop", "block"]);

/** 描述镜像工具可提交的完整配置；启用时的路径和方向组合仍由后端作原子语义校验。 */
export const mirrorConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    rootDirectory: z.string().max(4_096),
    locations: z.array(locationPatternSchema).max(2_000),
    mirrorRequest: z.boolean(),
    mirrorResponse: z.boolean(),
    layout: mirrorLayoutSchema,
    onOverflow: mirrorOverflowPolicySchema,
    maxQueueLength: z.number().int().min(1).max(4_096),
  })
  .strict();

/** 描述镜像配置及累计写入状态；错误只包含稳定机器码，不暴露本机目录或系统错误。 */
export const mirrorPublicStateSchema = mirrorConfigurationSchema
  .extend({
    writtenFiles: safeUnsignedIntegerSchema,
    droppedWrites: safeUnsignedIntegerSchema,
    lastError: z.string().nullable(),
  })
  .strict();

/** 限定自动保存输出格式；native 用于完整恢复，har 用于跨工具交换。 */
export const autoSaveFormatSchema = z.enum(["native", "har"]);

/** 描述自动保存可提交配置；时间与事务计数均为零时由后端拒绝启用状态。 */
export const autoSaveConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    directory: z.string().max(4_096),
    intervalSeconds: z.number().int().min(0).max(86_400),
    everyNTransactions: z.number().int().min(0).max(100_000),
    format: autoSaveFormatSchema,
    maxFiles: z.number().int().min(1).max(1_000),
    includeBodies: z.boolean(),
  })
  .strict();

/** 描述自动保存的配置与最近结果；最近输出路径只面向本机桌面，不参与浏览器持久化。 */
export const autoSavePublicStateSchema = autoSaveConfigurationSchema
  .extend({
    lastSavedAtMilliseconds: safeUnsignedIntegerSchema.nullable(),
    lastSavedPath: z.string().nullable(),
    lastError: z.string().nullable(),
  })
  .strict();

/** 描述一条反向代理监听规则；本机监听地址必须是 IP，远程主机可为域名或 IP。 */
export const reverseProxyEntrySchema = z
  .object({
    id: z.string().min(1).max(64),
    enabled: z.boolean(),
    listenHost: z.string().min(1).max(45),
    listenPort: z.number().int().min(1).max(65_535),
    remoteHost: z.string().min(1).max(253),
    remotePort: z.number().int().min(1).max(65_535),
    remoteScheme: z.enum(["http", "https"]),
    preserveHostHeader: z.boolean(),
    stripPathPrefix: z.string().max(2_048),
  })
  .strict();

/** 描述一条透明 TCP 端口转发规则；连接内容不进入 HTTP 事务模型。 */
export const portForwardEntrySchema = z
  .object({
    id: z.string().min(1).max(64),
    enabled: z.boolean(),
    listenHost: z.string().min(1).max(45),
    listenPort: z.number().int().min(1).max(65_535),
    targetHost: z.string().min(1).max(253),
    targetPort: z.number().int().min(1).max(65_535),
  })
  .strict();

/** 返回辅助监听器实际绑定的端点；界面必须展示此值而不能从请求端口猜测运行状态。 */
export const listenerBindingSchema = z
  .object({
    id: z.string().min(1).max(64),
    boundEndpoint: z.string().min(1),
  })
  .strict();

/** 描述辅助监听配置和运行时绑定，两个规则列表总是作为同一冲突域读取。 */
export const auxiliaryListenerPublicStateSchema = z
  .object({
    configuration: z
      .object({
        reverseProxies: z.array(reverseProxyEntrySchema).max(128),
        portForwards: z.array(portForwardEntrySchema).max(128),
      })
      .strict(),
    bindings: z
      .object({
        reverseProxies: z.array(listenerBindingSchema).max(128),
        portForwards: z.array(listenerBindingSchema).max(128),
      })
      .strict(),
  })
  .strict();

/** 封包滤镜的传输条件；any 在同一规则中同时匹配 TCP 与 UDP。 */
export const packetFilterTransportSchema = z.enum(["any", "tcp", "udp"]);

/** 封包滤镜的方向条件；up 是客户端到服务器，down 是服务器到客户端。 */
export const packetFilterDirectionSchema = z.enum(["any", "up", "down"]);

/** 封包滤镜只提供变长替换、丢弃和关闭连接三种有实际效果的最终动作。 */
export const packetFilterActionSchema = z.enum(["modify", "drop", "close"]);

/** 描述一条有序封包滤镜；十六进制模式使用空格分隔字节与 `??` 通配符。 */
export const packetFilterRuleSchema = z
  .object({
    id: z.string().min(1).max(64),
    name: z.string().min(1).max(128),
    enabled: z.boolean(),
    transport: packetFilterTransportSchema,
    direction: packetFilterDirectionSchema,
    host: z.string().max(253),
    port: z.number().int().min(1).max(65_535).nullable(),
    minimumLength: z
      .number()
      .int()
      .min(1)
      .max(16 * 1024 * 1024)
      .nullable(),
    maximumLength: z
      .number()
      .int()
      .min(1)
      .max(16 * 1024 * 1024)
      .nullable(),
    pattern: z.string().max(4_096),
    replacement: z.string().max(4_096),
    action: packetFilterActionSchema,
    replaceAll: z.boolean(),
    continueMatching: z.boolean(),
  })
  .strict();

/** 保存可热更新和持久化的封包滤镜总开关与执行顺序。 */
export const packetFilterConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    rules: z.array(packetFilterRuleSchema).max(256),
  })
  .strict();

/** 聚合 M3 工具状态；snapshot 不携带任一事务正文或挂起草稿。 */
export const toolsPublicStateSchema = z
  .object({
    pipelineOrder: z.array(z.string().min(1)).min(1).max(32),
    packetFilters: packetFilterConfigurationSchema,
    blockList: blockListConfigurationSchema,
    noCaching: noCachingConfigurationSchema,
    blockCookies: blockCookiesConfigurationSchema,
    dnsSpoofing: dnsSpoofingConfigurationSchema,
    mapLocal: mapLocalConfigurationSchema,
    mapRemote: mapRemoteConfigurationSchema,
    rewrite: rewriteConfigurationSchema,
    breakpoints: breakpointsConfigurationSchema,
    throttling: throttlingPublicStateSchema,
    mirror: mirrorPublicStateSchema,
    autoSave: autoSavePublicStateSchema,
    suspendedBreakpointCount: safeUnsignedIntegerSchema,
  })
  .strict();

/** 插件清单可公开的 Native Hook 名称；线上的 ABI 枚举由后端负责冻结，浏览器仅消费稳定字符串。 */
export const pluginHookSchema = z.enum([
  "on_connection_open",
  "on_stream_data",
  "on_connection_close",
]);

/** 插件运行时类型；当前数据热路径仅支持 native，sidecar 保留为后续控制面扩展。 */
export const pluginRuntimeSchema = z.enum(["native", "sidecar"]);

/** 插件生命周期状态；Failed 和 Incompatible 保留机器错误码以便管理界面给出可恢复动作。 */
export const pluginStateSchema = z.enum([
  "disabled",
  "enabled",
  "failed",
  "incompatible",
]);

/** 受支持的声明式配置字段类型；该集合与后端 JSON Schema 子集严格对应。 */
export const pluginConfigValueTypeSchema = z.enum([
  "string",
  "number",
  "integer",
  "boolean",
]);

/** 描述一个由宿主渲染的插件配置字段；format=password 字段不含旧值，只通过 configuredSecretFields 标识状态。 */
export const pluginConfigFieldSchema = z
  .object({
    type: pluginConfigValueTypeSchema,
    title: z.string(),
    description: z.string(),
    enum: z.array(z.union([z.string(), z.number(), z.boolean()])),
    default: z.union([z.string(), z.number(), z.boolean()]).nullable(),
    format: z.string(),
    xAdvanced: z.boolean(),
    minimum: z.number().nullable(),
    maximum: z.number().nullable(),
    minLength: z.number().int().nonnegative().nullable(),
    maxLength: z.number().int().nonnegative().nullable(),
  })
  .strict();

/** 描述可自动生成设置表单的对象 Schema；additionalProperties 恒为 false，未知字段由后端拒绝。 */
export const pluginConfigSchema = z
  .object({
    type: z.literal("object"),
    title: z.string(),
    description: z.string(),
    properties: z.record(z.string(), pluginConfigFieldSchema),
    required: z.array(z.string()),
    additionalProperties: z.boolean(),
  })
  .strict();

/** 公开插件列表行；配置、文件路径和 Native ABI 上下文不属于此响应。 */
export const pluginSnapshotSchema = z
  .object({
    id: z.string().min(1),
    name: z.string(),
    version: z.string(),
    apiVersion: z.number().int().nonnegative(),
    runtime: pluginRuntimeSchema,
    hooks: z.array(pluginHookSchema),
    enabled: z.boolean(),
    state: pluginStateSchema,
    errorCode: z.string().nullable(),
    activeConnections: z.number().int().nonnegative(),
  })
  .strict();

/** 插件设置详情；configuration 永不返回 password 字段，避免浏览器缓存或错误上报泄露秘密。 */
export const pluginDetailsSchema = z
  .object({
    snapshot: pluginSnapshotSchema,
    configSchema: pluginConfigSchema.nullable(),
    configuration: z.record(z.string(), z.unknown()),
    configuredSecretFields: z.array(z.string()),
  })
  .strict();

/** 写入插件配置的稳定请求体；对象内容由服务端对应 manifest 的 Schema 校验。 */
export const pluginConfigurationUpdateSchema = z
  .object({
    configuration: z.record(z.string(), z.unknown()),
  })
  .strict();

/** 插件 manifest 的自由行为说明标签；宿主不维护枚举、不授权也不拒绝。 */
export const extensionCapabilitySchema = z.string();

/** 用户可覆盖的预编译匹配范围；空数组表示该维度不限制。 */
export const extensionMatchSchema = z
  .object({
    entries: z.array(z.string()),
    processNames: z.array(z.string()),
    processPaths: z.array(z.string()),
    transports: z.array(z.string()),
    protocols: z.array(z.string()),
    directions: z.array(z.string()),
    schemes: z.array(z.string()),
    hosts: z.array(z.string()),
    cidrs: z.array(z.string()),
    ports: z.array(z.number().int().min(1).max(65_535)),
    methods: z.array(z.string()),
    paths: z.array(z.string()),
    statusCodes: z.array(z.number().int().min(100).max(999)),
    mimeTypes: z.array(z.string()),
    labels: z.array(z.string()),
  })
  .strict();

/** 插件作者声明的运行说明；宿主只持久化和展示，不据此限制可信 Mod。 */
export const extensionLimitsSchema = z
  .object({
    timeoutMs: z.number().int().nonnegative(),
    maxPendingEvents: z.number().int().nonnegative(),
    maxOutputBytes: z.number().int().nonnegative(),
    maxStorageBytes: z.number().int().nonnegative(),
  })
  .strict();

/** 单插件持久用户意图；能力列表只用于说明插件使用的宿主接口，不作为运行门禁。 */
export const pluginUserConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    activeVersion: z.string().nullable(),
    moduleOrder: z.array(z.string()),
    subscriptionOverrides: z.record(z.string(), extensionMatchSchema),
    failurePolicy: z.enum(["failClosed", "failOpen"]),
    limits: extensionLimitsSchema.nullable(),
    configurationSchemaVersion: z.string().nullable(),
    configuration: z.unknown(),
    secretReferences: z.record(z.string(), z.string().startsWith("secret://")),
    automaticRestart: z.boolean(),
  })
  .strict();

/** 宿主权威插件平台配置文件；schemaVersion 用于拒绝未知迁移格式。 */
export const pluginPlatformConfigurationSchema = z
  .object({
    schemaVersion: z.number().int().positive(),
    plugins: z.record(z.string(), pluginUserConfigurationSchema),
  })
  .strict();

/** 隔离运行时实例快照；不包含插件路径、配置或正文。 */
export const extensionInstanceSnapshotSchema = z
  .object({
    pluginId: z.string().min(1),
    version: z.string().min(1),
    runtimeKind: z.enum([
      "wasm",
      "sidecar",
      "nativeWorker",
      "native",
      "legacyNative",
    ]),
    instanceGeneration: z.number().int().nonnegative(),
    consecutiveFailures: z.number().int().nonnegative(),
    inFlightInvocations: z.number().int().nonnegative(),
  })
  .strict();

/** 固定预算调用追踪；仅含摘要、动作和稳定错误码。 */
export const extensionInvocationTraceSchema = z
  .object({
    pluginId: z.string(),
    moduleId: z.string(),
    eventId: z.string(),
    stage: z.string(),
    action: z.string().nullable(),
    elapsedMicroseconds: z.number().int().nonnegative(),
    inputBytes: z.number().int().nonnegative(),
    outputBytes: z.number().int().nonnegative(),
    errorCode: z.string().nullable(),
  })
  .strict();

export const recordingStateSchema = z.enum(["recording", "paused"]);

export const recordingLimitsSchema = z
  .object({
    maxTransactions: z
      .number()
      .int()
      .positive()
      .max(maximumRecordingTransactions),
    maxBodyBytes: z.number().int().positive().max(maximumRecordingBodyBytes),
    maxTotalBodyBytes: z
      .number()
      .int()
      .positive()
      .max(maximumRecordingTotalBodyBytes),
  })
  .strict();

export const recordingSnapshotSchema = z
  .object({
    recordingSessionId: z.string().min(1),
    state: recordingStateSchema,
    startedAtMilliseconds: safeUnsignedIntegerSchema,
    transactionCount: safeUnsignedIntegerSchema,
    droppedCount: safeUnsignedIntegerSchema,
    totalBodyBytes: safeUnsignedIntegerSchema,
    totalMetadataBytes: safeUnsignedIntegerSchema,
    metadataMemoryBudgetBytes: safePositiveIntegerSchema,
    pendingCleanupCount: safeUnsignedIntegerSchema,
    limits: recordingLimitsSchema,
    ignoreLocations: z.array(locationPatternSchema),
    recordTunnelMetadata: z.boolean(),
  })
  .strict();

export const recordingUpdateSchema = z
  .object({
    state: recordingStateSchema.optional(),
    ignoreLocations: z.array(locationPatternSchema).optional(),
    recordTunnelMetadata: z.boolean().optional(),
  })
  .strict();

export const recordingResponseSchema = z
  .object({
    serverInstanceId: serverInstanceIdSchema,
    revision: safeUnsignedIntegerSchema,
    recording: recordingSnapshotSchema,
  })
  .strict();


export type BlockListConfiguration = z.infer<
  typeof blockListConfigurationSchema
>;

export type PacketFilterTransport = z.infer<typeof packetFilterTransportSchema>;

export type PacketFilterDirection = z.infer<typeof packetFilterDirectionSchema>;

export type PacketFilterAction = z.infer<typeof packetFilterActionSchema>;

export type PacketFilterRule = z.infer<typeof packetFilterRuleSchema>;

export type PacketFilterConfiguration = z.infer<
  typeof packetFilterConfigurationSchema
>;

export type NoCachingConfiguration = z.infer<
  typeof noCachingConfigurationSchema
>;

export type BlockCookiesConfiguration = z.infer<
  typeof blockCookiesConfigurationSchema
>;

export type DnsSpoofingConfiguration = z.infer<
  typeof dnsSpoofingConfigurationSchema
>;

export type MapLocalConfiguration = z.infer<typeof mapLocalConfigurationSchema>;

export type MapLocalImportResult = z.infer<typeof mapLocalImportResultSchema>;

export type MapRemoteConfiguration = z.infer<
  typeof mapRemoteConfigurationSchema
>;

export type RewriteConfiguration = z.infer<typeof rewriteConfigurationSchema>;

export type BreakpointsConfiguration = z.infer<
  typeof breakpointsConfigurationSchema
>;

export type ThrottlingConfiguration = z.infer<
  typeof throttlingConfigurationSchema
>;

export type ThrottlingPublicState = z.infer<typeof throttlingPublicStateSchema>;

export type MirrorConfiguration = z.infer<typeof mirrorConfigurationSchema>;

export type MirrorPublicState = z.infer<typeof mirrorPublicStateSchema>;

export type AutoSaveConfiguration = z.infer<typeof autoSaveConfigurationSchema>;

export type AutoSavePublicState = z.infer<typeof autoSavePublicStateSchema>;

export type ReverseProxyEntry = z.infer<typeof reverseProxyEntrySchema>;

export type PortForwardEntry = z.infer<typeof portForwardEntrySchema>;

export type AuxiliaryListenerPublicState = z.infer<
  typeof auxiliaryListenerPublicStateSchema
>;

export type EditableHttpMessage = z.infer<typeof editableHttpMessageSchema>;

export type SuspendedBreakpoint = z.infer<typeof suspendedBreakpointSchema>;

export type ToolsPublicState = z.infer<typeof toolsPublicStateSchema>;

export type PluginHook = z.infer<typeof pluginHookSchema>;

export type PluginRuntime = z.infer<typeof pluginRuntimeSchema>;

export type PluginState = z.infer<typeof pluginStateSchema>;

export type PluginConfigValueType = z.infer<typeof pluginConfigValueTypeSchema>;

export type PluginConfigField = z.infer<typeof pluginConfigFieldSchema>;

export type PluginConfigSchema = z.infer<typeof pluginConfigSchema>;

export type PluginSnapshot = z.infer<typeof pluginSnapshotSchema>;

export type PluginDetails = z.infer<typeof pluginDetailsSchema>;

export type PluginConfigurationUpdate = z.infer<
  typeof pluginConfigurationUpdateSchema
>;

export type ExtensionCapability = z.infer<typeof extensionCapabilitySchema>;

export type ExtensionMatch = z.infer<typeof extensionMatchSchema>;

export type ExtensionLimits = z.infer<typeof extensionLimitsSchema>;

export type PluginUserConfiguration = z.infer<
  typeof pluginUserConfigurationSchema
>;

export type PluginPlatformConfiguration = z.infer<
  typeof pluginPlatformConfigurationSchema
>;

export type ExtensionInstanceSnapshot = z.infer<
  typeof extensionInstanceSnapshotSchema
>;

export type ExtensionInvocationTrace = z.infer<
  typeof extensionInvocationTraceSchema
>;

export type RecordingSnapshot = z.infer<typeof recordingSnapshotSchema>;

export type RecordingUpdate = z.infer<typeof recordingUpdateSchema>;

export type RecordingResponse = z.infer<typeof recordingResponseSchema>;

export type HeaderField = z.infer<typeof headerFieldSchema>;
