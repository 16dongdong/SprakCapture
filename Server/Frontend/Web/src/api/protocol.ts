import { z } from "zod";

export const maximumConnections = 16_384;
export const maximumRelayBufferSize = 1_048_576;
// 数据面 512 MiB 合计预算中固定预留 64 MiB 给自动 SOCKS5 正文镜像。
export const maximumTotalRelayBufferSize = 448 * 1024 * 1024;
export const maximumShutdownTimeoutSeconds = 30;
export const maximumUdpPacketSize = 65_507;
export const maximumHttpHeaderBytes = 1024 * 1024;
export const maximumHttpCaptureBodyBytes = 64 * 1024 * 1024;
export const maximumHttpTimeoutMilliseconds = 5 * 60 * 1000;
// 录制更新和正文响应必须复用后端固定边界，避免客户端接受随后必定被拒绝或无界展开的值。
export const maximumRecordingTransactions = Number.MAX_SAFE_INTEGER;
export const maximumRecordingBodyBytes = Number.MAX_SAFE_INTEGER;
export const maximumRecordingTotalBodyBytes = Number.MAX_SAFE_INTEGER;
export const maximumTransactionCollectionTokenCharacters = 128;
export const maximumCachedCertificates = 4_096;
export const maximumPluginPackageBytes = 64 * 1024 * 1024;
// JavaScript 字符串长度远低于安全整数上限；保持精确上界即可，不用乘法制造不可精确数字。
export const maximumEncodedBodyCharacters = Number.MAX_SAFE_INTEGER;
// FileDescriptorSet 的后端上限为 16 MiB；Base64 传输按 4/3 展开并保留完整填充字符。
export const maximumDescriptorEncodedCharacters =
  Math.ceil((16 * 1024 * 1024) / 3) * 4;
// Rust u64 只有在 JavaScript 安全整数范围内才能保持 revision、sequence 与计数的精确顺序。
const safeUnsignedIntegerSchema = z
  .number()
  .int()
  .nonnegative()
  .max(Number.MAX_SAFE_INTEGER);
const safePositiveIntegerSchema = z
  .number()
  .int()
  .positive()
  .max(Number.MAX_SAFE_INTEGER);
// 后台实例标识采用随机 UUID；revision 只能在同一标识内建立顺序。
export const serverInstanceIdSchema = z.string().uuid();

export const serviceStateSchema = z.enum([
  "stopped",
  "starting",
  "running",
  "stopping",
  "faulted",
]);

export const sessionStateSchema = z.enum([
  "negotiating",
  "authenticating",
  "connecting",
  "binding",
  "udpAssociating",
  "relaying",
  "closed",
  "failed",
]);

export const metricsSchema = z
  .object({
    acceptedConnections: safeUnsignedIntegerSchema,
    activeConnections: safeUnsignedIntegerSchema,
    failedConnections: safeUnsignedIntegerSchema,
    bytesUp: safeUnsignedIntegerSchema,
    bytesDown: safeUnsignedIntegerSchema,
    udpPacketsUp: safeUnsignedIntegerSchema,
    udpPacketsDown: safeUnsignedIntegerSchema,
    droppedUdpPackets: safeUnsignedIntegerSchema,
  })
  .strict();

export const sessionSnapshotSchema = z
  .object({
    sessionId: z.string().min(1),
    clientAddress: z.string(),
    username: z.string(),
    command: z.enum(["connect", "bind", "udpAssociate"]).or(z.literal("")),
    targetAddress: z.string(),
    state: sessionStateSchema,
    bytesUp: safeUnsignedIntegerSchema,
    bytesDown: safeUnsignedIntegerSchema,
    createdAtMilliseconds: safeUnsignedIntegerSchema,
    updatedAtMilliseconds: safeUnsignedIntegerSchema,
    closedAtMilliseconds: safeUnsignedIntegerSchema,
    errorMessage: z.string(),
  })
  .strict();

