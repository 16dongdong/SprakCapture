import type { ControlClient } from "@/api/controlClient";
import type {
  EncodedBodyResponse,
  DecodedProtobufView,
  ProtobufConfiguration,
  ServiceSnapshot,
  SessionSnapshot,
  TransactionDetail,
  TransactionSummary,
  ValidateConfiguration,
  ValidationReport,
} from "@/api/protocol";

const baseTimestamp = 1_720_000_000_000;
export const defaultServerInstanceId = "00000000-0000-4000-8000-000000000001";

/**
 * 创建严格协议形状的会话样本；覆盖字段只用于表达当前测试关注点。
 */
export function createSessionSnapshot(
  overrides: Partial<SessionSnapshot> = {},
): SessionSnapshot {
  return {
    sessionId: "session-alpha",
    clientAddress: "127.0.0.1:50100",
    username: "本地用户",
    command: "connect",
    targetAddress: "alpha.example:443",
    state: "relaying",
    bytesUp: 1536,
    bytesDown: 4096,
    createdAtMilliseconds: baseTimestamp,
    updatedAtMilliseconds: baseTimestamp + 2500,
    closedAtMilliseconds: 0,
    errorMessage: "",
    ...overrides,
  };
}

/**
 * 创建不含正文的事务摘要；默认值覆盖严格 schema 的 nullable 时序和工具标记字段。
 */
export function createTransactionSummary(
  overrides: Partial<TransactionSummary> = {},
): TransactionSummary {
  return {
    transactionId: "transaction-alpha",
    recordingSessionId: "recording-alpha",
    sequence: 1,
    protocol: "http",
    method: "GET",
    host: "alpha.example",
    port: 80,
    path: "/resource",
    query: "",
    urlDisplay: "http://alpha.example/resource",
    status: "complete",
    statusCode: 200,
    clientAddress: "127.0.0.1:50100",
    clientProcessName: null,
    clientProcessId: null,
    contentType: "text/plain",
    timings: {
      startAtMilliseconds: baseTimestamp,
      dnsEndAtMilliseconds: null,
      connectEndAtMilliseconds: baseTimestamp + 10,
      tlsEndAtMilliseconds: null,
      requestSentAtMilliseconds: baseTimestamp + 20,
      responseStartAtMilliseconds: baseTimestamp + 30,
      endAtMilliseconds: baseTimestamp + 40,
    },
    sizes: {
      requestHeaderBytes: 96,
      requestBodyBytes: 0,
      responseHeaderBytes: 128,
      responseBodyBytes: 12,
    },
    flags: {
      mappedLocal: false,
      mappedRemote: false,
      rewritten: false,
      breakpointHit: false,
      throttled: false,
      mitmDecrypted: false,
      bodyTruncated: false,
      headersTruncated: false,
      fromCache: false,
    },
    error: null,
    notes: "",
    tags: [],
    appliedTools: [],
    ...overrides,
  };
}

/**
 * 创建不含认证口令的服务快照；测试按 revision 模拟后端权威状态推进。
 */
