import {
  createContext,
  type PropsWithChildren,
  type MutableRefObject,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
} from "react";

import {
  HttpControlClient,
  type ControlClient,
  ControlClientError,
  type ClientCertificateImportSelection,
  type MapLocalImportSelection,
  type MediaPreviewBody,
  type TransactionPageRequest,
} from "../api/controlClient";
import {
  type EventConnectionState,
  type EventStreamClient,
} from "../api/eventClient";
import { deriveServerSentEventsUrl } from "../api/controlEndpoint";
import { ServerSentEventClient } from "../api/serverSentEventClient";
import {
  type EncodedBodyResponse,
  type DecodedProtobufView,
  type DnsSpoofingConfiguration,
  type ProtobufConfiguration,
  type ProtobufConfigurationUpdate,
  type ProtobufDescriptorUpload,
  type BlockCookiesConfiguration,
  type BlockListConfiguration,
  type BreakpointsConfiguration,
  type AutoSaveConfiguration,
  type AuxiliaryListenerPublicState,
  type MirrorConfiguration,
  type PortForwardEntry,
  type PluginConfigurationUpdate,
  type PluginDetails,
  type PluginSnapshot,
  type ReverseProxyEntry,
  serverInstanceIdSchema,
  serviceSnapshotSchema,
  type MessageSide,
  type ConfigurationUpdate,
  type ExportRequest,
  type EventMessage,
  type MapLocalConfiguration,
  type MapLocalImportResult,
  type MapRemoteConfiguration,
  type NoCachingConfiguration,
  type PacketFilterConfiguration,
  type RewriteConfiguration,
  type RecordingRuleConfiguration,
  type ServiceSnapshot,
  type SslConfiguration,
  type ClientCertificateUpdate,
  type SuspendedBreakpoint,
  type ThrottlingConfiguration,
  type TransactionDetail,
  type TransactionPage,
  type ValidateConfiguration,
  type ValidateRequest,
  type ValidationReport,
  type AdvancedRepeatJob,
  type AdvancedRepeatStartRequest,
  type ComposeRequest,
  type ComposeRequestOverrides,
  type ComposeResult,
  type ProcessSelectionSnapshot,
  type ProcessSelectionUpdate,
} from "../api/protocol";
import i18n from "../i18n";
import { mergeServiceEvent } from "./serviceEventReducer";
import { createTransactionDetailRepository } from "./transactionDetailRepository";

type ControlConnectionState = "connecting" | "connected" | "disconnected";

/**
 * 标识当前唯一控制写入的归属区域。
 *
 * 运行上下文：控制面一次只允许一个写入请求，归属字段让工具栏只反馈发起操作的控件，避免无关控件在请求期间一起变灰或改变文案。
 * 失败语义：空值表示没有占用写入槽位；未知归属不会进入类型系统，因此不会意外解除互斥。
 */
type ControlActionScope =
  | "service"
  | "recording"
  | "recordingClear"
  | "configuration"
  | "ssl"
  | "sslRoot"
  | "sslClientCertificate"
  | "tool"
  | "breakpoint"
  | "repeat"
  | "plugin";

interface ServiceStoreState {
  snapshot: ServiceSnapshot | null;
  controlConnection: ControlConnectionState;
  eventConnection: EventConnectionState;
  connectionMessage: string;
  actionPending: boolean;
  activeAction: ControlActionScope | null;
  lastError: string | null;
  suspendedBreakpoints: SuspendedBreakpoint[];
}

