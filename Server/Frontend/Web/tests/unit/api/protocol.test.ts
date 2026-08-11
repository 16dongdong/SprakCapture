import { describe, expect, it } from "vitest";

import {
  encodedBodyResponseSchema,
  configurationUpdateSchema,
  eventMessageSchema,
  publicConfigurationSchema,
  recordingResponseSchema,
  recordingUpdateSchema,
  serviceSnapshotSchema,
  sessionSnapshotSchema,
  sslConfigurationSchema,
  sslPublicStateSchema,
  transactionDetailSchema,
  transactionPageSchema,
} from "@/api/protocol";
import {
  createServiceSnapshot,
  createSessionSnapshot,
  createTransactionSummary,
} from "#tests/testFixtures";

describe("控制协议校验", () => {
  it("接受英文机器状态和毫秒时间戳", () => {
    const parsedSnapshot = serviceSnapshotSchema.parse(
      createServiceSnapshot(),
    );

    expect(parsedSnapshot.sessions[0]?.state).toBe("relaying");
    expect(
      parsedSnapshot.sessions[0]?.createdAtMilliseconds,
    ).toBeGreaterThan(1_000_000_000_000);
    expect(parsedSnapshot.listeners.socks5.enabled).toBe(true);
    expect(parsedSnapshot.recording.state).toBe("recording");
    expect(parsedSnapshot.transactions.items).toEqual([]);
  });

  it("拒绝中文线状态和旧时间字段", () => {
    const invalidSession = {
      ...createSessionSnapshot(),
      state: "转发中",
      createdAt: 1_720_000_000,
    };
    Reflect.deleteProperty(invalidSession, "createdAtMilliseconds");

    expect(sessionSnapshotSchema.safeParse(invalidSession).success).toBe(false);
  });

  it("严格限制 SSL 公开状态、缓存上限和证书有效期", () => {
    const ssl = createServiceSnapshot().ssl;
    expect(sslPublicStateSchema.safeParse(ssl).success).toBe(true);
    expect(
      sslConfigurationSchema.safeParse({
        enabled: true,
        includeLocations: [],
        excludeLocations: [],
        maxCachedCertificates: 4097,
        useClientSni: true,
      }).success,
    ).toBe(false);
    expect(
      sslPublicStateSchema.safeParse({
        ...ssl,
        cachedLeafCount: ssl.maxCachedCertificates + 1,
      }).success,
    ).toBe(false);
    expect(
      sslPublicStateSchema.safeParse({
        ...ssl,
        ca: {
          ...ssl.ca,
          validToMilliseconds: ssl.ca.validFromMilliseconds,
        },
      }).success,
    ).toBe(false);
  });

  it("拒绝在公开配置中混入认证口令", () => {
    const validSnapshot = createServiceSnapshot();
    const unsafeSnapshot = {
      ...validSnapshot,
      configuration: {
        ...validSnapshot.configuration,
        password: "不应出现在快照中",
      },
    };

    expect(serviceSnapshotSchema.safeParse(unsafeSnapshot).success).toBe(false);
  });

  it("拒绝顶层监听地址和错误文案镜像字段", () => {
    const snapshot = createServiceSnapshot();
    const legacySnapshot = {
      ...snapshot,
      boundEndpoint: "127.0.0.1:1080",
      errorMessage: null,
    };
    const legacyListenerSnapshot = {
      ...snapshot,
      listeners: {
        ...snapshot.listeners,
        socks5: {
          ...snapshot.listeners.socks5,
          errorMessage: null,
        },
      },
    };

    expect(serviceSnapshotSchema.safeParse(legacySnapshot).success).toBe(false);
    expect(
      serviceSnapshotSchema.safeParse(legacyListenerSnapshot).success,
    ).toBe(false);
  });

  it("拒绝超过后端资源边界的配置", () => {
    const configuration = createServiceSnapshot().configuration;
    const invalidConfigurations = [
      { ...configuration, maxConnections: 16_385 },
      { ...configuration, shutdownTimeout: 30.1 },
      { ...configuration, relayBufferSize: 1_048_577 },
      { ...configuration, udpMaxPacketSize: 65_508 },
      {
        ...configuration,
        httpProxy: {
          ...configuration.httpProxy,
          maxHeaderBytes: 1024 * 1024 + 1,
        },
      },
    ];

    for (const invalidConfiguration of invalidConfigurations) {
      expect(
        publicConfigurationSchema.safeParse(invalidConfiguration).success,
      ).toBe(false);
    }
  });

  it("配置更新始终要求完整 HTTP 代理对象", () => {
    const {
      authenticationUsernames: _authenticationUsernames,
      ...editableConfiguration
    } = createServiceSnapshot().configuration;
    const completeUpdate = {
      ...editableConfiguration,
      upstreamProxy: {
        enabled: editableConfiguration.upstreamProxy.enabled,
        protocol: editableConfiguration.upstreamProxy.protocol,
        host: editableConfiguration.upstreamProxy.host,
        port: editableConfiguration.upstreamProxy.port,
        username: editableConfiguration.upstreamProxy.username,
        password: null,
      },
      credentials: null,
    };
    const updateWithoutHttpProxy = { ...completeUpdate };
    Reflect.deleteProperty(updateWithoutHttpProxy, "httpProxy");

    expect(configurationUpdateSchema.safeParse(completeUpdate).success).toBe(
      true,
    );
    expect(
      configurationUpdateSchema.safeParse(updateWithoutHttpProxy).success,
    ).toBe(false);
  });

  it("接受带 revision collection 的录制与事务事件", () => {
    const snapshot = createServiceSnapshot();
    const transaction = createTransactionSummary();
    const recordingEvent = eventMessageSchema.parse({
      type: "recording",
      serverInstanceId: snapshot.serverInstanceId,
      revision: 2,
      recording: {
        ...snapshot.recording,
        state: "paused",
      },
    });
    const transactionsEvent = eventMessageSchema.parse({
      type: "transactions",
      serverInstanceId: snapshot.serverInstanceId,
      revision: 3,
      transactions: {
        ...snapshot.transactions,
        revision: 3,
        total: 1,
        items: [transaction],
      },
    });

    expect(recordingEvent.type).toBe("recording");
    expect(transactionsEvent.type).toBe("transactions");
  });

  it("接受独立的 WinDivert 实时快照事件", () => {
    const snapshot = createServiceSnapshot();
    const processCaptureEvent = eventMessageSchema.parse({
      type: "processCapture",
      serverInstanceId: snapshot.serverInstanceId,
      revision: 2,
      processCapture: {
        running: true,
        configuredProcessIds: [1200, 2400],
        trackedFlows: 3,
        acceptedConnections: 5,
        redirectedPackets: 42,
        restoredPackets: 38,
        bytesUp: 4096,
        bytesDown: 8192,
        lastError: null,
      },
    });

    expect(processCaptureEvent.type).toBe("processCapture");
    if (processCaptureEvent.type !== "processCapture") {
      throw new Error("进程捕获事件判别失败");
    }
    expect(processCaptureEvent.processCapture.trackedFlows).toBe(3);
  });

  it("严格拒绝事务 collection 中的正文或未知字段", () => {
    const transaction = {
      ...createTransactionSummary(),
      responseBody: "不应进入摘要",
    };
    const invalidPage = {
      ...createServiceSnapshot().transactions,
      total: 1,
      items: [transaction],
    };

    expect(transactionPageSchema.safeParse(invalidPage).success).toBe(false);
  });

  // 新控制字段承载并发分页、真实页边界和内存预算；严格模型拒绝旧形状，避免静默丢失保护语义。
  it("严格要求集合令牌、下一页偏移、元数据预算与头截断标记", () => {
    const snapshot = createServiceSnapshot();
    const pageWithoutToken = { ...snapshot.transactions };
    const pageWithoutNextOffset = { ...snapshot.transactions };
    const recordingWithoutBudget = { ...snapshot.recording };
    const transactionWithoutHeaderFlag = createTransactionSummary();

    Reflect.deleteProperty(pageWithoutToken, "collectionToken");
    Reflect.deleteProperty(pageWithoutNextOffset, "nextOffset");
    Reflect.deleteProperty(recordingWithoutBudget, "metadataMemoryBudgetBytes");
    Reflect.deleteProperty(
      transactionWithoutHeaderFlag.flags,
      "headersTruncated",
    );

    expect(transactionPageSchema.safeParse(pageWithoutToken).success).toBe(
      false,
    );
    expect(
      transactionPageSchema.safeParse(pageWithoutNextOffset).success,
    ).toBe(false);
    expect(
      serviceSnapshotSchema.safeParse({
        ...snapshot,
        recording: recordingWithoutBudget,
      }).success,
    ).toBe(false);
    expect(
      transactionPageSchema.safeParse({
        ...snapshot.transactions,
        total: 1,
        items: [transactionWithoutHeaderFlag],
      }).success,
    ).toBe(false);
  });

  it("录制部分更新不允许重新启用正文裁剪或事务淘汰", () => {
    const snapshot = createServiceSnapshot();
    const validUpdate = {
      state: "paused",
    };

    expect(recordingUpdateSchema.safeParse(validUpdate).success).toBe(true);
    expect(
      recordingUpdateSchema.safeParse({
        ...validUpdate,
        unknownField: true,
      }).success,
    ).toBe(false);
    expect(
      recordingUpdateSchema.safeParse({
        limits: {
          maxTransactions: 1_000,
        },
      }).success,
    ).toBe(false);
    expect(
      recordingResponseSchema.safeParse({
        serverInstanceId: snapshot.serverInstanceId,
        revision: 2,
        recording: snapshot.recording,
      }).success,
    ).toBe(true);
  });

  it("严格校验事务详情、正文元信息和标准 base64", () => {
    const transactionId = "transaction-alpha";
    const bodyMeta = {
      transactionId,
      side: "response",
      contentType: "application/octet-stream",
      encoding: "identity",
      storedBytes: 3,
      originalBytes: 3,
      truncated: false,
    };
    const detail = {
      revision: 2,
      transaction: createTransactionSummary({ transactionId }),
      requestHeaders: [{ name: "accept", value: "*/*" }],
      responseHeaders: [],
      requestBody: null,
      responseBody: bodyMeta,
    };

    expect(transactionDetailSchema.safeParse(detail).success).toBe(true);
    expect(
      transactionDetailSchema.safeParse({
        ...detail,
        requestBody: bodyMeta,
      }).success,
    ).toBe(false);
    expect(
      encodedBodyResponseSchema.safeParse({
        revision: 3,
        meta: bodyMeta,
        base64: "AAEC",
      }).success,
    ).toBe(true);
    expect(
      encodedBodyResponseSchema.safeParse({
        revision: 3,
        meta: bodyMeta,
        base64: "AAEC_",
      }).success,
    ).toBe(false);
    expect(
      encodedBodyResponseSchema.safeParse({
        revision: 3,
        meta: {
          ...bodyMeta,
          storedBytes: 4,
          originalBytes: 4,
        },
        base64: "AAEC",
      }).success,
    ).toBe(false);
  });

  it("拒绝跨 revision、跨录制会话和不安全整数的混合响应", () => {
    const snapshot = createServiceSnapshot();
    const transaction = createTransactionSummary({
      recordingSessionId: "recording-other",
    });
    const mixedPage = {
      ...snapshot.transactions,
      total: 1,
      items: [transaction],
    };

    expect(transactionPageSchema.safeParse(mixedPage).success).toBe(false);
    expect(
      serviceSnapshotSchema.safeParse({
        ...snapshot,
        revision: 2,
      }).success,
    ).toBe(false);
    expect(
      serviceSnapshotSchema.safeParse({
        ...snapshot,
        transactions: {
          ...snapshot.transactions,
          recordingSessionId: "recording-other",
        },
      }).success,
    ).toBe(false);
    expect(
      eventMessageSchema.safeParse({
        type: "transactions",
        serverInstanceId: snapshot.serverInstanceId,
        revision: 2,
        transactions: {
          ...snapshot.transactions,
          revision: 1,
        },
      }).success,
    ).toBe(false);
    expect(
      recordingResponseSchema.safeParse({
        serverInstanceId: snapshot.serverInstanceId,
        revision: Number.MAX_SAFE_INTEGER + 1,
        recording: snapshot.recording,
      }).success,
    ).toBe(false);
    expect(
      transactionPageSchema.safeParse({
        ...snapshot.transactions,
        collectionToken: "x".repeat(129),
      }).success,
    ).toBe(false);
  });

  it("严格校验后台实例标识及完整事件身份一致性", () => {
    const snapshot = createServiceSnapshot();
    expect(
      serviceSnapshotSchema.safeParse({
        ...snapshot,
        serverInstanceId: "非 UUID",
      }).success,
    ).toBe(false);
    expect(
      eventMessageSchema.safeParse({
        type: "snapshot",
        serverInstanceId: "00000000-0000-4000-8000-000000000002",
        snapshot,
      }).success,
    ).toBe(false);
    expect(
      eventMessageSchema.safeParse({
        type: "metrics",
        revision: 2,
        metrics: snapshot.metrics,
      }).success,
    ).toBe(false);
  });
});
