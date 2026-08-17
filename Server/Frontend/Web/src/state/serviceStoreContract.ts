import type { PropsWithChildren } from "react";

import type {
  ClientCertificateImportSelection,
  ControlClient,
  MapLocalImportSelection,
  MediaPreviewBody,
  TransactionPageRequest,
} from "../api/controlClient";
import type { EventConnectionState, EventStreamClient } from "../api/eventClient";
import type {
  AdvancedRepeatJob,
  AdvancedRepeatStartRequest,
  AutoSaveConfiguration,
  AuxiliaryListenerPublicState,
  BlockCookiesConfiguration,
  BlockListConfiguration,
  BreakpointsConfiguration,
  ClientCertificateUpdate,
  ComposeRequest,
  ComposeRequestOverrides,
  ComposeResult,
  ConfigurationUpdate,
  DecodedProtobufView,
  DnsSpoofingConfiguration,
  EncodedBodyResponse,
  ExportRequest,
  ManagementApiKeyResponse,
  ManagementIdentity,
  ManagementSessionResponse,
  MapLocalConfiguration,
  MapLocalImportResult,
  MapRemoteConfiguration,
  McpConfiguration,
  MessageSide,
  MirrorConfiguration,
  MultiAccountPublicState,
  NoCachingConfiguration,
  PacketFilterConfiguration,
  PluginConfigurationUpdate,
  PluginDetails,
  PluginSnapshot,
  PortForwardEntry,
  ProcessSelectionSnapshot,
  ProcessSelectionUpdate,
  ProtobufConfiguration,
  ProtobufConfigurationUpdate,
  ProtobufDescriptorUpload,
  ReverseProxyEntry,
  RewriteConfiguration,
  ServiceSnapshot,
  SslConfiguration,
  SuspendedBreakpoint,
  ThrottlingConfiguration,
  TransactionDetail,
  TransactionPage,
  ValidateConfiguration,
  ValidateRequest,
  ValidationReport,
} from "../api/protocol";

export type ControlConnectionState = "connecting" | "connected" | "disconnected";

/**
 * 标识当前唯一控制写入的归属区域。
 *
 * 运行上下文：控制面一次只允许一个写入请求，归属字段让工具栏只反馈发起操作的控件。
 * 失败语义：空值表示没有占用写入槽位；未知归属不会进入类型系统。
 */
export type ControlActionScope =
  | "service"
  | "recording"
  | "recordingClear"
  | "configuration"
  | "mcp"
  | "ssl"
  | "sslRoot"
  | "sslClientCertificate"
  | "tool"
  | "breakpoint"
  | "repeat"
  | "plugin";

/** 服务状态与控制连接状态的只读投影；所有页面共享同一份权威快照。 */
export interface ServiceStoreState {
  snapshot: ServiceSnapshot | null;
  controlConnection: ControlConnectionState;
  eventConnection: EventConnectionState;
  connectionMessage: string;
  actionPending: boolean;
  activeAction: ControlActionScope | null;
  lastError: string | null;
  suspendedBreakpoints: SuspendedBreakpoint[];
}

/**
 * 页面可消费的完整 Store 契约；方法只定义稳定输入输出，错误展示与写入互斥由实现统一处理。
 */