interface ServiceStoreValue extends ServiceStoreState {
  refresh(): Promise<void>;
  listTransactions(
    page?: TransactionPageRequest,
    signal?: AbortSignal,
  ): Promise<TransactionPage>;
  toggleService(): Promise<void>;
  toggleRecording(): Promise<void>;
  clearRecording(): Promise<void>;
  getTransactionDetail(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<TransactionDetail>;
  getLiveTransactionDetail(
    transactionId: string,
    revision: string,
    signal?: AbortSignal,
  ): Promise<TransactionDetail>;
  getTransactionBody(
    transactionId: string,
    side: MessageSide,
    signal?: AbortSignal,
  ): Promise<EncodedBodyResponse>;
  getResponseMediaPreview(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<MediaPreviewBody>;
  decodeProtobuf(
    transactionId: string,
    side: MessageSide,
    signal?: AbortSignal,
  ): Promise<DecodedProtobufView>;
  getProtobufConfiguration(
    signal?: AbortSignal,
  ): Promise<ProtobufConfiguration>;
  getValidateConfiguration(
    signal?: AbortSignal,
  ): Promise<ValidateConfiguration>;
  validateResponse(
    transactionId: string,
    request: ValidateRequest,
    signal?: AbortSignal,
  ): Promise<ValidationReport>;
  getValidationReports(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<ValidationReport[]>;
  composeRequest(request: ComposeRequest): Promise<ComposeResult | null>;
  repeatTransaction(
    transactionId: string,
    overrides?: ComposeRequestOverrides,
  ): Promise<ComposeResult | null>;
  startAdvancedRepeat(
    request: AdvancedRepeatStartRequest,
  ): Promise<AdvancedRepeatJob | null>;
  getAdvancedRepeat(jobId: string): Promise<AdvancedRepeatJob>;
  cancelAdvancedRepeat(jobId: string): Promise<AdvancedRepeatJob | null>;
  updateConfiguration(update: ConfigurationUpdate): Promise<void>;
  getProcesses(signal?: AbortSignal): Promise<ProcessSelectionSnapshot>;
  updateProcessSelection(
    update: ProcessSelectionUpdate,
    signal?: AbortSignal,
  ): Promise<ProcessSelectionSnapshot>;
  listPlugins(signal?: AbortSignal): Promise<PluginSnapshot[]>;
  getPluginDetails(
    pluginId: string,
    signal?: AbortSignal,
  ): Promise<PluginDetails>;
  setPluginEnabled(pluginId: string, enabled: boolean): Promise<boolean>;
  updatePluginConfiguration(
    pluginId: string,
    update: PluginConfigurationUpdate,
  ): Promise<PluginDetails | null>;
  reloadPlugin(pluginId: string): Promise<boolean>;
  installPluginPackage(packageFile: File): Promise<boolean>;
  uninstallPlugin(pluginId: string): Promise<boolean>;
  updateSsl(update: SslConfiguration): Promise<boolean>;
  regenerateSslRoot(): Promise<boolean>;
  exportSslRoot(format: "pem" | "cer"): Promise<Blob>;
  importClientCertificate(
    input: ClientCertificateImportSelection,
  ): Promise<boolean>;
  updateClientCertificate(
    id: string,
    update: ClientCertificateUpdate,
  ): Promise<boolean>;
  removeClientCertificate(id: string): Promise<boolean>;
  updateBlockList(update: BlockListConfiguration): Promise<boolean>;
  updateRecordingRules(update: RecordingRuleConfiguration): Promise<boolean>;
  updatePacketFilters(update: PacketFilterConfiguration): Promise<boolean>;
  updateNoCaching(update: NoCachingConfiguration): Promise<boolean>;
  updateBlockCookies(update: BlockCookiesConfiguration): Promise<boolean>;
  updateDnsSpoofing(update: DnsSpoofingConfiguration): Promise<boolean>;
  updateMapLocal(update: MapLocalConfiguration): Promise<boolean>;
  importMapLocalFiles(
    selection: MapLocalImportSelection,
  ): Promise<MapLocalImportResult | null>;
  updateMapRemote(update: MapRemoteConfiguration): Promise<boolean>;
  updateRewrite(update: RewriteConfiguration): Promise<boolean>;
  updateBreakpoints(update: BreakpointsConfiguration): Promise<boolean>;
  updateThrottling(update: ThrottlingConfiguration): Promise<boolean>;
  updateProtobufConfiguration(
    update: ProtobufConfigurationUpdate,
  ): Promise<boolean>;
  uploadProtobufDescriptor(upload: ProtobufDescriptorUpload): Promise<boolean>;
  updateMirror(update: MirrorConfiguration): Promise<boolean>;
  updateAutoSave(update: AutoSaveConfiguration): Promise<boolean>;
  saveAutoSaveNow(): Promise<boolean>;
  updateReverseProxies(update: ReverseProxyEntry[]): Promise<boolean>;
  updatePortForwards(update: PortForwardEntry[]): Promise<boolean>;
  getReverseProxies(): Promise<AuxiliaryListenerPublicState>;
  getPortForwards(): Promise<AuxiliaryListenerPublicState>;
  refreshSuspendedBreakpoints(): Promise<void>;
  continueBreakpoint(
    transactionId: string,
    draft: SuspendedBreakpoint["draft"],
  ): Promise<boolean>;
  abortBreakpoint(transactionId: string): Promise<boolean>;
  exportRecording(request: ExportRequest): Promise<Blob>;
}

interface ServiceProviderProps extends PropsWithChildren {
  controlClient?: ControlClient;
  eventClient?: EventStreamClient;
  broadcastChannelFactory?: (name: string) => BroadcastChannel;
}

type StoreAction =
  | { type: "snapshot"; snapshot: ServiceSnapshot }
  | { type: "invalidate" }
  | {
      type: "connection";
      channel: "control" | "event";
      state: ControlConnectionState | EventConnectionState;
      message: string;
    }
  | { type: "pending"; activeAction: ControlActionScope | null }
  | { type: "error"; message: string | null }
  | { type: "breakpoints"; suspended: SuspendedBreakpoint[] };

const initialState: ServiceStoreState = {
  snapshot: null,
  controlConnection: "connecting",
  eventConnection: "connecting",
  connectionMessage: i18n.t("app.control.connecting"),
  actionPending: false,
  activeAction: null,
  lastError: null,
  suspendedBreakpoints: [],
};

const ServiceStoreContext = createContext<ServiceStoreValue | null>(null);
const broadcastChannelName = "proxy-control-state-v2";

interface StoreRuntime {
  stateRef: MutableRefObject<ServiceStoreState>;
  dispatchAction(action: StoreAction): void;
}

interface StoreRuntimeBinding {
  state: ServiceStoreState;
  runtime: StoreRuntime;
}

interface ResolvedServiceClients {
  controlClient: ControlClient;
  eventClient: EventStreamClient;
}

interface SnapshotCoordinator {
  broadcastRef: MutableRefObject<BroadcastChannel | null>;
  currentServerInstanceId(): string | null;
  acceptSnapshot(
    snapshot: ServiceSnapshot,
    expectedServerInstanceId?: string | null,
  ): void;
  acceptEventSnapshot(snapshot: ServiceSnapshot): void;
  acceptBroadcastSnapshot(snapshot: ServiceSnapshot): void;
  invalidateSnapshot(serverInstanceId: string, revision: number): void;
  acceptBroadcastInvalidation(serverInstanceId: string, revision: number): void;
}

/** 插件控制面操作集合；读取操作不占用全局写入槽位，安装、卸载、启停、重载和配置写入统一串行化。 */
interface PluginOperations {
  listPlugins(signal?: AbortSignal): Promise<PluginSnapshot[]>;
  getPluginDetails(
    pluginId: string,
    signal?: AbortSignal,
  ): Promise<PluginDetails>;
  setPluginEnabled(pluginId: string, enabled: boolean): Promise<boolean>;
  updatePluginConfiguration(
    pluginId: string,
    update: PluginConfigurationUpdate,
  ): Promise<PluginDetails | null>;
  reloadPlugin(pluginId: string): Promise<boolean>;
  installPluginPackage(packageFile: File): Promise<boolean>;
  uninstallPlugin(pluginId: string): Promise<boolean>;
}

interface TransactionReaders {
  listTransactions(
    page?: TransactionPageRequest,
    signal?: AbortSignal,
  ): Promise<TransactionPage>;
  getProcesses(signal?: AbortSignal): Promise<ProcessSelectionSnapshot>;
  updateProcessSelection(
    update: ProcessSelectionUpdate,
    signal?: AbortSignal,
  ): Promise<ProcessSelectionSnapshot>;
  getTransactionDetail(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<TransactionDetail>;
  getLiveTransactionDetail(
    transactionId: string,
    revision: string,
    signal?: AbortSignal,
  ): Promise<TransactionDetail>;
  getTransactionBody(
    transactionId: string,
    side: MessageSide,
    signal?: AbortSignal,
  ): Promise<EncodedBodyResponse>;
  getResponseMediaPreview(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<MediaPreviewBody>;
  decodeProtobuf(
    transactionId: string,
    side: MessageSide,
    signal?: AbortSignal,
  ): Promise<DecodedProtobufView>;
  getProtobufConfiguration(
    signal?: AbortSignal,
  ): Promise<ProtobufConfiguration>;
  getValidateConfiguration(
    signal?: AbortSignal,
  ): Promise<ValidateConfiguration>;
  validateResponse(
    transactionId: string,
    request: ValidateRequest,
    signal?: AbortSignal,
  ): Promise<ValidationReport>;
  getValidationReports(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<ValidationReport[]>;
  getAdvancedRepeat(jobId: string): Promise<AdvancedRepeatJob>;
}

/**
 * 合并状态动作；revision 只在同一后台实例内比较，新实例允许从低 revision 重新计数。
 */
function reduceStore(
  state: ServiceStoreState,
  action: StoreAction,
): ServiceStoreState {
  if (action.type === "snapshot") {
    if (
      state.snapshot !== null &&
      action.snapshot.serverInstanceId === state.snapshot.serverInstanceId &&
      action.snapshot.revision < state.snapshot.revision
    ) {
      return state;
    }
    return {
      ...state,
      snapshot: action.snapshot,
      lastError: null,
      suspendedBreakpoints:
        action.snapshot.tools.suspendedBreakpointCount === 0
          ? []
          : state.suspendedBreakpoints,
    };
  }
  if (action.type === "invalidate") {
    return {
      ...state,
      snapshot: null,
      lastError: null,
    };
  }
  if (action.type === "connection") {
    return {
      ...state,
      [action.channel === "control" ? "controlConnection" : "eventConnection"]:
        action.state,
      connectionMessage: action.message,
    };
  }
  if (action.type === "pending") {
    return {
      ...state,
      actionPending: action.activeAction !== null,
      activeAction: action.activeAction,
    };
  }
  if (action.type === "breakpoints") {
    return { ...state, suspendedBreakpoints: action.suspended };
  }
  return { ...state, lastError: action.message };
}

/**
 * 把未知失败转换为可展示信息；控制客户端错误保留后端精确原因。
 */
function describeError(error: unknown): string {
  if (error instanceof ControlClientError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 同步推进 reducer 与命令侧状态引用；所有写入走同一入口，避免 React 提交前读取旧 actionPending。
 */
function useStoreRuntime(): StoreRuntimeBinding {
  const [state, dispatch] = useReducer(reduceStore, initialState);
  const stateRef = useRef(initialState);
  const dispatchAction = useCallback((action: StoreAction) => {
    stateRef.current = reduceStore(stateRef.current, action);
    dispatch(action);
  }, []);
  const runtime = useMemo(
    () => ({ stateRef, dispatchAction }),
    [dispatchAction],
  );

  return { state, runtime };
}

/**
 * 解析可注入客户端；生产环境只在依赖未注入时由唯一 HTTP 控制地址创建 REST 与事件流实现。
 *
 * 运行上下文：ServiceProvider 首次渲染时执行，SSE 地址必须从 VITE_CONTROL_BASE_URL 派生，不能单独覆盖。
 * 参数：controlClient 和 eventClient 为测试或桌面宿主注入的完整客户端；未传入时使用浏览器默认实现。
 * 失败语义：VITE_CONTROL_BASE_URL 不满足绝对 HTTP(S) 地址约束时端点解析抛出 TypeError，阻止页面连接到错误服务。
 */
function useResolvedServiceClients(
  controlClient?: ControlClient,
  eventClient?: EventStreamClient,
): ResolvedServiceClients {
  const configuredControlBaseUrl =
    import.meta.env.VITE_CONTROL_BASE_URL || undefined;
  const resolvedControlClient = useMemo(
    () => controlClient ?? new HttpControlClient(configuredControlBaseUrl),
    [configuredControlBaseUrl, controlClient],
  );
  const resolvedEventClient = useMemo(
    () =>
      eventClient ??
      new ServerSentEventClient(
        deriveServerSentEventsUrl(configuredControlBaseUrl),
      ),
    [configuredControlBaseUrl, eventClient],
  );

  return useMemo(
    () => ({
      controlClient: resolvedControlClient,
      eventClient: resolvedEventClient,
    }),
    [resolvedControlClient, resolvedEventClient],
  );
}

/**
 * 统一 revision 顺序与跨窗口广播；外部消息只落地不回播，防止 BroadcastChannel 消息环。
 */
function useSnapshotCoordinator(runtime: StoreRuntime): SnapshotCoordinator {
  const broadcastRef = useRef<BroadcastChannel | null>(null);
  const activeServerInstanceIdRef = useRef<string | null>(null);
  const minimumRevisionRef = useRef(-1);

  /**
   * 落地快照投影；HTTP 与 SSE 直连可建立新后台代际，跨窗口消息只能推进已确认的同一实例。
   * `publish` 只用于原子 HTTP 快照，局部 SSE 合并结果不满足完整快照 revision 约束且不得跨窗复制。
   */
  const advanceSnapshot = useCallback(
    (
      snapshot: ServiceSnapshot,
      publish: boolean,
      allowInstanceTransition: boolean,
      expectedServerInstanceId?: string | null,
    ) => {
      const activeServerInstanceId = activeServerInstanceIdRef.current;
      if (
        expectedServerInstanceId !== undefined &&
        activeServerInstanceId !== expectedServerInstanceId &&
        snapshot.serverInstanceId !== activeServerInstanceId
      ) {
        // 请求发出后若已确认另一后台实例，只接受当前实例响应，拒绝旧进程迟到的 HTTP 结果。
        return;
      }
      if (activeServerInstanceId === null) {
        // BroadcastChannel 可能残留旧后台消息；必须先由 HTTP/SSE 直连确认当前实例。
        if (allowInstanceTransition) {
          activeServerInstanceIdRef.current = snapshot.serverInstanceId;
          minimumRevisionRef.current = -1;
        } else {
          return;
        }
      } else if (snapshot.serverInstanceId !== activeServerInstanceId) {
        if (!allowInstanceTransition) {
          return;
        }
        activeServerInstanceIdRef.current = snapshot.serverInstanceId;
        minimumRevisionRef.current = -1;
      }
      const currentSnapshot = runtime.stateRef.current.snapshot;
      const currentRevision =
        currentSnapshot?.serverInstanceId === snapshot.serverInstanceId
          ? currentSnapshot.revision
          : -1;
      if (snapshot.revision < currentRevision) {
        return;
      }
      if (
        snapshot.revision === currentRevision &&
        currentSnapshot !== null &&
        snapshot.transactions.revision <= currentSnapshot.transactions.revision
      ) {
        // 初始 HTTP 快照、SSE 首帧和显式写响应可能携带同一 revision；内容已覆盖时不得重复 dispatch。
        return;
      }
      if (
        currentSnapshot === null &&
        snapshot.revision < minimumRevisionRef.current
      ) {
        // 失效水位表示“低于该 revision 的旧快照不可恢复”，而不是排除水位自身。
        // clear 的写响应和紧随其后的权威 GET 会共享同一 revision；拒绝等值快照会让
        // snapshot 永久停在 null，工作区只能持续显示“正在加载事务详情”。
        return;
      }
      minimumRevisionRef.current = snapshot.revision;
      runtime.dispatchAction({ type: "snapshot", snapshot });
      if (publish) {
        broadcastRef.current?.postMessage({ type: "snapshot", snapshot });
      }
    },
    [runtime.dispatchAction, runtime.stateRef],
  );

  /**
   * 失效同一后台实例的完整快照；其他实例的旧动作或跨窗口消息不得清空当前状态。
   */
  const advanceInvalidation = useCallback(
    (serverInstanceId: string, revision: number, publish: boolean) => {
      if (activeServerInstanceIdRef.current !== serverInstanceId) {
        return;
      }
      const currentSnapshot = runtime.stateRef.current.snapshot;
      const currentRevision =
        currentSnapshot?.serverInstanceId === serverInstanceId
          ? currentSnapshot.revision
          : -1;
      if (revision < currentRevision || revision < minimumRevisionRef.current) {
        return;
      }
      minimumRevisionRef.current = revision;
      runtime.dispatchAction({ type: "invalidate" });
      if (publish) {
        broadcastRef.current?.postMessage({
          type: "invalidate",
          serverInstanceId,
          revision,
        });
      }
    },
    [runtime.dispatchAction, runtime.stateRef],
  );

  const currentServerInstanceId = useCallback(
    () => activeServerInstanceIdRef.current,
    [],
  );
  const acceptSnapshot = useCallback(
    (snapshot: ServiceSnapshot, expectedServerInstanceId?: string | null) =>
      advanceSnapshot(snapshot, true, true, expectedServerInstanceId),
    [advanceSnapshot],
  );
  const acceptEventSnapshot = useCallback(
    (snapshot: ServiceSnapshot) => advanceSnapshot(snapshot, false, true),
    [advanceSnapshot],
  );
  const acceptBroadcastSnapshot = useCallback(
    (snapshot: ServiceSnapshot) => advanceSnapshot(snapshot, false, false),
    [advanceSnapshot],
  );
  const invalidateSnapshot = useCallback(
    (serverInstanceId: string, revision: number) =>
      advanceInvalidation(serverInstanceId, revision, true),
    [advanceInvalidation],
  );
  const acceptBroadcastInvalidation = useCallback(
    (serverInstanceId: string, revision: number) =>
      advanceInvalidation(serverInstanceId, revision, false),
    [advanceInvalidation],
  );

  return useMemo(
    () => ({
      broadcastRef,
      currentServerInstanceId,
      acceptSnapshot,
      acceptEventSnapshot,
      acceptBroadcastSnapshot,
      invalidateSnapshot,
      acceptBroadcastInvalidation,
    }),
    [
      acceptBroadcastInvalidation,
      acceptBroadcastSnapshot,
      acceptEventSnapshot,
      acceptSnapshot,
      currentServerInstanceId,
      invalidateSnapshot,
    ],
  );
}

/**
 * 主动刷新控制快照；失败时保持明确未连接状态，不生成默认运行数据。
 */
function useRefreshAction(
  runtime: StoreRuntime,
  controlClient: ControlClient,
  coordinator: SnapshotCoordinator,
): () => Promise<void> {
  return useCallback(async () => {
    const expectedServerInstanceId = coordinator.currentServerInstanceId();
    runtime.dispatchAction({
      type: "connection",
      channel: "control",
      state: "connecting",
      message: i18n.t("app.control.connecting"),
    });
    try {
      const snapshot = await controlClient.getSnapshot();
      coordinator.acceptSnapshot(snapshot, expectedServerInstanceId);
      runtime.dispatchAction({
        type: "connection",
        channel: "control",
        state: "connected",
        message: i18n.t("app.control.connected"),
      });
      runtime.dispatchAction({ type: "error", message: null });
    } catch (error) {
      const message = describeError(error);
      runtime.dispatchAction({
        type: "connection",
        channel: "control",
        state: "disconnected",
        message,
      });
      runtime.dispatchAction({ type: "error", message });
    }
  }, [controlClient, coordinator, runtime]);
}

/**
 * 判断控制动作是否具备统一前置条件；快照缺失、断连或已有动作时直接拒绝。
 */
function isControlActionReady(state: ServiceStoreState): boolean {
  return (
    state.activeAction === null &&
    state.controlConnection === "connected" &&
    state.snapshot !== null
  );
}

/**
 * 原子占用动作槽位；scope 标识发起控件，guard 与 pending 写入同在一次同步调用内，失败返回 null 且不修改状态。
 */
function beginControlAction(
  runtime: StoreRuntime,
  scope: ControlActionScope,
  guard: (state: ServiceStoreState) => boolean,
): ServiceStoreState | null {
  const currentState = runtime.stateRef.current;
  if (!guard(currentState)) {
    return null;
  }
  runtime.dispatchAction({ type: "pending", activeAction: scope });
  runtime.dispatchAction({ type: "error", message: null });
  return currentState;
}

/**
 * 释放指定归属的动作槽位；仅当前 scope 匹配时清除，防止后续异步完成误释放其他控件的写入。
 */
function finishControlAction(
  runtime: StoreRuntime,
  scope: ControlActionScope,
): void {
  if (runtime.stateRef.current.activeAction === scope) {
    runtime.dispatchAction({ type: "pending", activeAction: null });
  }
}

/**
 * 封装服务、录制与配置写操作；每种动作共享同步互斥槽位，但仍分别处理协议响应。
 */
function useServiceMutations(
  runtime: StoreRuntime,
  controlClient: ControlClient,
  coordinator: SnapshotCoordinator,
) {
  /**
   * 切换服务状态；未知或切换中的状态不产生控制请求。
   */
  const toggleService = useCallback(async () => {
    const currentState = beginControlAction(runtime, "service", (state) => {
      const serviceState = state.snapshot?.serviceState;
      return (
        isControlActionReady(state) &&
        (serviceState === "stopped" ||
          serviceState === "running" ||
          serviceState === "faulted")
      );
    });
    if (currentState === null) {
      return;
    }
    try {
      const snapshot =
        currentState.snapshot?.serviceState === "running"
          ? await controlClient.stopService()
          : await controlClient.startService();
      coordinator.acceptSnapshot(
        snapshot,
        currentState.snapshot?.serverInstanceId,
      );
    } catch (error) {
      runtime.dispatchAction({
        type: "error",
        message: describeError(error),
      });
    } finally {
      finishControlAction(runtime, "service");
    }
  }, [controlClient, coordinator, runtime]);

  /**
   * 在录制与暂停之间切换；旧快照持续承载界面，完整新快照到达后再原子替换。
   * RecordingResponse 不含事务页，若先失效快照会让整个工作区短暂卸载并闪白；
   * 这里不拼接跨 revision 数据，也不清空可见状态，只接受后端返回的完整一致快照。
   */
  const toggleRecording = useCallback(async () => {
    const currentState = beginControlAction(
      runtime,
      "recording",
      isControlActionReady,
    );
    if (currentState === null) {
      return;
    }
    try {
      await controlClient.updateRecording({
        state:
          currentState.snapshot?.recording.state === "recording"
            ? "paused"
            : "recording",
      });
      // 写响应只用于确认动作成功；revision 顺序由完整快照协调器统一裁决。
      // 保留现有快照可以稳定 DOM 和分栏尺寸，避免录制状态切换期间的界面闪动。
      coordinator.acceptSnapshot(
        await controlClient.getSnapshot(),
        currentState.snapshot?.serverInstanceId,
      );
    } catch (error) {
      runtime.dispatchAction({
        type: "error",
        message: describeError(error),
      });
    } finally {
      finishControlAction(runtime, "recording");
    }
  }, [controlClient, coordinator, runtime]);

  /**
   * 清空录制后先使旧快照失效再重读；等于清空水位的完整快照必须恢复工作区。
   * 运行上下文：写响应只携带录制状态，随后 GET 才包含空事务页及新的 collectionToken。
   * 失败语义：同步失败时保持快照失效并显示控制错误，不回放已删除事务。
   */
  const clearRecording = useCallback(async () => {
    const currentState = beginControlAction(
      runtime,
      "recordingClear",
      isControlActionReady,
    );
    if (currentState === null) {
      return;
    }
    try {
      const response = await controlClient.clearRecording();
      coordinator.invalidateSnapshot(
        response.serverInstanceId,
        response.revision,
      );
      coordinator.acceptSnapshot(
        await controlClient.getSnapshot(),
        currentState.snapshot?.serverInstanceId,
      );
    } catch (error) {
      runtime.dispatchAction({
        type: "error",
        message: describeError(error),
      });
    } finally {
      finishControlAction(runtime, "recordingClear");
    }
  }, [controlClient, coordinator, runtime]);

  /**
   * 原样提交完整配置；协议验证失败由控制客户端抛出并写入可见错误状态。
   */
  const updateConfiguration = useCallback(
    async (update: ConfigurationUpdate) => {
      const currentState = beginControlAction(
        runtime,
        "configuration",
        isControlActionReady,
      );
      if (currentState === null) {
        return;
      }
      try {
        coordinator.acceptSnapshot(
          await controlClient.updateConfiguration(update),
          currentState.snapshot?.serverInstanceId,
        );
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
      } finally {
        finishControlAction(runtime, "configuration");
      }
    },
    [controlClient, coordinator, runtime],
  );

  /**
   * 实时提交 SSL 主机规则并重读完整快照；布尔结果让对话框只在成功后关闭。
   */
  const updateSsl = useCallback(
    async (update: SslConfiguration): Promise<boolean> => {
      const currentState = beginControlAction(
        runtime,
        "ssl",
        isControlActionReady,
      );
      if (currentState === null) {
        return false;
      }
      try {
        await controlClient.updateSsl(update);
        coordinator.acceptSnapshot(
          await controlClient.getSnapshot(),
          currentState.snapshot?.serverInstanceId,
        );
        return true;
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        return false;
      } finally {
        finishControlAction(runtime, "ssl");
      }
    },
    [controlClient, coordinator, runtime],
  );

  /**
   * 更换根 CA 并立即重读快照；确认步骤由调用对话框承担，失败保留现有证书视图。
   */
  const regenerateSslRoot = useCallback(async (): Promise<boolean> => {
    const currentState = beginControlAction(
      runtime,
      "sslRoot",
      isControlActionReady,
    );
    if (currentState === null) {
      return false;
    }
    try {
      await controlClient.regenerateSslRoot();
      coordinator.acceptSnapshot(
        await controlClient.getSnapshot(),
        currentState.snapshot?.serverInstanceId,
      );
      return true;
    } catch (error) {
      runtime.dispatchAction({
        type: "error",
        message: describeError(error),
      });
      return false;
    } finally {
      finishControlAction(runtime, "sslRoot");
    }
  }, [controlClient, coordinator, runtime]);

  /**
   * 下载公开根证书；二进制结果直接交给对话框创建临时下载 URL，失败继续进入全局错误区。
   */
  const exportSslRoot = useCallback(
    async (format: "pem" | "cer"): Promise<Blob> => {
      try {
        return await controlClient.exportSslRoot(format);
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        throw error;
      }
    },
    [controlClient, runtime],
  );

  /** 串行执行客户端证书写操作并重读权威快照，防止规则列表与连接池状态跨 revision。 */
  const mutateClientCertificate = useCallback(
    async (request: () => Promise<unknown>): Promise<boolean> => {
      const currentState = beginControlAction(
        runtime,
        "sslClientCertificate",
        isControlActionReady,
      );
      if (currentState === null) {
        return false;
      }
      try {
        await request();
        coordinator.acceptSnapshot(
          await controlClient.getSnapshot(),
          currentState.snapshot?.serverInstanceId,
        );
        return true;
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        return false;
      } finally {
        finishControlAction(runtime, "sslClientCertificate");
      }
    },
    [controlClient, coordinator, runtime],
  );

  /** 导入客户端证书文件；File 与口令只存在于当前浏览器请求中。 */
  const importClientCertificate = useCallback(
    (input: ClientCertificateImportSelection) =>
      mutateClientCertificate(() =>
        controlClient.importClientCertificate(input),
      ),
    [controlClient, mutateClientCertificate],
  );

  /** 更新已导入身份的名称、启用状态和主机匹配规则。 */
  const updateClientCertificate = useCallback(
    (id: string, update: ClientCertificateUpdate) =>
      mutateClientCertificate(() =>
        controlClient.updateClientCertificate(id, update),
      ),
    [controlClient, mutateClientCertificate],
  );

  /** 删除指定客户端身份；成功后完整快照立即移除该项。 */
  const removeClientCertificate = useCallback(
    (id: string) =>
      mutateClientCertificate(() => controlClient.removeClientCertificate(id)),
    [controlClient, mutateClientCertificate],
  );

  /**
   * 统一提交任一工具配置并重读完整快照；工具资源仅返回局部状态，不能与旧事务页自行拼接。
   */
  const updateTool = useCallback(
    async (request: () => Promise<unknown>): Promise<boolean> => {
      const currentState = beginControlAction(
        runtime,
        "tool",
        isControlActionReady,
      );
      if (currentState === null) {
        return false;
      }
      try {
        await request();
        coordinator.acceptSnapshot(
          await controlClient.getSnapshot(),
          currentState.snapshot?.serverInstanceId,
        );
        return true;
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        return false;
      } finally {
        finishControlAction(runtime, "tool");
      }
    },
    [controlClient, coordinator, runtime],
  );

  /** 提交屏蔽列表配置并在服务端验证 Location 与状态码边界。 */
  const updateBlockList = useCallback(
    (update: BlockListConfiguration) =>
      updateTool(() => controlClient.updateBlockList(update)),
    [controlClient, updateTool],
  );

  /** 提交无缓存规则；请求和响应头方向由后端保持固定流水线顺序。 */
  const updateNoCaching = useCallback(
    (update: NoCachingConfiguration) =>
      updateTool(() => controlClient.updateNoCaching(update)),
    [controlClient, updateTool],
  );

  /** 提交录制规则集；后端原子替换共享匹配器，现有监听器和进程捕获无需重启。 */
  const updateRecordingRules = useCallback(
    (update: RecordingRuleConfiguration) =>
      updateTool(() => controlClient.updateRecordingRules(update)),
    [controlClient, updateTool],
  );

  /** 提交封包滤镜；运行中 TCP/UDP 连接从下一块线上字节开始使用新快照。 */
  const updatePacketFilters = useCallback(
    (update: PacketFilterConfiguration) =>
      updateTool(() => controlClient.updatePacketFilters(update)),
    [controlClient, updateTool],
  );

  /** 提交 Cookie 剥离规则；仅更新工具状态，不改变浏览器的本地 Cookie 存储。 */
  const updateBlockCookies = useCallback(
    (update: BlockCookiesConfiguration) =>
      updateTool(() => controlClient.updateBlockCookies(update)),
    [controlClient, updateTool],
  );

  /** 提交 DNS 映射规则；控制面热更新共享解析器，不重启监听器或中断现有连接。 */
  const updateDnsSpoofing = useCallback(
    (update: DnsSpoofingConfiguration) =>
      updateTool(() => controlClient.updateDnsSpoofing(update)),
    [controlClient, updateTool],
  );

  /** 提交本地映射规则，响应短路和录制终态均由后端流水线负责。 */
  const updateMapLocal = useCallback(
    (update: MapLocalConfiguration) =>
      updateTool(() => controlClient.updateMapLocal(update)),
    [controlClient, updateTool],
  );

  /** 导入浏览器选择的本地文件或目录；操作期间复用工具写入槽，失败返回 null 并保留当前规则草稿。 */
  const importMapLocalFiles = useCallback(
    async (
      selection: MapLocalImportSelection,
    ): Promise<MapLocalImportResult | null> => {
      const currentState = beginControlAction(
        runtime,
        "tool",
        isControlActionReady,
      );
      if (currentState === null) {
        return null;
      }
      try {
        return await controlClient.importMapLocalFiles(selection);
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        return null;
      } finally {
        finishControlAction(runtime, "tool");
      }
    },
    [controlClient, runtime],
  );

  /** 提交远程映射规则，原始 URL 的可视痕迹仍以事务记录为准。 */
  const updateMapRemote = useCallback(
    (update: MapRemoteConfiguration) =>
      updateTool(() => controlClient.updateMapRemote(update)),
    [controlClient, updateTool],
  );

  /** 提交 Rewrite 集合；正则编译失败由控制 API 回传稳定错误。 */
  const updateRewrite = useCallback(
    (update: RewriteConfiguration) =>
      updateTool(() => controlClient.updateRewrite(update)),
    [controlClient, updateTool],
  );

  /** 提交断点配置；已挂起项目保持自身草稿和超时生命周期。 */
  const updateBreakpoints = useCallback(
    (update: BreakpointsConfiguration) =>
      updateTool(() => controlClient.updateBreakpoints(update)),
    [controlClient, updateTool],
  );

  /** 提交带宽限制配置；控制面请求不经过数据面节流。 */
  const updateThrottling = useCallback(
    (update: ThrottlingConfiguration) =>
      updateTool(() => controlClient.updateThrottling(update)),
    [controlClient, updateTool],
  );

  /** 提交 Protobuf 解码开关与路由；描述符清单由上传操作单独维护，避免更新路由时丢失已登记文件。 */
  const updateProtobufConfiguration = useCallback(
    (update: ProtobufConfigurationUpdate) =>
      updateTool(() => controlClient.updateProtobufConfiguration(update)),
    [controlClient, updateTool],
  );

  /** 上传描述符后刷新快照协调器；上传内容只在 ControlClient 请求期间保留，不写入浏览器持久化状态。 */
  const uploadProtobufDescriptor = useCallback(
    (upload: ProtobufDescriptorUpload) =>
      updateTool(() => controlClient.uploadProtobufDescriptor(upload)),
    [controlClient, updateTool],
  );

  /** 提交镜像目录和写入策略；后端写入器读取新快照，不中断已在转发的请求。 */
  const updateMirror = useCallback(
    (update: MirrorConfiguration) =>
      updateTool(() => controlClient.updateMirror(update)),
    [controlClient, updateTool],
  );

  /** 提交自动保存触发器和归档选项；更新后立即重读快照以显示最近状态。 */
  const updateAutoSave = useCallback(
    (update: AutoSaveConfiguration) =>
      updateTool(() => controlClient.updateAutoSave(update)),
    [controlClient, updateTool],
  );

  /** 手动保存当前录制会话；成功后刷新完整快照而不是在前端拼接局部状态。 */
  const saveAutoSaveNow = useCallback(
    () => updateTool(() => controlClient.saveAutoSaveNow()),
    [controlClient, updateTool],
  );

  /** 提交反向代理监听规则；服务端负责断开旧连接、重绑端口并返回权威快照。 */
  const updateReverseProxies = useCallback(
    (update: ReverseProxyEntry[]) =>
      updateTool(() => controlClient.updateReverseProxies(update)),
    [controlClient, updateTool],
  );

  /** 提交 TCP 端口转发规则；规则更新沿用同一服务生命周期和错误呈现路径。 */
  const updatePortForwards = useCallback(
    (update: PortForwardEntry[]) =>
      updateTool(() => controlClient.updatePortForwards(update)),
    [controlClient, updateTool],
  );

  /** 读取反向代理规则和实际端口绑定；失败进入共享错误区并交由对话框保留当前草稿。 */
  const getReverseProxies = useCallback(async () => {
    try {
      return await controlClient.getReverseProxies();
    } catch (error) {
      runtime.dispatchAction({ type: "error", message: describeError(error) });
      throw error;
    }
  }, [controlClient, runtime]);

  /** 读取 TCP 转发规则和实际端口绑定；本函数不占用写入槽位，允许对话框打开时刷新状态。 */
  const getPortForwards = useCallback(async () => {
    try {
      return await controlClient.getPortForwards();
    } catch (error) {
      runtime.dispatchAction({ type: "error", message: describeError(error) });
      throw error;
    }
  }, [controlClient, runtime]);

  /** 读取当前挂起队列；页面初次连接和事件重连后都可用此操作恢复编辑器状态。 */
  const refreshSuspendedBreakpoints = useCallback(async () => {
    try {
      runtime.dispatchAction({
        type: "breakpoints",
        suspended: await controlClient.listSuspendedBreakpoints(),
      });
    } catch (error) {
      runtime.dispatchAction({ type: "error", message: describeError(error) });
    }
  }, [controlClient, runtime]);

  /** 回写草稿并继续流水线；成功后同时刷新队列和事务快照，避免编辑器残留旧项。 */
  const continueBreakpoint = useCallback(
    async (
      transactionId: string,
      draft: SuspendedBreakpoint["draft"],
    ): Promise<boolean> => {
      const currentState = beginControlAction(
        runtime,
        "breakpoint",
        isControlActionReady,
      );
      if (currentState === null) {
        return false;
      }
      try {
        await controlClient.continueBreakpoint(transactionId, draft);
        runtime.dispatchAction({
          type: "breakpoints",
          suspended: await controlClient.listSuspendedBreakpoints(),
        });
        coordinator.acceptSnapshot(
          await controlClient.getSnapshot(),
          currentState.snapshot?.serverInstanceId,
        );
        return true;
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        return false;
      } finally {
        finishControlAction(runtime, "breakpoint");
      }
    },
    [controlClient, coordinator, runtime],
  );

  /** 中止挂起事务并释放槽位；刷新顺序与继续操作一致，防止队列计数短暂漂移。 */
  const abortBreakpoint = useCallback(
    async (transactionId: string): Promise<boolean> => {
      const currentState = beginControlAction(
        runtime,
        "breakpoint",
        isControlActionReady,
      );
      if (currentState === null) {
        return false;
      }
      try {
        await controlClient.abortBreakpoint(transactionId);
        runtime.dispatchAction({
          type: "breakpoints",
          suspended: await controlClient.listSuspendedBreakpoints(),
        });
        coordinator.acceptSnapshot(
          await controlClient.getSnapshot(),
          currentState.snapshot?.serverInstanceId,
        );
        return true;
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        return false;
      } finally {
        finishControlAction(runtime, "breakpoint");
      }
    },
    [controlClient, coordinator, runtime],
  );

  /** 导出 HAR 文件；导出不改写服务状态，失败仅写入共享错误区。 */
  const exportRecording = useCallback(
    async (request: ExportRequest): Promise<Blob> => {
      try {
        return await controlClient.exportRecording(request);
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        throw error;
      }
    },
    [controlClient, runtime],
  );

  /** 执行需要写入事务或作业状态的重复操作；同一控制槽确保编辑提交与服务配置不会并发覆盖界面状态。 */
  const executeRepeatAction = useCallback(
    async <Result,>(
      operation: () => Promise<Result>,
    ): Promise<Result | null> => {
      const currentState = beginControlAction(
        runtime,
        "repeat",
        isControlActionReady,
      );
      if (currentState === null) {
        return null;
      }
      try {
        const result = await operation();
        coordinator.acceptSnapshot(
          await controlClient.getSnapshot(),
          currentState.snapshot?.serverInstanceId,
        );
        return result;
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        return null;
      } finally {
        finishControlAction(runtime, "repeat");
      }
    },
    [controlClient, coordinator, runtime],
  );

  /** 提交新的编辑请求；成功后快照立即刷新，以便连接工作区定位后台创建的 pending 事务。 */
  const composeRequest = useCallback(
    (request: ComposeRequest) =>
      executeRepeatAction(() => controlClient.composeRequest(request)),
    [controlClient, executeRepeatAction],
  );

  /** 从原始事务派生重复请求；空覆盖保持原始请求完全只读。 */
  const repeatTransaction = useCallback(
    (transactionId: string, overrides?: ComposeRequestOverrides) =>
      executeRepeatAction(() =>
        controlClient.repeatTransaction(transactionId, overrides),
      ),
    [controlClient, executeRepeatAction],
  );

  /** 创建已确认的高级重复作业；作业运行期不占用控制槽，便于用户继续浏览事务。 */
  const startAdvancedRepeat = useCallback(
    (request: AdvancedRepeatStartRequest) =>
      executeRepeatAction(() => controlClient.startAdvancedRepeat(request)),
    [controlClient, executeRepeatAction],
  );

  /** 发送协作式取消并刷新事务快照；已运行迭代由后端自行收敛。 */
  const cancelAdvancedRepeat = useCallback(
    (jobId: string) =>
      executeRepeatAction(() => controlClient.cancelAdvancedRepeat(jobId)),
    [controlClient, executeRepeatAction],
  );

  return useMemo(
    () => ({
      toggleService,
      toggleRecording,
      clearRecording,
      updateConfiguration,
      updateSsl,
      regenerateSslRoot,
      exportSslRoot,
      importClientCertificate,
      updateClientCertificate,
      removeClientCertificate,
      updateBlockList,
      updateRecordingRules,
      updatePacketFilters,
      updateNoCaching,
      updateBlockCookies,
      updateDnsSpoofing,
      updateMapLocal,
      importMapLocalFiles,
      updateMapRemote,
      updateRewrite,
      updateBreakpoints,
      updateThrottling,
      updateProtobufConfiguration,
      uploadProtobufDescriptor,
      updateMirror,
      updateAutoSave,
      saveAutoSaveNow,
      updateReverseProxies,
      updatePortForwards,
      getReverseProxies,
      getPortForwards,
      refreshSuspendedBreakpoints,
      continueBreakpoint,
      abortBreakpoint,
      exportRecording,
      composeRequest,
      repeatTransaction,
      startAdvancedRepeat,
      cancelAdvancedRepeat,
    }),
    [
      clearRecording,
      toggleRecording,
      toggleService,
      updateConfiguration,
      updateSsl,
      regenerateSslRoot,
      exportSslRoot,
      importClientCertificate,
      updateClientCertificate,
      removeClientCertificate,
      updateBlockList,
      updateRecordingRules,
      updatePacketFilters,
      updateNoCaching,
      updateBlockCookies,
      updateDnsSpoofing,
      updateMapLocal,
      importMapLocalFiles,
      updateMapRemote,
      updateRewrite,
      updateBreakpoints,
      updateThrottling,
      updateProtobufConfiguration,
      uploadProtobufDescriptor,
      updateMirror,
      updateAutoSave,
      saveAutoSaveNow,
      updateReverseProxies,
      updatePortForwards,
      getReverseProxies,
      getPortForwards,
      refreshSuspendedBreakpoints,
      continueBreakpoint,
      abortBreakpoint,
      exportRecording,
      composeRequest,
      repeatTransaction,
      startAdvancedRepeat,
      cancelAdvancedRepeat,
    ],
  );
}

/** 从唯一实现推导服务写操作契约，避免维护与返回对象重复的手写方法清单。 */
type ServiceMutations = ReturnType<typeof useServiceMutations>;

/**
 * 提供插件管理读写动作；读取保持按需请求，写入复用全局动作槽位，防止安装、启停和服务配置重启同时改变底层生命周期。
 */
function usePluginOperations(
  runtime: StoreRuntime,
  controlClient: ControlClient,
): PluginOperations {
  /** 执行一个会改变插件生命周期或配置的控制请求，并将后端错误写入全局可见状态。 */
  const executePluginAction = useCallback(
    async <Result,>(
      operation: () => Promise<Result>,
    ): Promise<Result | null> => {
      const currentState = beginControlAction(
        runtime,
        "plugin",
        (state) =>
          state.activeAction === null &&
          state.controlConnection === "connected",
      );
      if (currentState === null) {
        return null;
      }
      try {
        return await operation();
      } catch (error) {
        runtime.dispatchAction({
          type: "error",
          message: describeError(error),
        });
        return null;
      } finally {
        finishControlAction(runtime, "plugin");
      }
    },
    [runtime],
  );

  /** 读取插件列表；管理对话框负责取消过期读取和显示局部加载状态。 */
  const listPlugins = useCallback(
    (signal?: AbortSignal) => controlClient.listPlugins(signal),
    [controlClient],
  );

  /** 读取单插件设置详情；秘密值已在后端脱敏，前端只接收可渲染的字段描述。 */
  const getPluginDetails = useCallback(
    (pluginId: string, signal?: AbortSignal) =>
      controlClient.getPluginDetails(pluginId, signal),
    [controlClient],
  );

  /** 切换插件运行意图，成功后由调用方重读列表以避免依赖局部猜测。 */
  const setPluginEnabled = useCallback(
    async (pluginId: string, enabled: boolean): Promise<boolean> =>
      (await executePluginAction(() =>
        controlClient.setPluginEnabled(pluginId, enabled),
      )) !== null,
    [controlClient, executePluginAction],
  );

  /** 保存设置表单并返回宿主脱敏后的权威详情，供对话框刷新秘密字段状态。 */
  const updatePluginConfiguration = useCallback(
    (pluginId: string, update: PluginConfigurationUpdate) =>
      executePluginAction(() =>
        controlClient.updatePluginConfiguration(pluginId, update),
      ),
    [controlClient, executePluginAction],
  );

  /** 请求重载已安装插件；失败由统一错误状态呈现，不清空管理对话框中的当前列表。 */
  const reloadPlugin = useCallback(
    async (pluginId: string): Promise<boolean> =>
      (await executePluginAction(() =>
        controlClient.reloadPlugin(pluginId),
      )) !== null,
    [controlClient, executePluginAction],
  );

  /** 上传本地插件包；文件只作为本次二进制请求体传递，不写入浏览器持久化状态。 */
  const installPluginPackage = useCallback(
    async (packageFile: File): Promise<boolean> =>
      (await executePluginAction(() =>
        controlClient.installPluginPackage(packageFile),
      )) !== null,
    [controlClient, executePluginAction],
  );

  /** 删除已停止处理连接的插件包；活动连接冲突由后端保持为可见错误而不是强行终止 UI。 */
  const uninstallPlugin = useCallback(
    async (pluginId: string): Promise<boolean> =>
      (await executePluginAction(() =>
        controlClient.uninstallPlugin(pluginId),
      )) !== null,
    [controlClient, executePluginAction],
  );

  return useMemo(
    () => ({
      listPlugins,
      getPluginDetails,
      setPluginEnabled,
      updatePluginConfiguration,
      reloadPlugin,
      installPluginPackage,
      uninstallPlugin,
    }),
    [
      getPluginDetails,
      installPluginPackage,
      listPlugins,
      reloadPlugin,
      setPluginEnabled,
      uninstallPlugin,
      updatePluginConfiguration,
    ],
  );
}

function useTransactionReaders(
  controlClient: ControlClient,
): TransactionReaders {
  const listTransactions = useCallback(
    (page?: TransactionPageRequest, signal?: AbortSignal) =>
      controlClient.listTransactions(page, signal),
    [controlClient],
  );
  const getTransactionDetail = useCallback(
    (transactionId: string, signal?: AbortSignal) =>
      controlClient.getTransactionDetail(transactionId, signal),
    [controlClient],
  );
  const transactionDetailRepository = useMemo(
    () =>
      createTransactionDetailRepository((transactionId) =>
        controlClient.getTransactionDetail(transactionId),
      ),
    [controlClient],
  );
  /** 通过 Provider 级仓库读取事件版本，供导航树与检查器共享同一在途请求和结果。 */
  const getLiveTransactionDetail = useCallback(
    (transactionId: string, revision: string, signal?: AbortSignal) =>
      transactionDetailRepository.read(transactionId, revision, signal),
    [transactionDetailRepository],
  );
  const getTransactionBody = useCallback(
    (transactionId: string, side: MessageSide, signal?: AbortSignal) =>
      side === "request"
        ? controlClient.getRequestBody(transactionId, signal)
        : controlClient.getResponseBody(transactionId, signal),
    [controlClient],
  );
  const getResponseMediaPreview = useCallback(
    (transactionId: string, signal?: AbortSignal) =>
      controlClient.getResponseMediaPreview(transactionId, signal),
    [controlClient],
  );
  const getAdvancedRepeat = useCallback(
    (jobId: string) => controlClient.getAdvancedRepeat(jobId),
    [controlClient],
  );
  const decodeProtobuf = useCallback(
    (transactionId: string, side: MessageSide, signal?: AbortSignal) =>
      controlClient.decodeProtobuf(transactionId, side, signal),
    [controlClient],
  );
  const getProtobufConfiguration = useCallback(
    (signal?: AbortSignal) => controlClient.getProtobufConfiguration(signal),
    [controlClient],
  );
  const getValidateConfiguration = useCallback(
    (signal?: AbortSignal) => controlClient.getValidateConfiguration(signal),
    [controlClient],
  );
  const validateResponse = useCallback(
    (transactionId: string, request: ValidateRequest, signal?: AbortSignal) =>
      controlClient.validateResponse(transactionId, request, signal),
    [controlClient],
  );
  const getValidationReports = useCallback(
    (transactionId: string, signal?: AbortSignal) =>
      controlClient.getValidationReports(transactionId, signal),
    [controlClient],
  );
  const getProcesses = useCallback(
    (signal?: AbortSignal) => controlClient.getProcesses(signal),
    [controlClient],
  );
  const updateProcessSelection = useCallback(
    (update: ProcessSelectionUpdate, signal?: AbortSignal) =>
      controlClient.updateProcessSelection(update, signal),
    [controlClient],
  );

  return useMemo(
    () => ({
      listTransactions,
      getTransactionDetail,
      getLiveTransactionDetail,
      getTransactionBody,
      getResponseMediaPreview,
      getAdvancedRepeat,
      decodeProtobuf,
      getProtobufConfiguration,
      getValidateConfiguration,
      validateResponse,
      getValidationReports,
      getProcesses,
      updateProcessSelection,
    }),
    [
      listTransactions,
      decodeProtobuf,
      getAdvancedRepeat,
      getTransactionBody,
      getResponseMediaPreview,
      getTransactionDetail,
      getLiveTransactionDetail,
      getProtobufConfiguration,
      getValidateConfiguration,
      getValidationReports,
      getProcesses,
      updateProcessSelection,
      validateResponse,
    ],
  );
}

/**
 * 校验并应用跨窗口消息；非法消息被丢弃，合法消息不再次广播。
 */
function acceptBroadcastMessage(
  messageData: unknown,
  coordinator: SnapshotCoordinator,
): void {
  const message = messageData as {
    type?: unknown;
    serverInstanceId?: unknown;
    revision?: unknown;
    snapshot?: unknown;
  };
  const parsedServerInstanceId = serverInstanceIdSchema.safeParse(
    message.serverInstanceId,
  );
  if (
    message.type === "invalidate" &&
    parsedServerInstanceId.success &&
    typeof message.revision === "number" &&
    Number.isSafeInteger(message.revision) &&
    message.revision >= 0
  ) {
    coordinator.acceptBroadcastInvalidation(
      parsedServerInstanceId.data,
      message.revision,
    );
    return;
  }
  if (message.type !== "snapshot") {
    return;
  }
  const parsedSnapshot = serviceSnapshotSchema.safeParse(message.snapshot);
  if (parsedSnapshot.success) {
    coordinator.acceptBroadcastSnapshot(parsedSnapshot.data);
  }
}

interface ServiceTransportOptions {
  runtime: StoreRuntime;
  eventClient: EventStreamClient;
  coordinator: SnapshotCoordinator;
  refresh(): Promise<void>;
  refreshSuspendedBreakpoints(): Promise<void>;
  broadcastChannelFactory?: (name: string) => BroadcastChannel;
}

/**
 * 管理刷新、事件流和 BroadcastChannel 生命周期；卸载时关闭全部长生命周期资源。
 */
function useServiceTransport(options: ServiceTransportOptions): void {
  const {
    runtime,
    eventClient,
    coordinator,
    refresh,
    refreshSuspendedBreakpoints,
    broadcastChannelFactory,
  } = options;
  const eventServerInstanceIdRef = useRef<string | null>(null);

  useEffect(() => {
    let channel: BroadcastChannel | null = null;
    let eventConnectionGeneration = 0;
    let pendingEventSnapshot: EventMessage | null = null;
    let arbitrationPromise: Promise<void> | null = null;
    let transportClosed = false;
    const receiveBroadcastMessage = (event: MessageEvent) => {
      acceptBroadcastMessage(event.data, coordinator);
    };

    /**
     * 合并已经通过实例仲裁的事件；局部事件缺少同实例基线时保持现有快照不变。
     */
    const acceptEventMessage = (message: EventMessage) => {
      const mergedSnapshot = mergeServiceEvent(
        runtime.stateRef.current.snapshot,
        message,
      );
      if (mergedSnapshot !== null) {
        // 每个窗口已拥有唯一 SSE，局部事件无需再广播完整快照；否则多窗口会形成 N² 复制与重复渲染。
        coordinator.acceptEventSnapshot(mergedSnapshot);
      }
      if (message.type === "breakpoints") {
        runtime.dispatchAction({
          type: "breakpoints",
          suspended: message.suspended,
        });
      }
    };

    /**
     * 用控制面快照仲裁事件流首帧的实例冲突；只有 HTTP 也确认候选实例后才切换代际。
     */
    const arbitratePendingEventSnapshot = () => {
      if (arbitrationPromise !== null || pendingEventSnapshot === null) {
        return;
      }
      const arbitrationGeneration = eventConnectionGeneration;
      arbitrationPromise = refresh()
        .then(() => {
          if (
            transportClosed ||
            arbitrationGeneration !== eventConnectionGeneration ||
            eventServerInstanceIdRef.current !== null
          ) {
            return;
          }
          const candidate = pendingEventSnapshot;
          pendingEventSnapshot = null;
          if (
            candidate === null ||
            coordinator.currentServerInstanceId() !== candidate.serverInstanceId
          ) {
            return;
          }
          eventServerInstanceIdRef.current = candidate.serverInstanceId;
          acceptEventMessage(candidate);
        })
        .finally(() => {
          if (arbitrationGeneration === eventConnectionGeneration) {
            arbitrationPromise = null;
          }
        });
    };

    if (typeof BroadcastChannel !== "undefined" || broadcastChannelFactory) {
      const factory =
        broadcastChannelFactory ??
        ((name: string) => new BroadcastChannel(name));
      channel = factory(broadcastChannelName);
      coordinator.broadcastRef.current = channel;
      channel.addEventListener("message", receiveBroadcastMessage);
    }

    void refresh().then(() => refreshSuspendedBreakpoints());
    eventClient.start({
      onConnectionState(connectionState, message) {
        if (connectionState === "connecting") {
          // 每次事件流重连都必须重新绑定首个实例标识，旧连接的迟到帧不能沿用新连接身份。
          eventConnectionGeneration += 1;
          eventServerInstanceIdRef.current = null;
          pendingEventSnapshot = null;
          arbitrationPromise = null;
        }
        runtime.dispatchAction({
          type: "connection",
          channel: "event",
          state: connectionState,
          message,
        });
      },
      onMessage(message) {
        const activeServerInstanceId = coordinator.currentServerInstanceId();
        if (eventServerInstanceIdRef.current === null) {
          if (activeServerInstanceId === null) {
            if (message.type !== "snapshot") {
              return;
            }
            eventServerInstanceIdRef.current = message.serverInstanceId;
          } else if (message.serverInstanceId === activeServerInstanceId) {
            eventServerInstanceIdRef.current = message.serverInstanceId;
          } else {
            // 实例冲突既可能来自旧连接迟到帧，也可能来自后台真实重启；
            // 交给 HTTP 控制面仲裁，避免误回退或永久拒绝新实例。
            if (message.type !== "snapshot") {
              return;
            }
            if (
              pendingEventSnapshot === null ||
              pendingEventSnapshot.serverInstanceId === message.serverInstanceId
            ) {
              pendingEventSnapshot = message;
            }
            arbitratePendingEventSnapshot();
            return;
          }
        } else if (
          message.serverInstanceId !== eventServerInstanceIdRef.current ||
          (activeServerInstanceId !== null &&
            message.serverInstanceId !== activeServerInstanceId)
        ) {
          return;
        }
        acceptEventMessage(message);
      },
    });

    return () => {
      transportClosed = true;
      eventConnectionGeneration += 1;
      eventClient.stop();
      channel?.removeEventListener("message", receiveBroadcastMessage);
      channel?.close();
      if (coordinator.broadcastRef.current === channel) {
        coordinator.broadcastRef.current = null;
      }
    };
  }, [
    broadcastChannelFactory,
    coordinator,
    eventClient,
    refresh,
    refreshSuspendedBreakpoints,
    runtime,
  ]);
}

/**
 * 组合稳定 Context 值；状态变化只替换值对象，不重建动作与读取函数。
 */
function useServiceStoreValue(
  state: ServiceStoreState,
  refresh: () => Promise<void>,
  mutations: ServiceMutations,
  pluginOperations: PluginOperations,
  readers: TransactionReaders,
): ServiceStoreValue {
  return useMemo(
    () => ({
      ...state,
      refresh,
      ...mutations,
      ...pluginOperations,
      ...readers,
    }),
    [mutations, pluginOperations, readers, refresh, state],
  );
}

/**
 * 提供主窗口与悬浮窗共享状态；生命周期、快照协调和控制动作由独立 hook 管理。
 */
export function ServiceProvider({
  children,
  controlClient,
  eventClient,
  broadcastChannelFactory,
}: ServiceProviderProps) {
  const { state, runtime } = useStoreRuntime();
  const clients = useResolvedServiceClients(controlClient, eventClient);
  const coordinator = useSnapshotCoordinator(runtime);
  const refresh = useRefreshAction(runtime, clients.controlClient, coordinator);
  const mutations = useServiceMutations(
    runtime,
    clients.controlClient,
    coordinator,
  );
  const pluginOperations = usePluginOperations(runtime, clients.controlClient);
  const readers = useTransactionReaders(clients.controlClient);
  useServiceTransport({
    runtime,
    eventClient: clients.eventClient,
    coordinator,
    refresh,
    refreshSuspendedBreakpoints: mutations.refreshSuspendedBreakpoints,
    broadcastChannelFactory,
  });
  const storeValue = useServiceStoreValue(
    state,
    refresh,
    mutations,
    pluginOperations,
    readers,
  );

  return (
    <ServiceStoreContext.Provider value={storeValue}>
      {children}
    </ServiceStoreContext.Provider>
  );
}

/**
 * 读取服务共享状态；缺少 Provider 属于装配错误并直接抛出。
 */
export function useServiceStore(): ServiceStoreValue {
  const value = useContext(ServiceStoreContext);
  if (value === null) {
    throw new Error(i18n.t("error.web.providerMissing"));
  }
  return value;
}