export const httpProxyConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    listenHost: z.string().min(1),
    listenPort: z.number().int().nonnegative().max(65535),
    maxConnections: z.number().int().min(1).max(maximumConnections),
    maxHeaderBytes: z
      .number()
      .int()
      .min(8 * 1024)
      .max(maximumHttpHeaderBytes),
    maxCaptureBodyBytes: z
      .number()
      .int()
      .min(1)
      .max(maximumHttpCaptureBodyBytes),
    connectTimeoutMilliseconds: z
      .number()
      .int()
      .min(1)
      .max(maximumHttpTimeoutMilliseconds),
    requestTimeoutMilliseconds: z
      .number()
      .int()
      .min(1)
      .max(maximumHttpTimeoutMilliseconds),
    headerReadTimeoutMilliseconds: z
      .number()
      .int()
      .min(1)
      .max(maximumHttpTimeoutMilliseconds),
    shutdownTimeoutMilliseconds: z
      .number()
      .int()
      .min(1)
      .max(maximumHttpTimeoutMilliseconds),
  })
  .strict();

/** 限定二级代理支持的标准隧道协议，监听端口仍由本地融合入口统一承载。 */
export const upstreamProxyProtocolSchema = z.enum(["http", "socks5"]);

/** 描述可公开展示的二级代理配置；口令只暴露是否已保存，绝不回传原文。 */
export const publicUpstreamProxyConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    protocol: upstreamProxyProtocolSchema,
    host: z.string(),
    port: z.number().int().min(1).max(65535),
    username: z.string(),
    hasPassword: z.boolean(),
  })
  .strict();

/** 接收二级代理更新；password=null 表示继续使用服务端已保存的口令。 */
export const upstreamProxyUpdateSchema = z
  .object({
    enabled: z.boolean(),
    protocol: upstreamProxyProtocolSchema,
    host: z.string(),
    port: z.number().int().min(1).max(65535),
    username: z.string(),
    password: z.string().nullable(),
  })
  .strict();

/** 描述 WinDivert 按进程透明捕获配置；代理端口必须与融合监听端口一致。 */
export const processCaptureConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    processIds: z.array(z.number().int().positive()),
    proxyPort: z.number().int().min(1).max(65535),
  })
  .strict();

/** 暴露透明捕获的数据面状态和有界计数器，供界面诊断驱动生命周期。 */
export const processCaptureSnapshotSchema = z
  .object({
    running: z.boolean(),
    configuredProcessIds: z.array(z.number().int().positive()),
    trackedFlows: safeUnsignedIntegerSchema,
    acceptedConnections: safeUnsignedIntegerSchema,
    redirectedPackets: safeUnsignedIntegerSchema,
    restoredPackets: safeUnsignedIntegerSchema,
    bytesUp: safeUnsignedIntegerSchema,
    bytesDown: safeUnsignedIntegerSchema,
    lastError: z.string().nullable(),
  })
  .strict();

/** 描述进程选择器中的运行实例；可执行路径是跨重启持久化与 PID 重新解析的稳定键。 */
export const processCandidateSchema = z
  .object({
    processId: z.number().int().positive(),
    name: z.string().min(1),
    executablePath: z.string().min(1),
  })
  .strict();

/** 描述进程管理页的完整视图；未运行的已保存路径仍保留在 selectedPaths 中。 */
export const processSelectionSnapshotSchema = z
  .object({
    enabled: z.boolean(),
    selectedPaths: z.array(z.string().min(1)),
    resolvedProcessIds: z.array(z.number().int().positive()),
    processes: z.array(processCandidateSchema),
    processIcons: z.record(
      z.string(),
      z.string().startsWith("data:image/png;base64,"),
    ),
  })
  .strict();

/** 提交稳定的可执行路径集合，运行时 PID 由后端实时解析。 */
export const processSelectionUpdateSchema = z
  .object({
    enabled: z.boolean(),
    selectedPaths: z.array(z.string().min(1)),
  })
  .strict();

export const publicConfigurationSchema = z
  .object({
    listenHost: z.string().min(1),
    listenPort: z.number().int().min(1).max(65535),
    authenticationMode: z.enum(["none", "password", "plugin"]),
    authenticationUsernames: z.array(z.string().min(1)),
    maxConnections: z.number().int().min(1).max(maximumConnections),
    connectTimeout: z.number().positive(),
    bindTimeout: z.number().positive(),
    idleTimeout: z.number().positive(),
    shutdownTimeout: z.number().positive().max(maximumShutdownTimeoutSeconds),
    readTimeout: z.number().positive(),
    relayBufferSize: z.number().int().min(1024).max(maximumRelayBufferSize),
    udpBindHost: z.string(),
    udpMaxPacketSize: z.number().int().min(512).max(maximumUdpPacketSize),
    httpProxy: httpProxyConfigurationSchema,
    upstreamProxy: publicUpstreamProxyConfigurationSchema,
    processCapture: processCaptureConfigurationSchema,
  })
  .strict();