export function createServiceSnapshot(
  overrides: Partial<ServiceSnapshot> = {},
): ServiceSnapshot {
  return {
    serverInstanceId: defaultServerInstanceId,
    revision: 1,
    serviceState: "stopped",
    metrics: {
      acceptedConnections: 2,
      activeConnections: 1,
      failedConnections: 0,
      bytesUp: 1536,
      bytesDown: 4096,
      udpPacketsUp: 0,
      udpPacketsDown: 0,
      droppedUdpPackets: 0,
    },
    sessions: [
      createSessionSnapshot(),
      createSessionSnapshot({
        sessionId: "session-beta",
        clientAddress: "127.0.0.1:50101",
        targetAddress: "beta.example:1080",
        state: "closed",
        bytesUp: 256,
        bytesDown: 512,
        createdAtMilliseconds: baseTimestamp + 1000,
        updatedAtMilliseconds: baseTimestamp + 3000,
        closedAtMilliseconds: baseTimestamp + 3000,
      }),
    ],
    configuration: {
      listenHost: "127.0.0.1",
      listenPort: 1080,
      authenticationMode: "none",
      authenticationUsernames: [],
      maxConnections: 1024,
      connectTimeout: 10,
      bindTimeout: 30,
      idleTimeout: 300,
      shutdownTimeout: 10,
      readTimeout: 30,
      relayBufferSize: 65_536,
      udpBindHost: "127.0.0.1",
      udpMaxPacketSize: 65_507,
      httpProxy: {
        enabled: true,
        listenHost: "127.0.0.1",
        listenPort: 1080,
        maxConnections: 512,
        maxHeaderBytes: 65_536,
        maxCaptureBodyBytes: 262_144,
        connectTimeoutMilliseconds: 10_000,
        requestTimeoutMilliseconds: 60_000,
        headerReadTimeoutMilliseconds: 15_000,
        shutdownTimeoutMilliseconds: 5_000,
      },
      upstreamProxy: {
        enabled: false,
        protocol: "socks5",
        host: "127.0.0.1",
        port: 1081,
        username: "",
        hasPassword: false,
      },
      processCapture: {
        enabled: false,
        processIds: [],
        proxyPort: 1080,
      },
    },
    processCapture: {
      running: false,
      configuredProcessIds: [],
      trackedFlows: 0,
      acceptedConnections: 0,
      redirectedPackets: 0,
      restoredPackets: 0,
      bytesUp: 0,
      bytesDown: 0,
      lastError: null,
    },
    listeners: {
      socks5: {
        enabled: true,
        state: "stopped",
        boundEndpoint: null,
        error: null,
      },
      httpProxy: {
        enabled: true,
        state: "stopped",
        boundEndpoint: null,
        error: null,
      },
    },
    ssl: {
      enabled: false,
      includeLocations: [],
      excludeLocations: [],
      maxCachedCertificates: 256,
      useClientSni: true,
      ca: {
        installed: true,
        subject: "CN=Local Proxy Root CA",
        validFromMilliseconds: baseTimestamp,
        validToMilliseconds: baseTimestamp + 3650 * 24 * 60 * 60 * 1000,
        fingerprintSha256: "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF",
        pemPath: "C:\\Users\\local\\certs\\rootCA.pem",
      },
      cachedLeafCount: 0,
      handshakeSuccessTotal: 0,
      handshakeFailureTotal: 0,
      clientCertificates: [],
      supportedHttpVersions: ["HTTP/1.0", "HTTP/1.1", "HTTP/2"],
    },
    tools: {
      pipelineOrder: [
        "recordingRules",
        "dnsSpoofing",
        "blockList",
        "noCaching",
        "blockCookies",
        "mapRemote",
        "mapLocal",
        "rewrite",
        "breakpoints",
        "throttling",
        "mirror",
        "autoSave",
        "packetFilters",
      ],
      recordingRules: {
        enabled: false,
        defaultAction: "record",
        ruleSets: [],
      },
      packetFilters: { enabled: false, rules: [] },
      blockList: {
        mode: "off",
        locations: [],
        statusCode: 403,
        responseBody: "",
        closeConnection: false,
      },
      noCaching: {
        enabled: false,
        locations: [],
        stripRequestHeaders: true,
        stripResponseHeaders: true,
        injectRequestNoCache: true,
        injectResponseNoStore: true,
      },
      blockCookies: {
        enabled: false,
        locations: [],
        stripRequestCookie: true,
        stripResponseSetCookie: true,
      },
      dnsSpoofing: { enabled: false, rules: [] },
      mapLocal: { enabled: false, rules: [] },
      mapRemote: { enabled: false, rules: [] },
      rewrite: { enabled: false, sets: [] },
      breakpoints: {
        enabled: false,
        rules: [],
        suspendTimeoutSeconds: 120,
        maxSuspended: 32,
        onTimeout: "continue",
      },
      throttling: {
        enabled: false,
        activePresetId: null,
        custom: {
          downloadBytesPerSecond: 12 * 1024 * 1024,
          uploadBytesPerSecond: 3 * 1024 * 1024,
          latencyMilliseconds: 50,
          latencyJitterMilliseconds: 0,
          reliabilityPercent: 100,
          mtu: 1500,
        },
        locations: [],
        presets: [
          {
            id: "lte",
            name: "LTE",
            downloadBytesPerSecond: 12 * 1024 * 1024,
            uploadBytesPerSecond: 3 * 1024 * 1024,
            latencyMilliseconds: 50,
            latencyJitterMilliseconds: 0,
            reliabilityPercent: 100,
            mtu: 1500,
          },
        ],
      },
      mirror: {
        enabled: false,
        rootDirectory: "",
        locations: [],
        mirrorRequest: true,
        mirrorResponse: true,
        layout: "hierarchical",
        onOverflow: "drop",
        maxQueueLength: 256,
        writtenFiles: 0,
        droppedWrites: 0,
        lastError: null,
      },
      autoSave: {
        enabled: false,
        directory: "",
        intervalSeconds: 0,
        everyNTransactions: 0,
        format: "native",
        maxFiles: 10,
        includeBodies: true,
        lastSavedAtMilliseconds: null,
        lastSavedPath: null,
        lastError: null,
      },
      suspendedBreakpointCount: 0,
    },
    recording: {
      recordingSessionId: "recording-alpha",
      state: "recording",
      startedAtMilliseconds: baseTimestamp,
      transactionCount: 0,
      droppedCount: 0,
      totalBodyBytes: 0,
      totalMetadataBytes: 0,
      metadataMemoryBudgetBytes: 256 * 1024 * 1024,
      pendingCleanupCount: 0,
      limits: {
        maxTransactions: 10_000,
        maxBodyBytes: 8 * 1024 * 1024,
        maxTotalBodyBytes: 256 * 1024 * 1024,
      },
      ignoreLocations: [],
      recordTunnelMetadata: true,
    },
    transactions: {
      revision: 1,
      recordingSessionId: "recording-alpha",
      collectionToken: "recording-alpha:0",
      total: 0,
      offset: 0,
      limit: 500,
      hasPrevious: false,
      hasMore: false,
      nextOffset: null,
      truncated: false,
      itemsTruncated: false,
      items: [],
    },
    advancedRepeats: [],
    plugins: [],
    ...overrides,
  };
}

