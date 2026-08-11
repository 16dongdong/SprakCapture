import { act, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { ControlClient } from "@/api/controlClient";
import type {
  AdvancedRepeatJob,
  ConfigurationUpdate,
  DecodedProtobufView,
  ProtobufConfiguration,
  EncodedBodyResponse,
  RecordingResponse,
  ServiceSnapshot,
  TransactionDetail,
  ValidateConfiguration,
  ValidationReport,
} from "@/api/protocol";
import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import {
  createServiceSnapshot,
  createSessionSnapshot,
  createTransactionSummary,
  defaultServerInstanceId,
} from "#tests/testFixtures";
import { ServiceProvider, useServiceStore } from "@/state/serviceStore";

const restartedServerInstanceId = "00000000-0000-4000-8000-000000000002";

/**
 * 输出传输层关键状态，使连续增量合并结果可以通过界面状态验证。
 */
function SnapshotProbe() {
  const { snapshot } = useServiceStore();
  return (
    <output>
      {snapshot === null
        ? "等待快照"
        : [
            snapshot.sessions.length,
            snapshot.metrics.acceptedConnections,
            snapshot.recording.state,
            snapshot.transactions.total,
            snapshot.listeners.httpProxy.boundEndpoint ?? "-",
            snapshot.serviceState,
          ].join(":")}
    </output>
  );
}

/** 记录共享快照引用真正提交到 React 的次数，用于验证同 revision 事件幂等。 */
function SnapshotRenderProbe({ onRender }: { onRender(): void }) {
  const { snapshot } = useServiceStore();
  if (snapshot !== null) {
    onRender();
  }
  return null;
}

/** 输出透明捕获实时指标，验证独立事件无需依赖代理会话即可推进界面。 */
function ProcessCaptureProbe() {
  const { snapshot } = useServiceStore();
  return (
    <output>
      {snapshot
        ? `${snapshot.processCapture.trackedFlows}:${snapshot.processCapture.redirectedPackets}:${snapshot.processCapture.restoredPackets}`
        : "等待捕获快照"}
    </output>
  );
}

/** 输出实时高级重复作业进度，验证界面不再依赖固定频率 GET。 */
function AdvancedRepeatProbe() {
  const { snapshot } = useServiceStore();
  const job = snapshot?.advancedRepeats[0] ?? null;
  return (
    <output>
      {job === null
        ? "无重复作业"
        : `${job.state}:${job.completedIterations}:${job.successCount}`}
    </output>
  );
}

/** 创建满足严格协议的高级重复作业；覆盖项只用于表达测试中的状态推进。 */
function createAdvancedRepeatJob(
  overrides: Partial<AdvancedRepeatJob> = {},
): AdvancedRepeatJob {
  return {
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
    startedAtMilliseconds: 1,
    finishedAtMilliseconds: null,
    completedIterations: 0,
    successCount: 0,
    failureCount: 0,
    latencyMilliseconds: { min: 0, max: 0, p50: 0, p95: 0, p99: 0 },
    lastError: null,
    ...overrides,
  };
}

let accessedStore: ReturnType<typeof useServiceStore> | null = null;

/**
 * 返回 Provider 最近一次渲染暴露的 Store；缺失表示测试装配尚未完成并立即失败。
 */
function requireAccessedStore(): ReturnType<typeof useServiceStore> {
  if (accessedStore === null) {
    throw new Error("测试 Store 尚未装配");
  }
  return accessedStore;
}

/**
 * 暴露测试 Provider 的当前 Store 引用；生产代码仍只通过 Context 读取状态。
 */
function StoreAccessProbe() {
  accessedStore = useServiceStore();
  return null;
}

interface BroadcastChannelHarness {
  channel: BroadcastChannel;
  emit(message: unknown): void;
}

/**
 * 创建内存 BroadcastChannel；测试可精确注入迟到消息并观察 Store 是否错误切换后台代际。
 */
function createBroadcastChannelHarness(): BroadcastChannelHarness {
  const channel = Object.assign(new EventTarget(), {
    name: "测试控制状态广播",
    onmessage: null,
    onmessageerror: null,
    postMessage: vi.fn(),
    close: vi.fn(),
  }) as BroadcastChannel;
  return {
    channel,
    emit(message: unknown) {
      channel.dispatchEvent(new MessageEvent("message", { data: message }));
    },
  };
}

/**
 * 创建满足现有控制界面的固定客户端；配置提交函数可注入以观察 Store 的原样传输。
 */
function createControlClient(
  snapshot: ServiceSnapshot,
  updateConfiguration: (
    update: ConfigurationUpdate,
  ) => Promise<ServiceSnapshot> = async () => snapshot,
): ControlClient {
  const recordingResponse: RecordingResponse = {
    serverInstanceId: snapshot.serverInstanceId,
    revision: snapshot.revision,
    recording: snapshot.recording,
  };
  const transactionDetail: TransactionDetail = {
    revision: snapshot.revision,
    transaction: createTransactionSummary(),
    requestHeaders: [],
    responseHeaders: [],
    requestBody: null,
    responseBody: null,
    requestPackets: [],
    responsePackets: [],
  };
  const emptyBody: EncodedBodyResponse = {
    revision: snapshot.revision,
    meta: {
      transactionId: transactionDetail.transaction.transactionId,
      side: "request",
      contentType: "text/plain",
      encoding: "identity",
      storedBytes: 0,
      originalBytes: 0,
      truncated: false,
    },
    base64: "",
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
    transactionId: transactionDetail.transaction.transactionId,
    validatorId: "htmlWellFormed",
    issues: [],
    validatedAtMilliseconds: snapshot.revision,
  };
  return {
    getSnapshot: async () => snapshot,
    startService: async () => snapshot,
    stopService: async () => snapshot,
    updateConfiguration,
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
      throw new Error("测试客户端未配置插件详情");
    },
    setPluginEnabled: async () => {
      throw new Error("测试客户端未配置插件启停");
    },
    updatePluginConfiguration: async () => {
      throw new Error("测试客户端未配置插件设置");
    },
    reloadPlugin: async () => {
      throw new Error("测试客户端未配置插件重载");
    },
    installPluginPackage: async () => {
      throw new Error("测试客户端未配置插件安装");
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
    getRequestBody: async () => emptyBody,
    getResponseBody: async () => ({
      ...emptyBody,
      meta: { ...emptyBody.meta, side: "response" },
    }),
    getResponseMediaPreview: async () => ({
      status: "complete",
      streamUrl: "http://127.0.0.1/media-preview",
      mimeType: "audio/mp4",
      capturedBytes: 0,
      totalBytes: 0,
      segmentCount: 1,
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
    composeRequest: async () => ({
      transactionId: transactionDetail.transaction.transactionId,
      revision: snapshot.revision,
    }),
    repeatTransaction: async () => ({
      transactionId: transactionDetail.transaction.transactionId,
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
  };
}

describe("服务状态事件合并", () => {
  it("实时合并高级重复作业进度", async () => {
    const initialSnapshot = createServiceSnapshot({ revision: 1 });
    const broadcastHarness = createBroadcastChannelHarness();
    const onSnapshotRender = vi.fn();
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      /** 暴露测试回调并建立事件连接，不创建真实网络资源。 */
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      /** 内存事件客户端没有待释放资源。 */
      stop() {},
    };

    render(
      <ServiceProvider
        controlClient={createControlClient(initialSnapshot)}
        eventClient={eventClient}
        broadcastChannelFactory={() => broadcastHarness.channel}
      >
        <AdvancedRepeatProbe />
        <SnapshotRenderProbe onRender={onSnapshotRender} />
      </ServiceProvider>,
    );
    await screen.findByText("无重复作业");
    await waitFor(() => expect(callbacks).not.toBeNull());
    vi.mocked(broadcastHarness.channel.postMessage).mockClear();

    const progressEvent = {
      type: "advancedRepeats" as const,
      serverInstanceId: defaultServerInstanceId,
      revision: 2,
      jobs: [
        createAdvancedRepeatJob({
          state: "running",
          completedIterations: 1,
          successCount: 1,
        }),
      ],
    };
    act(() => {
      callbacks?.onMessage(progressEvent);
    });

    expect(screen.getByText("running:1:1")).toBeInTheDocument();
    const renderCountAfterFirstEvent = onSnapshotRender.mock.calls.length;
    act(() => callbacks?.onMessage(progressEvent));
    expect(onSnapshotRender).toHaveBeenCalledTimes(renderCountAfterFirstEvent);
    // 每个窗口已有独立 SSE；局部混合投影不再复制为不满足 schema 的完整跨窗快照。
    expect(broadcastHarness.channel.postMessage).not.toHaveBeenCalled();
  });

  it("独立合并 WinDivert 流表和数据包计数", async () => {
    const initialSnapshot = createServiceSnapshot({ revision: 1 });
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };

    render(
      <ServiceProvider
        controlClient={createControlClient(initialSnapshot)}
        eventClient={eventClient}
      >
        <ProcessCaptureProbe />
      </ServiceProvider>,
    );
    await screen.findByText("0:0:0");
    await waitFor(() => expect(callbacks).not.toBeNull());

    act(() => {
      callbacks?.onMessage({
        type: "processCapture",
        serverInstanceId: defaultServerInstanceId,
        revision: 2,
        processCapture: {
          running: true,
          configuredProcessIds: [1200],
          trackedFlows: 4,
          acceptedConnections: 6,
          redirectedPackets: 96,
          restoredPackets: 88,
          bytesUp: 4096,
          bytesDown: 8192,
          lastError: null,
        },
      });
    });

    expect(screen.getByText("4:96:88")).toBeInTheDocument();
  });

  it("同一事件循环内连续消息不会用旧快照覆盖前一增量", async () => {
    const initialSnapshot = createServiceSnapshot({
      revision: 1,
      sessions: [],
      metrics: {
        acceptedConnections: 0,
        activeConnections: 0,
        failedConnections: 0,
        bytesUp: 0,
        bytesDown: 0,
        udpPacketsUp: 0,
        udpPacketsDown: 0,
        droppedUdpPackets: 0,
      },
    });
    const controlClient = createControlClient(initialSnapshot);
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };

    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <SnapshotProbe />
      </ServiceProvider>,
    );
    await screen.findByText("0:0:recording:0:-:stopped");
    await waitFor(() => expect(callbacks).not.toBeNull());

    act(() => {
      callbacks?.onMessage({
        type: "sessions",
        serverInstanceId: defaultServerInstanceId,
        revision: 2,
        sessions: [createSessionSnapshot()],
      });
      callbacks?.onMessage({
        type: "metrics",
        serverInstanceId: defaultServerInstanceId,
        revision: 3,
        metrics: {
          ...initialSnapshot.metrics,
          acceptedConnections: 7,
        },
      });
    });

    expect(screen.getByText("1:7:recording:0:-:stopped")).toBeInTheDocument();

    act(() => {
      callbacks?.onMessage({
        type: "recording",
        serverInstanceId: defaultServerInstanceId,
        revision: 4,
        recording: {
          ...initialSnapshot.recording,
          state: "paused",
        },
      });
      callbacks?.onMessage({
        type: "transactions",
        serverInstanceId: defaultServerInstanceId,
        revision: 5,
        transactions: {
          ...initialSnapshot.transactions,
          revision: 5,
          total: 1,
          items: [createTransactionSummary()],
        },
      });
    });

    expect(screen.getByText("1:7:paused:1:-:stopped")).toBeInTheDocument();

    act(() => {
      callbacks?.onMessage({
        type: "serviceState",
        serverInstanceId: defaultServerInstanceId,
        revision: 6,
        serviceState: "running",
        listeners: {
          ...initialSnapshot.listeners,
          httpProxy: {
            enabled: true,
            state: "running",
            boundEndpoint: "127.0.0.1:8888",
            error: null,
          },
        },
      });
    });

    expect(
      screen.getByText("1:7:paused:1:127.0.0.1:8888:running"),
    ).toBeInTheDocument();
  });

  it("全局修订已被其他投影推进时仍接收更新的 WebSocket 事务页", async () => {
    const initialSnapshot = createServiceSnapshot({
      revision: 1,
      transactions: {
        ...createServiceSnapshot().transactions,
        revision: 1,
      },
    });
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };

    render(
      <ServiceProvider
        controlClient={createControlClient(initialSnapshot)}
        eventClient={eventClient}
      >
        <SnapshotProbe />
      </ServiceProvider>,
    );
    await screen.findByText("2:2:recording:0:-:stopped");

    act(() => {
      // HTTP/BroadcastChannel 与 WebSocket 使用不同调度队列，指标投影可能先到；
      // 事务页必须按自身 revision 合并，不能被无关的全局 revision 丢弃。
      callbacks?.onMessage({
        type: "metrics",
        serverInstanceId: defaultServerInstanceId,
        revision: 3,
        metrics: {
          ...initialSnapshot.metrics,
          acceptedConnections: 9,
        },
      });
      callbacks?.onMessage({
        type: "transactions",
        serverInstanceId: defaultServerInstanceId,
        revision: 2,
        transactions: {
          ...initialSnapshot.transactions,
          revision: 2,
          total: 1,
          items: [createTransactionSummary()],
        },
      });
    });

    expect(screen.getByText("2:9:recording:1:-:stopped")).toBeInTheDocument();
  });

  it("后台重启后接受新实例低 revision并拒绝旧跨窗口快照", async () => {
    accessedStore = null;
    const baseSnapshot = createServiceSnapshot();
    const oldSnapshot = createServiceSnapshot({
      revision: 80,
      recording: {
        ...baseSnapshot.recording,
        startedAtMilliseconds: 2_000,
      },
      transactions: {
        ...baseSnapshot.transactions,
        revision: 80,
      },
    });
    const transaction = createTransactionSummary({
      recordingSessionId: "recording-restarted",
    });
    const restartedSnapshot = createServiceSnapshot({
      serverInstanceId: restartedServerInstanceId,
      revision: 2,
      serviceState: "running",
      sessions: [],
      metrics: {
        ...baseSnapshot.metrics,
        acceptedConnections: 0,
      },
      recording: {
        ...baseSnapshot.recording,
        recordingSessionId: "recording-restarted",
        // 新进程墙钟即使倒退，随机实例标识仍能明确建立新代际。
        startedAtMilliseconds: 1_000,
        transactionCount: 1,
      },
      transactions: {
        ...baseSnapshot.transactions,
        revision: 2,
        recordingSessionId: "recording-restarted",
        collectionToken: "recording-restarted:1",
        total: 1,
        items: [transaction],
      },
    });
    const getSnapshot = vi
      .fn<() => Promise<ServiceSnapshot>>()
      .mockResolvedValueOnce(oldSnapshot)
      .mockResolvedValue(restartedSnapshot);
    const controlClient: ControlClient = {
      ...createControlClient(oldSnapshot),
      getSnapshot,
    };
    const eventClient: EventStreamClient = {
      start(callbacks) {
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    const broadcastHarness = createBroadcastChannelHarness();

    render(
      <ServiceProvider
        broadcastChannelFactory={() => broadcastHarness.channel}
        controlClient={controlClient}
        eventClient={eventClient}
      >
        <SnapshotProbe />
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await screen.findByText("2:2:recording:0:-:stopped");
    await act(async () => {
      await requireAccessedStore().refresh();
    });
    expect(screen.getByText("0:0:recording:1:-:running")).toBeInTheDocument();
    act(() => {
      broadcastHarness.emit({
        type: "snapshot",
        serverInstanceId: oldSnapshot.serverInstanceId,
        snapshot: oldSnapshot,
      });
    });
    expect(requireAccessedStore().snapshot?.serverInstanceId).toBe(
      restartedServerInstanceId,
    );
    expect(screen.getByText("0:0:recording:1:-:running")).toBeInTheDocument();
  });

  it("新实例确认后拒绝旧 HTTP 在途快照切回", async () => {
    accessedStore = null;
    const initialSnapshot = createServiceSnapshot({
      revision: 80,
      transactions: {
        ...createServiceSnapshot().transactions,
        revision: 80,
      },
    });
    const staleHttpSnapshot = createServiceSnapshot({
      revision: 81,
      transactions: {
        ...initialSnapshot.transactions,
        revision: 81,
      },
    });
    const restartedSnapshot = createServiceSnapshot({
      serverInstanceId: restartedServerInstanceId,
      revision: 2,
      serviceState: "running",
      sessions: [],
      transactions: {
        ...initialSnapshot.transactions,
        revision: 2,
      },
    });
    let resolveStaleHttpSnapshot!: (snapshot: ServiceSnapshot) => void;
    const staleHttpResult = new Promise<ServiceSnapshot>((resolve) => {
      resolveStaleHttpSnapshot = resolve;
    });
    const getSnapshot = vi
      .fn<() => Promise<ServiceSnapshot>>()
      .mockResolvedValueOnce(initialSnapshot)
      .mockReturnValueOnce(staleHttpResult)
      .mockResolvedValue(restartedSnapshot);
    const controlClient: ControlClient = {
      ...createControlClient(initialSnapshot),
      getSnapshot,
    };
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <SnapshotProbe />
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await waitFor(() =>
      expect(accessedStore?.controlConnection).toBe("connected"),
    );

    let refreshPromise!: Promise<void>;
    act(() => {
      refreshPromise = requireAccessedStore().refresh();
    });
    act(() => {
      callbacks?.onMessage({
        type: "snapshot",
        serverInstanceId: restartedSnapshot.serverInstanceId,
        snapshot: restartedSnapshot,
      });
    });
    await waitFor(() =>
      expect(requireAccessedStore().snapshot?.serverInstanceId).toBe(
        restartedServerInstanceId,
      ),
    );

    await act(async () => {
      resolveStaleHttpSnapshot(staleHttpSnapshot);
      await refreshPromise;
    });

    expect(requireAccessedStore().snapshot?.serverInstanceId).toBe(
      restartedServerInstanceId,
    );
    expect(requireAccessedStore().snapshot?.revision).toBe(2);
  });

  it("HTTP 已确认新实例后拒绝旧事件流首个完整快照", async () => {
    accessedStore = null;
    const oldSnapshot = createServiceSnapshot({
      revision: 80,
      transactions: {
        ...createServiceSnapshot().transactions,
        revision: 80,
      },
    });
    const restartedSnapshot = createServiceSnapshot({
      serverInstanceId: restartedServerInstanceId,
      revision: 2,
      serviceState: "running",
      transactions: {
        ...oldSnapshot.transactions,
        revision: 2,
      },
    });
    const getSnapshot = vi
      .fn<() => Promise<ServiceSnapshot>>()
      .mockResolvedValue(restartedSnapshot);
    const controlClient: ControlClient = {
      ...createControlClient(restartedSnapshot),
      getSnapshot,
    };
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };

    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <SnapshotProbe />
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await waitFor(() =>
      expect(accessedStore?.snapshot?.serverInstanceId).toBe(
        restartedServerInstanceId,
      ),
    );

    act(() => {
      callbacks?.onMessage({
        type: "snapshot",
        serverInstanceId: oldSnapshot.serverInstanceId,
        snapshot: oldSnapshot,
      });
    });

    await waitFor(() => expect(getSnapshot).toHaveBeenCalledTimes(2));
    expect(requireAccessedStore().snapshot?.serverInstanceId).toBe(
      restartedServerInstanceId,
    );
    expect(requireAccessedStore().snapshot?.revision).toBe(2);
  });

  it("事件流重连到新实例后由 HTTP 仲裁并切换快照代际", async () => {
    accessedStore = null;
    const oldSnapshot = createServiceSnapshot({
      revision: 80,
      transactions: {
        ...createServiceSnapshot().transactions,
        revision: 80,
      },
    });
    const restartedSnapshot = createServiceSnapshot({
      serverInstanceId: restartedServerInstanceId,
      revision: 2,
      serviceState: "running",
      transactions: {
        ...oldSnapshot.transactions,
        revision: 2,
      },
    });
    const getSnapshot = vi
      .fn<() => Promise<ServiceSnapshot>>()
      .mockResolvedValueOnce(oldSnapshot)
      .mockResolvedValue(restartedSnapshot);
    const controlClient: ControlClient = {
      ...createControlClient(oldSnapshot),
      getSnapshot,
    };
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };

    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <SnapshotProbe />
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await waitFor(() =>
      expect(accessedStore?.snapshot?.serverInstanceId).toBe(
        oldSnapshot.serverInstanceId,
      ),
    );

    act(() => {
      callbacks?.onConnectionState("connecting", "正在重新连接事件流");
      callbacks?.onMessage({
        type: "snapshot",
        serverInstanceId: restartedSnapshot.serverInstanceId,
        snapshot: restartedSnapshot,
      });
    });

    await waitFor(() =>
      expect(accessedStore?.snapshot?.serverInstanceId).toBe(
        restartedServerInstanceId,
      ),
    );
    expect(getSnapshot).toHaveBeenCalledTimes(2);
    expect(requireAccessedStore().snapshot?.revision).toBe(2);
  });

  it("较旧录制动作响应不会回退事件流已经确认的快照", async () => {
    accessedStore = null;
    const initialSnapshot = createServiceSnapshot({ revision: 1 });
    const staleResponse: RecordingResponse = {
      serverInstanceId: initialSnapshot.serverInstanceId,
      revision: 8,
      recording: {
        ...initialSnapshot.recording,
        state: "paused",
      },
    };
    const latestSnapshot = createServiceSnapshot({
      revision: 10,
      serviceState: "running",
      sessions: [createSessionSnapshot()],
      recording: {
        ...initialSnapshot.recording,
        state: "recording",
        transactionCount: 1,
      },
      transactions: {
        ...initialSnapshot.transactions,
        revision: 10,
        total: 1,
        items: [createTransactionSummary()],
      },
    });
    let resolveRecordingResult!: (response: RecordingResponse) => void;
    const recordingResult = new Promise<RecordingResponse>((resolve) => {
      resolveRecordingResult = resolve;
    });
    const controlClient: ControlClient = {
      ...createControlClient(initialSnapshot),
      updateRecording: () => recordingResult,
    };
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };

    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <SnapshotProbe />
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await screen.findByText("2:2:recording:0:-:stopped");
    await waitFor(() =>
      expect(accessedStore?.controlConnection).toBe("connected"),
    );

    let togglePromise!: Promise<void>;
    act(() => {
      togglePromise = accessedStore!.toggleRecording();
    });
    act(() => {
      callbacks?.onMessage({
        type: "snapshot",
        serverInstanceId: latestSnapshot.serverInstanceId,
        snapshot: latestSnapshot,
      });
    });
    expect(screen.getByText("1:2:recording:1:-:running")).toBeInTheDocument();

    await act(async () => {
      resolveRecordingResult(staleResponse);
      await togglePromise;
    });
    act(() => {
      callbacks?.onMessage({
        type: "metrics",
        serverInstanceId: latestSnapshot.serverInstanceId,
        revision: 11,
        metrics: {
          ...latestSnapshot.metrics,
          acceptedConnections: 11,
        },
      });
    });

    expect(screen.getByText("1:11:recording:1:-:running")).toBeInTheDocument();
  });

  it("同一事件循环重复切换服务只发送一次控制请求", async () => {
    accessedStore = null;
    const initialSnapshot = createServiceSnapshot({ revision: 1 });
    const runningSnapshot = createServiceSnapshot({
      revision: 2,
      serviceState: "running",
      transactions: {
        ...initialSnapshot.transactions,
        revision: 2,
      },
    });
    let resolveStartService!: (snapshot: ServiceSnapshot) => void;
    const startResult = new Promise<ServiceSnapshot>((resolve) => {
      resolveStartService = resolve;
    });
    const startService = vi.fn(() => startResult);
    const controlClient: ControlClient = {
      ...createControlClient(initialSnapshot),
      startService,
    };
    const eventClient: EventStreamClient = {
      start(callbacks) {
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await waitFor(() =>
      expect(accessedStore?.controlConnection).toBe("connected"),
    );

    let firstToggle!: Promise<void>;
    let secondToggle!: Promise<void>;
    act(() => {
      firstToggle = requireAccessedStore().toggleService();
      secondToggle = requireAccessedStore().toggleService();
    });

    expect(startService).toHaveBeenCalledOnce();
    expect(requireAccessedStore().actionPending).toBe(true);
    expect(requireAccessedStore().activeAction).toBe("service");

    await act(async () => {
      resolveStartService(runningSnapshot);
      await Promise.all([firstToggle, secondToggle]);
    });

    expect(startService).toHaveBeenCalledOnce();
    expect(requireAccessedStore().actionPending).toBe(false);
    expect(requireAccessedStore().activeAction).toBeNull();
    expect(requireAccessedStore().snapshot?.serviceState).toBe("running");
  });

  it("切换录制期间保留旧事务页并用完整快照原子替换", async () => {
    accessedStore = null;
    const transaction = createTransactionSummary();
    const baseSnapshot = createServiceSnapshot();
    const initialSnapshot = createServiceSnapshot({
      revision: 1,
      recording: {
        ...baseSnapshot.recording,
        transactionCount: 1,
      },
      transactions: {
        ...baseSnapshot.transactions,
        revision: 1,
        total: 1,
        items: [transaction],
      },
    });
    const synchronizedSnapshot = createServiceSnapshot({
      revision: 2,
      recording: {
        ...initialSnapshot.recording,
        state: "paused",
      },
      transactions: {
        ...initialSnapshot.transactions,
        revision: 2,
      },
    });
    let resolveSnapshot!: (snapshot: ServiceSnapshot) => void;
    const synchronizedResult = new Promise<ServiceSnapshot>((resolve) => {
      resolveSnapshot = resolve;
    });
    const getSnapshot = vi
      .fn<() => Promise<ServiceSnapshot>>()
      .mockResolvedValueOnce(initialSnapshot)
      .mockReturnValueOnce(synchronizedResult);
    const updateRecording = vi.fn(async (): Promise<RecordingResponse> => ({
      serverInstanceId: initialSnapshot.serverInstanceId,
      revision: 2,
      recording: synchronizedSnapshot.recording,
    }));
    const controlClient: ControlClient = {
      ...createControlClient(initialSnapshot),
      getSnapshot,
      updateRecording,
    };
    const eventClient: EventStreamClient = {
      start(callbacks) {
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <SnapshotProbe />
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await screen.findByText("2:2:recording:1:-:stopped");
    await waitFor(() =>
      expect(accessedStore?.controlConnection).toBe("connected"),
    );

    let togglePromise!: Promise<void>;
    act(() => {
      togglePromise = requireAccessedStore().toggleRecording();
    });
    await waitFor(() => {
      expect(updateRecording).toHaveBeenCalledOnce();
      expect(getSnapshot).toHaveBeenCalledTimes(2);
    });

    // 录制写响应不含事务页，等待完整快照期间继续渲染上一份一致数据，
    // 防止工作区卸载后重建造成白屏、分栏抖动以及当前选择丢失。
    expect(screen.queryByText("等待快照")).not.toBeInTheDocument();
    expect(requireAccessedStore().snapshot).toBe(initialSnapshot);
    expect(screen.getByText("2:2:recording:1:-:stopped")).toBeInTheDocument();

    await act(async () => {
      resolveSnapshot(synchronizedSnapshot);
      await togglePromise;
    });

    expect(requireAccessedStore().snapshot?.revision).toBe(2);
    expect(requireAccessedStore().snapshot?.transactions.revision).toBe(2);
    expect(screen.getByText("2:2:paused:1:-:stopped")).toBeInTheDocument();
  });

  it("完整配置由 Store 原样提交且不回填字段", async () => {
    accessedStore = null;
    const initialSnapshot = createServiceSnapshot();
    const updateConfiguration = vi.fn(
      async (_update: ConfigurationUpdate) => initialSnapshot,
    );
    const eventClient: EventStreamClient = {
      start(callbacks) {
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    render(
      <ServiceProvider
        controlClient={createControlClient(
          initialSnapshot,
          updateConfiguration,
        )}
        eventClient={eventClient}
      >
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await waitFor(() => {
      expect(accessedStore?.snapshot).not.toBeNull();
      expect(accessedStore?.controlConnection).toBe("connected");
    });
    const {
      authenticationUsernames: _authenticationUsernames,
      ...editableConfiguration
    } = initialSnapshot.configuration;
    const completeUpdate: ConfigurationUpdate = {
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

    await act(async () => {
      await accessedStore?.updateConfiguration(completeUpdate);
    });

    expect(updateConfiguration).toHaveBeenCalledWith(completeUpdate);
  });

  it("清空录制后重新读取完整快照并同步移除事务", async () => {
    accessedStore = null;
    const transaction = createTransactionSummary();
    const baseSnapshot = createServiceSnapshot();
    const initialSnapshot = createServiceSnapshot({
      revision: 1,
      recording: {
        ...baseSnapshot.recording,
        transactionCount: 1,
      },
      transactions: {
        ...baseSnapshot.transactions,
        total: 1,
        items: [transaction],
      },
    });
    const clearedSnapshot = createServiceSnapshot({
      revision: 3,
      recording: {
        ...initialSnapshot.recording,
        transactionCount: 0,
      },
      transactions: {
        ...initialSnapshot.transactions,
        revision: 3,
        collectionToken: "recording-alpha:1",
        total: 0,
        items: [],
      },
    });
    const getSnapshot = vi
      .fn<() => Promise<ServiceSnapshot>>()
      .mockResolvedValueOnce(initialSnapshot)
      .mockResolvedValueOnce(clearedSnapshot);
    const clearRecording = vi.fn(async (): Promise<RecordingResponse> => ({
      serverInstanceId: initialSnapshot.serverInstanceId,
      // clear 响应与随后 GET 的完整快照在没有其他事件时必然共享同一 revision。
      // 该边界曾使失效水位错误拒绝权威快照，导致工作区永久停留在加载状态。
      revision: clearedSnapshot.revision,
      recording: clearedSnapshot.recording,
    }));
    const controlClient: ControlClient = {
      ...createControlClient(initialSnapshot),
      getSnapshot,
      clearRecording,
    };
    const eventClient: EventStreamClient = {
      start(callbacks) {
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };

    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <SnapshotProbe />
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await screen.findByText("2:2:recording:1:-:stopped");
    await waitFor(() =>
      expect(accessedStore?.controlConnection).toBe("connected"),
    );

    await act(async () => {
      await accessedStore?.clearRecording();
    });

    expect(clearRecording).toHaveBeenCalledOnce();
    expect(getSnapshot).toHaveBeenCalledTimes(2);
    expect(requireAccessedStore().snapshot?.recording.recordingSessionId).toBe(
      "recording-alpha",
    );
    expect(requireAccessedStore().snapshot?.transactions.collectionToken).toBe(
      "recording-alpha:1",
    );
    expect(screen.getByText("2:2:recording:0:-:stopped")).toBeInTheDocument();
  });

  it("清空已提交但快照同步失败时不继续展示旧事务", async () => {
    accessedStore = null;
    const transaction = createTransactionSummary();
    const baseSnapshot = createServiceSnapshot();
    const initialSnapshot = createServiceSnapshot({
      recording: {
        ...baseSnapshot.recording,
        transactionCount: 1,
      },
      transactions: {
        ...baseSnapshot.transactions,
        total: 1,
        items: [transaction],
      },
    });
    const synchronizationError = new Error("完整快照同步失败");
    const getSnapshot = vi
      .fn<() => Promise<ServiceSnapshot>>()
      .mockResolvedValueOnce(initialSnapshot)
      .mockRejectedValueOnce(synchronizationError);
    const controlClient: ControlClient = {
      ...createControlClient(initialSnapshot),
      getSnapshot,
      clearRecording: async () => ({
        serverInstanceId: initialSnapshot.serverInstanceId,
        revision: 2,
        recording: {
          ...initialSnapshot.recording,
          transactionCount: 0,
        },
      }),
    };
    const eventClient: EventStreamClient = {
      start(callbacks) {
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <SnapshotProbe />
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await screen.findByText("2:2:recording:1:-:stopped");
    await waitFor(() =>
      expect(accessedStore?.controlConnection).toBe("connected"),
    );

    await act(async () => {
      await requireAccessedStore().clearRecording();
    });

    expect(screen.getByText("等待快照")).toBeInTheDocument();
    expect(requireAccessedStore().snapshot).toBeNull();
    expect(requireAccessedStore().lastError).toBe(synchronizationError.message);
  });

  it("详情和正文读取原样传递取消信号并保留 AbortError", async () => {
    accessedStore = null;
    const snapshot = createServiceSnapshot();
    const abortError = new DOMException("请求已取消", "AbortError");
    const getTransactionDetail = vi.fn(async () => {
      throw abortError;
    });
    const baseClient = createControlClient(snapshot);
    const getRequestBody = vi.fn(baseClient.getRequestBody);
    const getResponseBody = vi.fn(baseClient.getResponseBody);
    const controlClient: ControlClient = {
      ...baseClient,
      getTransactionDetail,
      getRequestBody,
      getResponseBody,
    };
    const eventClient: EventStreamClient = {
      start(callbacks) {
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <StoreAccessProbe />
      </ServiceProvider>,
    );
    await waitFor(() =>
      expect(accessedStore?.controlConnection).toBe("connected"),
    );
    const abortController = new AbortController();

    await expect(
      accessedStore!.getTransactionDetail(
        "transaction-alpha",
        abortController.signal,
      ),
    ).rejects.toBe(abortError);
    await accessedStore!.getTransactionBody(
      "transaction-alpha",
      "request",
      abortController.signal,
    );
    await accessedStore!.getTransactionBody(
      "transaction-alpha",
      "response",
      abortController.signal,
    );

    expect(getTransactionDetail).toHaveBeenCalledWith(
      "transaction-alpha",
      abortController.signal,
    );
    expect(getRequestBody).toHaveBeenCalledWith(
      "transaction-alpha",
      abortController.signal,
    );
    expect(getResponseBody).toHaveBeenCalledWith(
      "transaction-alpha",
      abortController.signal,
    );
  });
});