export const configurationUpdateSchema = publicConfigurationSchema
  .omit({ authenticationUsernames: true, upstreamProxy: true })
  .extend({
    credentials: z
      .object({
        username: z.string().min(1),
        password: z.string().min(1),
      })
      .strict()
      .nullable(),
    upstreamProxy: upstreamProxyUpdateSchema,
  })
  .strict();

export const listenerStateSchema = z.enum([
  "disabled",
  "stopped",
  "running",
  "failed",
]);

export const listenerErrorSchema = z
  .object({
    code: z.string().min(1),
    messageKey: z.string().min(1),
    params: z.record(z.string()),
  })
  .strict();

export const listenerSnapshotSchema = z
  .object({
    enabled: z.boolean(),
    state: listenerStateSchema,
    boundEndpoint: z.string().nullable(),
    error: listenerErrorSchema.nullable(),
  })
  .strict();

export const listenersSnapshotSchema = z
  .object({
    socks5: listenerSnapshotSchema,
    httpProxy: listenerSnapshotSchema,
  })
  .strict();

export const locationPatternSchema = z
  .object({
    protocol: z.string(),
    host: z.string(),
    port: z.string(),
    path: z.string(),
    query: z.string().nullable(),
  })
  .strict();

export const sslConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    includeLocations: z.array(locationPatternSchema),
    excludeLocations: z.array(locationPatternSchema),
    maxCachedCertificates: z
      .number()
      .int()
      .positive()
      .max(maximumCachedCertificates),
    useClientSni: z.boolean(),
  })
  .strict();

export const certificateAuthorityInfoSchema = z
  .object({
    installed: z.boolean(),
    subject: z.string(),
    validFromMilliseconds: safeUnsignedIntegerSchema,
    validToMilliseconds: safeUnsignedIntegerSchema,
    fingerprintSha256: z.string().min(1),
    pemPath: z.string().min(1),
  })
  .strict()
  // 已安装证书的结束时间必须晚于起始时间，避免界面展示损坏证书元数据。
  .superRefine((certificate, context) => {
    if (
      certificate.installed &&
      certificate.validToMilliseconds <= certificate.validFromMilliseconds
    ) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["validToMilliseconds"],
        message: "certificate validity is inconsistent",
      });
    }
  });

/** 客户端证书来源格式；导入后后端统一保存为可直接握手的规范化材料。 */
export const clientCertificateFormatSchema = z.enum(["pkcs12", "pem", "der"]);

/** 客户端证书公开元数据；协议刻意排除私钥、口令和密钥路径。 */
export const clientCertificateInfoSchema = z
  .object({
    id: z.string().regex(/^[0-9a-f]{32}$/),
    name: z.string().min(1).max(80),
    format: clientCertificateFormatSchema,
    enabled: z.boolean(),
    locations: z.array(locationPatternSchema).min(1),
    subject: z.string().min(1),
    issuer: z.string().min(1),
    validFromMilliseconds: safeUnsignedIntegerSchema,
    validToMilliseconds: safeUnsignedIntegerSchema,
    fingerprintSha256: z.string().min(1),
  })
  .strict();

/** 更新客户端身份时只允许修改公开字段，密钥材料必须重新导入。 */
export const clientCertificateUpdateSchema = z
  .object({
    name: z.string().trim().min(1).max(80),
    enabled: z.boolean(),
    locations: z.array(locationPatternSchema).min(1),
  })
  .strict();

export const sslPublicStateSchema = sslConfigurationSchema
  .extend({
    ca: certificateAuthorityInfoSchema,
    cachedLeafCount: safeUnsignedIntegerSchema,
    handshakeSuccessTotal: safeUnsignedIntegerSchema,
    handshakeFailureTotal: safeUnsignedIntegerSchema,
    clientCertificates: z.array(clientCertificateInfoSchema).max(64),
    supportedHttpVersions: z.array(z.string().min(1)).min(1),
  })
  .strict()
  // 缓存实际项不得超过配置上限，防止错误状态把无界缓存伪装成有效协议。
  .superRefine((state, context) => {
    if (state.cachedLeafCount > state.maxCachedCertificates) {
      context.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["cachedLeafCount"],
        message: "leaf cache exceeds configured limit",
      });
    }
  });