/**
 * 创建覆盖完整控制接口的测试桩；默认响应共享同一快照，调用方只覆盖当前测试关注的方法。
 */
export function createControlClientStub(
  snapshot: ServiceSnapshot,
  overrides: Partial<ControlClient> = {},
): ControlClient {
  const transaction =
    snapshot.transactions.items[0] ?? createTransactionSummary();
  const transactionDetail: TransactionDetail = {
    revision: snapshot.revision,
    transaction,
    requestHeaders: [],
    responseHeaders: [],
    requestBody: null,
    responseBody: null,
    requestPackets: [],
    responsePackets: [],
  };
  /**
   * 为指定消息侧创建空正文响应；默认值满足正文长度与 Base64 的严格对应关系。
   */
  const createBodyResponse = (
    side: "request" | "response",
  ): EncodedBodyResponse => ({
    revision: snapshot.revision,
    meta: {
      transactionId: transaction.transactionId,
      side,
      contentType: "text/plain",
      encoding: "identity",
      storedBytes: 0,
      originalBytes: 0,
      truncated: false,
    },
    base64: "",
  });
  const recordingResponse = {
    serverInstanceId: snapshot.serverInstanceId,
    revision: snapshot.revision,
    recording: snapshot.recording,
  };
  const protobufView: DecodedProtobufView = {
    messageType: null,
    json: null,
    decodeError: "protobufDisabled",
  };
  const protobufConfiguration: ProtobufConfiguration = {
    enabled: false,
    schemas: [],
    routes: [],
  };
  const validateConfiguration: ValidateConfiguration = {
    enabled: true,
    validators: [
      { id: "htmlWellFormed", enabled: true },
      { id: "jsonSchema", enabled: false },
      { id: "w3cHtmlOnline", enabled: false },
    ],
    allowOnlineValidators: false,
    w3cEndpoint: "https://validator.w3.org/nu/?out=json",
  };
  const validationReport: ValidationReport = {
    transactionId: transaction.transactionId,
    validatorId: "htmlWellFormed",
    issues: [],
    validatedAtMilliseconds: snapshot.revision,
  };
  return {
    getSnapshot: async () => snapshot,
    startService: async () => snapshot,
    stopService: async () => snapshot,
    updateConfiguration: async () => snapshot,
    getProcesses: async () => ({
      enabled: false,
      selectedPaths: [],
      resolvedProcessIds: [],
      processes: [],
      processIcons: {},
    }),
    updateProcessSelection: async (update) => ({
      enabled: update.enabled,
      selectedPaths: update.selectedPaths,
      resolvedProcessIds: [],
      processes: [],
      processIcons: {},
    }),
    listPlugins: async () => [],
    getPluginDetails: async () => {
      throw new Error("测试夹具未配置插件详情");
    },
    setPluginEnabled: async () => {
      throw new Error("测试夹具未配置插件启停");
    },
    updatePluginConfiguration: async () => {
      throw new Error("测试夹具未配置插件设置");
    },
    reloadPlugin: async () => {
      throw new Error("测试夹具未配置插件重载");
    },
    installPluginPackage: async () => {
      throw new Error("测试夹具未配置插件安装");
    },
    uninstallPlugin: async () => {},
    getExtensionPlatformConfiguration: async () => ({
      schemaVersion: 1,
      plugins: {},
    }),
    updateExtensionPlatformConfiguration: async (pluginId, configuration) => ({
      schemaVersion: 1,
      plugins: { [pluginId]: configuration },
    }),
    removeExtensionPlatformConfiguration: async () => ({
      schemaVersion: 1,
      plugins: {},
    }),
    getExtensionRuntimeSnapshots: async () => [],
    getExtensionInvocationTraces: async () => [],
    clearExtensionInvocationTraces: async () => {},
    getSsl: async () => snapshot.ssl,
    updateSsl: async () => snapshot.ssl,
    regenerateSslRoot: async () => snapshot.ssl,
    exportSslRoot: async () => new Blob(["certificate"]),
    importClientCertificate: async () => snapshot.ssl,
    updateClientCertificate: async () => snapshot.ssl,
    removeClientCertificate: async () => snapshot.ssl,
    clearSessions: async () => snapshot,
    getRecording: async () => recordingResponse,
    updateRecording: async () => recordingResponse,
    clearRecording: async () => recordingResponse,
    listTransactions: async () => snapshot.transactions,
    getTransactionDetail: async () => transactionDetail,
    getRequestBody: async () => createBodyResponse("request"),
    getResponseBody: async () => createBodyResponse("response"),
    getResponseMediaPreview: async () => ({
      status: "complete",
      streamUrl: "http://127.0.0.1/media-preview",
      mimeType: "audio/mp4",
      capturedBytes: 0,
      totalBytes: 0,
      segmentCount: 1,
    }),
    composeRequest: async () => ({
      transactionId: transaction.transactionId,
      revision: snapshot.revision,
    }),
    repeatTransaction: async () => ({
      transactionId: transaction.transactionId,
      revision: snapshot.revision,
    }),
    startAdvancedRepeat: async () => ({
      jobId: "00000000-0000-4000-8000-000000000001",
      state: "queued",
      plan: {
        name: "test",
        base: {
          method: "GET",
          url: "http://alpha.example/resource",
          headers: [],
          bodyBase64: "",
          viaProxy: true,
        },
        concurrency: 1,
        totalIterations: 1,
        intervalMilliseconds: 0,
        recordEach: true,
        stopOnError: false,
      },
      startedAtMilliseconds: snapshot.revision,
      finishedAtMilliseconds: null,
      completedIterations: 0,
      successCount: 0,
      failureCount: 0,
      latencyMilliseconds: { min: 0, max: 0, p50: 0, p95: 0, p99: 0 },
      lastError: null,
    }),
    listAdvancedRepeats: async () => [],
    getAdvancedRepeat: async () => ({
      jobId: "00000000-0000-4000-8000-000000000001",
      state: "completed",
      plan: {
        name: "test",
        base: {
          method: "GET",
          url: "http://alpha.example/resource",
          headers: [],
          bodyBase64: "",
          viaProxy: true,
        },
        concurrency: 1,
        totalIterations: 1,
        intervalMilliseconds: 0,
        recordEach: true,
        stopOnError: false,
      },
      startedAtMilliseconds: snapshot.revision,
      finishedAtMilliseconds: snapshot.revision,
      completedIterations: 1,
      successCount: 1,
      failureCount: 0,
      latencyMilliseconds: { min: 0, max: 0, p50: 0, p95: 0, p99: 0 },
      lastError: null,
    }),
    cancelAdvancedRepeat: async () => ({
      jobId: "00000000-0000-4000-8000-000000000001",
      state: "cancelled",
      plan: {
        name: "test",
        base: {
          method: "GET",
          url: "http://alpha.example/resource",
          headers: [],
          bodyBase64: "",
          viaProxy: true,
        },
        concurrency: 1,
        totalIterations: 1,
        intervalMilliseconds: 0,
        recordEach: true,
        stopOnError: false,
      },
      startedAtMilliseconds: snapshot.revision,
      finishedAtMilliseconds: snapshot.revision,
      completedIterations: 0,
      successCount: 0,
      failureCount: 0,
      latencyMilliseconds: { min: 0, max: 0, p50: 0, p95: 0, p99: 0 },
      lastError: null,
    }),
    decodeProtobuf: async () => protobufView,
    getProtobufConfiguration: async () => protobufConfiguration,
    updateProtobufConfiguration: async () => protobufConfiguration,
    uploadProtobufDescriptor: async () => protobufConfiguration,
    getValidateConfiguration: async () => validateConfiguration,
    validateResponse: async () => validationReport,
    getValidationReports: async () => [],
    updateBlockList: async () => snapshot.tools,
    updateRecordingRules: async () => snapshot.tools,
    updatePacketFilters: async () => snapshot.tools,
    updateNoCaching: async () => snapshot.tools,
    updateBlockCookies: async () => snapshot.tools,
    updateDnsSpoofing: async () => snapshot.tools,
    updateMapLocal: async () => snapshot.tools,
    importMapLocalFiles: async () => ({
      localPath: "imports/test/fixture.json",
      fileCount: 1,
      totalBytes: 2,
    }),
    updateMapRemote: async () => snapshot.tools,
    updateRewrite: async () => snapshot.tools,
    updateBreakpoints: async () => snapshot.tools,
    updateThrottling: async () => snapshot.tools,
    updateMirror: async () => snapshot.tools,
    updateAutoSave: async () => snapshot.tools,
    saveAutoSaveNow: async () => snapshot.tools.autoSave,
    getReverseProxies: async () => ({
      configuration: { reverseProxies: [], portForwards: [] },
      bindings: { reverseProxies: [], portForwards: [] },
    }),
    updateReverseProxies: async () => snapshot,
    getPortForwards: async () => ({
      configuration: { reverseProxies: [], portForwards: [] },
      bindings: { reverseProxies: [], portForwards: [] },
    }),
    updatePortForwards: async () => snapshot,
    listSuspendedBreakpoints: async () => [],
    continueBreakpoint: async () => {},
    abortBreakpoint: async () => {},
    exportRecording: async () => new Blob(["{}"]),
    ...overrides,
  };
}