export interface ServiceStoreValue extends ServiceStoreState {
  refresh(): Promise<void>;
  listTransactions(page?: TransactionPageRequest, signal?: AbortSignal): Promise<TransactionPage>;
  toggleService(): Promise<void>;
  toggleRecording(): Promise<void>;
  clearRecording(): Promise<void>;
  getTransactionDetail(transactionId: string, signal?: AbortSignal): Promise<TransactionDetail>;
  getLiveTransactionDetail(transactionId: string, revision: string, signal?: AbortSignal): Promise<TransactionDetail>;
  getTransactionBody(transactionId: string, side: MessageSide, signal?: AbortSignal): Promise<EncodedBodyResponse>;
  getResponseMediaPreview(transactionId: string, signal?: AbortSignal): Promise<MediaPreviewBody>;
  decodeProtobuf(transactionId: string, side: MessageSide, signal?: AbortSignal): Promise<DecodedProtobufView>;
  getProtobufConfiguration(signal?: AbortSignal): Promise<ProtobufConfiguration>;
  getValidateConfiguration(signal?: AbortSignal): Promise<ValidateConfiguration>;
  validateResponse(transactionId: string, request: ValidateRequest, signal?: AbortSignal): Promise<ValidationReport>;
  getValidationReports(transactionId: string, signal?: AbortSignal): Promise<ValidationReport[]>;
  composeRequest(request: ComposeRequest): Promise<ComposeResult | null>;
  repeatTransaction(transactionId: string, overrides?: ComposeRequestOverrides): Promise<ComposeResult | null>;
  startAdvancedRepeat(request: AdvancedRepeatStartRequest): Promise<AdvancedRepeatJob | null>;
  getAdvancedRepeat(jobId: string): Promise<AdvancedRepeatJob>;
  cancelAdvancedRepeat(jobId: string): Promise<AdvancedRepeatJob | null>;
  updateConfiguration(update: ConfigurationUpdate): Promise<boolean>;
  updateMcpConfiguration(configuration: McpConfiguration): Promise<void>;
  getManagementIdentity(): Promise<ManagementIdentity | null>;
  getMultiAccountState(signal?: AbortSignal): Promise<MultiAccountPublicState | null>;
  updateManagementIdentity(username: string, password: string): Promise<ManagementApiKeyResponse | null>;
  getManagementApiKey(): Promise<ManagementApiKeyResponse | null>;
  createManagementSession(): Promise<ManagementSessionResponse | null>;
  getProcesses(signal?: AbortSignal): Promise<ProcessSelectionSnapshot>;
  updateProcessSelection(update: ProcessSelectionUpdate, signal?: AbortSignal): Promise<ProcessSelectionSnapshot>;
  listPlugins(signal?: AbortSignal): Promise<PluginSnapshot[]>;
  getPluginDetails(pluginId: string, signal?: AbortSignal): Promise<PluginDetails>;
  setPluginEnabled(pluginId: string, enabled: boolean): Promise<boolean>;
  updatePluginConfiguration(pluginId: string, update: PluginConfigurationUpdate): Promise<PluginDetails | null>;
  reloadPlugin(pluginId: string): Promise<boolean>;
  installPluginPackage(packageFile: File): Promise<boolean>;
  uninstallPlugin(pluginId: string): Promise<boolean>;
  updateSsl(update: SslConfiguration): Promise<boolean>;
  regenerateSslRoot(): Promise<boolean>;
  exportSslRoot(format: "pem" | "cer"): Promise<Blob>;
  importClientCertificate(input: ClientCertificateImportSelection): Promise<boolean>;
  updateClientCertificate(id: string, update: ClientCertificateUpdate): Promise<boolean>;
  removeClientCertificate(id: string): Promise<boolean>;
  updateBlockList(update: BlockListConfiguration): Promise<boolean>;
  updatePacketFilters(update: PacketFilterConfiguration): Promise<boolean>;
  updateNoCaching(update: NoCachingConfiguration): Promise<boolean>;
  updateBlockCookies(update: BlockCookiesConfiguration): Promise<boolean>;
  updateDnsSpoofing(update: DnsSpoofingConfiguration): Promise<boolean>;
  updateMapLocal(update: MapLocalConfiguration): Promise<boolean>;
  importMapLocalFiles(selection: MapLocalImportSelection): Promise<MapLocalImportResult | null>;
  updateMapRemote(update: MapRemoteConfiguration): Promise<boolean>;
  updateRewrite(update: RewriteConfiguration): Promise<boolean>;
  updateBreakpoints(update: BreakpointsConfiguration): Promise<boolean>;
  updateThrottling(update: ThrottlingConfiguration): Promise<boolean>;
  updateProtobufConfiguration(update: ProtobufConfigurationUpdate): Promise<boolean>;
  uploadProtobufDescriptor(upload: ProtobufDescriptorUpload): Promise<boolean>;
  updateMirror(update: MirrorConfiguration): Promise<boolean>;
  updateAutoSave(update: AutoSaveConfiguration): Promise<boolean>;
  saveAutoSaveNow(): Promise<boolean>;
  updateReverseProxies(update: ReverseProxyEntry[]): Promise<boolean>;
  updatePortForwards(update: PortForwardEntry[]): Promise<boolean>;
  getReverseProxies(): Promise<AuxiliaryListenerPublicState>;
  getPortForwards(): Promise<AuxiliaryListenerPublicState>;
  refreshSuspendedBreakpoints(): Promise<void>;
  continueBreakpoint(transactionId: string, draft: SuspendedBreakpoint["draft"]): Promise<boolean>;
  abortBreakpoint(transactionId: string): Promise<boolean>;
  exportRecording(request: ExportRequest): Promise<Blob>;
}

/** Provider 允许测试注入控制与事件客户端；生产环境未传入时创建真实 HTTP/SSE 客户端。 */
export interface ServiceProviderProps extends PropsWithChildren {
  controlClient?: ControlClient;
  eventClient?: EventStreamClient;
  broadcastChannelFactory?: (name: string) => BroadcastChannel;
}