/** 保留重复头与原始顺序，供工具规则与事务详情共用有线 HTTP 表示。 */
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

/** 录制规则动作按首条命中结果执行；REJECT 会在代理拥有连接时阻断请求。 */
export const recordingRuleActionSchema = z.enum([
  "record",
  "doNotRecord",
  "reject",
]);

/** 录制规则条件覆盖主机、地址、端口、进程、协议和 HTTP 方法。 */
export const recordingRuleKindSchema = z.enum([
  "domain",
  "domainSuffix",
  "domainKeyword",
  "destinationIpCidr",
  "clientIpCidr",
  "port",
  "processName",
  "protocol",
  "method",
  "match",
]);

/** 描述规则集中的一条有序规则；match 条件的值固定为空字符串。 */
export const recordingRuleSchema = z
  .object({
    id: z.string().min(1).max(64),
    enabled: z.boolean(),
    kind: recordingRuleKindSchema,
    value: z.string().max(512),
    action: recordingRuleActionSchema,
  })
  .strict();

/** 描述一组有名称的录制规则；组和规则均按界面顺序参与首条命中。 */
export const recordingRuleSetSchema = z
  .object({
    id: z.string().min(1).max(64),
    name: z.string().min(1).max(128),
    enabled: z.boolean(),
    rules: z.array(recordingRuleSchema).max(1_024),
  })
  .strict();

/** 描述可持久化的录制规则配置；未命中时执行 defaultAction。 */
export const recordingRuleConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    defaultAction: recordingRuleActionSchema,
    ruleSets: z.array(recordingRuleSetSchema).max(64),
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
    recordingRules: recordingRuleConfigurationSchema,
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

export type ServiceState = z.infer<typeof serviceStateSchema>;
export type SessionState = z.infer<typeof sessionStateSchema>;
export type ServiceMetrics = z.infer<typeof metricsSchema>;
export type SessionSnapshot = z.infer<typeof sessionSnapshotSchema>;
export type HttpProxyConfiguration = z.infer<
  typeof httpProxyConfigurationSchema
>;
export type PublicConfiguration = z.infer<typeof publicConfigurationSchema>;
export type ConfigurationUpdate = z.infer<typeof configurationUpdateSchema>;
export type ListenerSnapshot = z.infer<typeof listenerSnapshotSchema>;
export type ListenerSnapshots = z.infer<typeof listenersSnapshotSchema>;
export type LocationPattern = z.infer<typeof locationPatternSchema>;
export type SslConfiguration = z.infer<typeof sslConfigurationSchema>;
export type SslPublicState = z.infer<typeof sslPublicStateSchema>;
export type ClientCertificateFormat = z.infer<
  typeof clientCertificateFormatSchema
>;
export type ClientCertificateInfo = z.infer<typeof clientCertificateInfoSchema>;
export type ClientCertificateUpdate = z.infer<
  typeof clientCertificateUpdateSchema
>;
export type BlockListConfiguration = z.infer<
  typeof blockListConfigurationSchema
>;
export type RecordingRuleAction = z.infer<typeof recordingRuleActionSchema>;
export type RecordingRuleKind = z.infer<typeof recordingRuleKindSchema>;
export type RecordingRule = z.infer<typeof recordingRuleSchema>;
export type RecordingRuleSet = z.infer<typeof recordingRuleSetSchema>;
export type RecordingRuleConfiguration = z.infer<
  typeof recordingRuleConfigurationSchema
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
export type TransactionSummary = z.infer<typeof transactionSummarySchema>;
export type TransactionPage = z.infer<typeof transactionPageSchema>;
export type HeaderField = z.infer<typeof headerFieldSchema>;
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
export type EventMessage = z.infer<typeof eventMessageSchema>;
export type ProcessCandidate = z.infer<typeof processCandidateSchema>;
export type ProcessSelectionSnapshot = z.infer<
  typeof processSelectionSnapshotSchema
>;
export type ProcessSelectionUpdate = z.infer<
  typeof processSelectionUpdateSchema
>;
