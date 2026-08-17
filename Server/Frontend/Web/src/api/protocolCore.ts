import { z } from "zod";

import {
  maximumCachedCertificates,
  maximumConnections,
  maximumHttpCaptureBodyBytes,
  maximumHttpHeaderBytes,
  maximumHttpTimeoutMilliseconds,
  maximumRelayBufferSize,
  maximumShutdownTimeoutSeconds,
  maximumUdpPacketSize,
  safeUnsignedIntegerSchema,
} from "./protocolShared";

/** 定义服务配置、监听器、会话与 TLS 控制面的稳定线协议。 */
export const serverInstanceIdSchema = z.string().uuid();

/** 定义 Web 窗口类型；该字段只描述展示容器，不携带原生窗口句柄。 */
export const uiWindowKindSchema = z.enum(["main", "floating", "independent"]);

/** 定义可由 MCP 识别的稳定页面；动态路由参数通过 section 单独表达。 */
export const uiPageSchema = z.enum([
  "overview",
  "connections",
  "accountManagement",
  "settings",
  "plugins",
  "floating",
  "dialog",
]);

/** 定义当前选择的领域对象类型；正文仍由对应控制工具按 ID 查询。 */
export const uiSelectionKindSchema = z.enum([
  "transaction",
  "streamPacket",
  "account",
  "ruleSet",
]);

/** 描述页面当前选中的有界稳定标识，不复制事务、账号或规则正文。 */
export const uiDataSelectionSchema = z
  .object({
    kind: uiSelectionKindSchema,
    ids: z.array(z.string().min(1).max(128)).min(1).max(64),
    side: z.enum(["request", "response"]).nullable(),
    sequence: safeUnsignedIntegerSchema.nullable(),
  })
  .strict();

/** 接收浏览器的单调界面心跳；sequence 防止旧请求覆盖新选择。 */
export const uiContextUpdateSchema = z
  .object({
    instanceId: z.string().uuid(),
    sequence: safeUnsignedIntegerSchema.min(1),
    windowKind: uiWindowKindSchema,
    page: uiPageSchema,
    section: z.string().min(1).max(64).nullable(),
    view: z.string().min(1).max(64).nullable(),
    selection: uiDataSelectionSchema.nullable(),
    focused: z.boolean(),
    visible: z.boolean(),
  })
  .strict();

/** 返回服务端确认的界面上下文；updatedAt 只使用控制服务时钟。 */
export const uiContextSchema = uiContextUpdateSchema.extend({
  updatedAtMilliseconds: safeUnsignedIntegerSchema,
});

/** 聚合所有活跃窗口，并标出最符合当前用户操作的主窗口。 */
export const uiContextSnapshotSchema = z
  .object({
    primary: uiContextSchema.nullable(),
    contexts: z.array(uiContextSchema).max(32),
  })
  .strict();

export type UiDataSelection = z.infer<typeof uiDataSelectionSchema>;
export type UiContextUpdate = z.infer<typeof uiContextUpdateSchema>;
export type UiContextSnapshot = z.infer<typeof uiContextSnapshotSchema>;

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

/** 描述独立账号服务的公开运行快照；完整 API Key 绝不进入控制快照。 */
export const multiAccountSummarySchema = z
  .object({
    onlineAccounts: safeUnsignedIntegerSchema,
    activeConnections: safeUnsignedIntegerSchema,
    uploadBytesPerSecond: safeUnsignedIntegerSchema,
    downloadBytesPerSecond: safeUnsignedIntegerSchema,
  })
  .strict();

export const publicMultiAccountConfigurationSchema = z
  .object({
    enabled: z.boolean(),
    remoteHost: z.string().min(1),
    remotePort: z.number().int().min(1).max(65535),
    state: z.enum(["stopped", "starting", "running", "stopping", "faulted"]),
    apiKeyPrefix: z.string().nullable(),
    apiKeyCreatedAt: safeUnsignedIntegerSchema.nullable(),
    summary: multiAccountSummarySchema.nullable(),
    error: z.string().nullable(),
  })
  .strict();
export type MultiAccountPublicState = z.infer<
  typeof publicMultiAccountConfigurationSchema
>;

/** 配置写入只携带生命周期输入，运行状态和 Key 指纹始终由监督器生成。 */
export const multiAccountConfigurationUpdateSchema = z
  .object({
    enabled: z.boolean(),
    remoteHost: z.string().min(1),
    remotePort: z.number().int().min(1).max(65535),
  })
  .strict();

/** 管理员身份响应只承载可公开元数据；密码与完整 Key 不能进入该长期可缓存对象。 */
export const managementIdentitySchema = z
  .object({
    username: z.string().min(1),
    credentialRevision: safeUnsignedIntegerSchema,
    apiKeyPrefix: z.string().min(1),
    apiKeyCreatedAt: safeUnsignedIntegerSchema,
  })
  .strict();

/** 完整 Key 响应只允许由直接用户动作消费，调用方不得写入快照或持久化状态。 */
export const managementApiKeyResponseSchema = z
  .object({
    identity: managementIdentitySchema,
    apiKey: z.string().min(1),
  })
  .strict();

export type ManagementIdentity = z.infer<typeof managementIdentitySchema>;
export type ManagementApiKeyResponse = z.infer<
  typeof managementApiKeyResponseSchema
>;

/** 内部账号管理路由只返回一次性相对路径，禁止重新引入独立远程 URL。 */
export const managementSessionResponseSchema = z
  .object({ path: z.string().startsWith("/account-management/") })
  .strict();
export type ManagementSessionResponse = z.infer<
  typeof managementSessionResponseSchema
>;

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
    startServiceOnLaunch: z.boolean(),
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
    multiAccount: publicMultiAccountConfigurationSchema,
  })
  .strict();

export const configurationUpdateSchema = publicConfigurationSchema
  .omit({
    authenticationUsernames: true,
    upstreamProxy: true,
    multiAccount: true,
  })
  .extend({
    credentials: z
      .object({
        username: z.string().min(1),
        password: z.string().min(1),
      })
      .strict()
      .nullable(),
    upstreamProxy: upstreamProxyUpdateSchema,
    multiAccount: multiAccountConfigurationUpdateSchema,
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

export type ProcessCandidate = z.infer<typeof processCandidateSchema>;

export type ProcessSelectionSnapshot = z.infer<
  typeof processSelectionSnapshotSchema
>;

export type ProcessSelectionUpdate = z.infer<
  typeof processSelectionUpdateSchema
>;
