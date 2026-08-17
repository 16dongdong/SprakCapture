import { z, type ZodType, type ZodTypeDef } from "zod";

import { resolveControlBaseUrl } from "./controlEndpoint";
import {
  autoSaveConfigurationSchema,
  autoSavePublicStateSchema,
  auxiliaryListenerPublicStateSchema,
  blockCookiesConfigurationSchema,
  blockListConfigurationSchema,
  breakpointsConfigurationSchema,
  advancedRepeatJobSchema,
  advancedRepeatStartRequestSchema,
  composeRequestSchema,
  composeResultSchema,
  composeRequestOverridesSchema,
  configurationUpdateSchema,
  dnsSpoofingConfigurationSchema,
  decodedProtobufViewSchema,
  encodedBodyResponseSchema,
  exportRequestSchema,
  mapLocalConfigurationSchema,
  mapLocalImportResultSchema,
  mapRemoteConfigurationSchema,
  managementApiKeyResponseSchema,
  managementIdentitySchema,
  managementSessionResponseSchema,
  maximumTransactionCollectionTokenCharacters,
  maximumPluginPackageBytes,
  noCachingConfigurationSchema,
  packetFilterConfigurationSchema,
  mirrorConfigurationSchema,
  pluginConfigurationUpdateSchema,
  pluginDetailsSchema,
  pluginPlatformConfigurationSchema,
  pluginSnapshotSchema,
  pluginUserConfigurationSchema,
  extensionInstanceSnapshotSchema,
  extensionInvocationTraceSchema,
  processSelectionSnapshotSchema,
  processSelectionUpdateSchema,
  publicMultiAccountConfigurationSchema,
  portForwardEntrySchema,
  protobufConfigurationSchema,
  protobufConfigurationUpdateSchema,
  protobufDescriptorUploadSchema,
  reverseProxyEntrySchema,
  recordingResponseSchema,
  recordingUpdateSchema,
  rewriteConfigurationSchema,
  serviceSnapshotSchema,
  sslConfigurationSchema,
  sslPublicStateSchema,
  clientCertificateFormatSchema,
  clientCertificateUpdateSchema,
  suspendedBreakpointSchema,
  throttlingConfigurationSchema,
  transactionDetailSchema,
  transactionPageSchema,
  toolsPublicStateSchema,
  uiContextSnapshotSchema,
  validateConfigurationSchema,
  validateRequestSchema,
  validationReportSchema,
  type BlockCookiesConfiguration,
  type BlockListConfiguration,
  type BreakpointsConfiguration,
  type AdvancedRepeatJob,
  type AdvancedRepeatStartRequest,
  type ComposeRequest,
  type ComposeRequestOverrides,
  type ComposeResult,
  type McpConfiguration,
  type ConfigurationUpdate,
  type DecodedProtobufView,
  type AutoSaveConfiguration,
  type AutoSavePublicState,
  type AuxiliaryListenerPublicState,
  type EncodedBodyResponse,
  type EditableHttpMessage,
  type DnsSpoofingConfiguration,
  type ExportRequest,
  type MapLocalConfiguration,
  type MapLocalImportResult,
  type LocationPattern,
  type MapRemoteConfiguration,
  type ManagementApiKeyResponse,
  type ManagementIdentity,
  type ManagementSessionResponse,
  type MultiAccountPublicState,
  type NoCachingConfiguration,
  type PacketFilterConfiguration,
  type MirrorConfiguration,
  type PluginConfigurationUpdate,
  type PluginDetails,
  type PluginPlatformConfiguration,
  type PluginSnapshot,
  type PluginUserConfiguration,
  type ExtensionInstanceSnapshot,
  type ExtensionInvocationTrace,
  type ProcessSelectionSnapshot,
  type ProcessSelectionUpdate,
  type PortForwardEntry,
  type ProtobufConfiguration,
  type ProtobufConfigurationUpdate,
  type ProtobufDescriptorUpload,
  type RecordingResponse,
  type RecordingUpdate,
  type RewriteConfiguration,
  type ReverseProxyEntry,
  type ServiceSnapshot,
  type SslConfiguration,
  type SslPublicState,
  type ClientCertificateFormat,
  type ClientCertificateUpdate,
  type SuspendedBreakpoint,
  type ThrottlingConfiguration,
  type TransactionDetail,
  type TransactionPage,
  type ToolsPublicState,
  type UiContextSnapshot,
  type UiContextUpdate,
  type ValidateConfiguration,
  type ValidateRequest,
  type ValidationReport,
} from "./protocol";
import i18n, { currentRequestLocale } from "../i18n";

export { defaultControlBaseUrl } from "./controlEndpoint";
const maximumTransactionPageSize = 1_000;
const maximumRootCertificateBytes = 1024 * 1024;
const maximumExportBytes = 64 * 1024 * 1024;
const controlErrorSchema = z
  .object({
    message: z.string().min(1),
    code: z.string().min(1),
    messageKey: z.string().min(1),
    params: z.record(z.string()),
  })
  .strict();

export class ControlClientError extends Error {
  readonly statusCode: number | null;

  /**
   * 保存控制接口失败的稳定状态码；网络层未返回响应时状态码为 null，原始异常保留在 cause。
   */
  constructor(
    message: string,
    statusCode: number | null = null,
    cause?: unknown,
  ) {
    super(message, { cause });
    this.name = "ControlClientError";
    this.statusCode = statusCode;
  }
}

/**
 * 描述一次有界事务读取；collectionToken 只在使用上一页非零 nextOffset 时出现。
 */
export interface TransactionPageRequest {
  offset?: number;
  limit?: number;
  collectionToken?: string;
}

/** 描述媒体预览惰性流；浏览器媒体元素直接消费 URL，不在控制客户端聚合 Blob。 */
export interface MediaPreviewBody {
  status: "complete" | "continuousPrefix" | "incomplete";
  streamUrl: string | null;
  mimeType: string;
  capturedBytes: number;
  totalBytes: number;
  segmentCount: number;
}

/** 保存一次浏览器文件选择中的正文和相对路径；目录选择使用 webkitRelativePath 保留层级。 */
export interface MapLocalImportFile {
  file: File;
  relativePath: string;
}

/** 描述 Map Local 文件或目录导入；directory 决定后端是否要求所有文件共享同一根目录。 */
export interface MapLocalImportSelection {
  directory: boolean;
  files: readonly MapLocalImportFile[];
}

/** 描述一次客户端证书文件选择；PEM/DER 需要 privateKey，PKCS#12/PFX 使用 password 解包。 */
export interface ClientCertificateImportSelection {
  name: string;
  format: ClientCertificateFormat;
  enabled: boolean;
  locations: readonly LocationPattern[];
  certificate: File;
  privateKey?: File;
  password?: string;
}

export interface ControlClient {
  updateUiContext(
    update: UiContextUpdate,
    signal?: AbortSignal,
  ): Promise<UiContextSnapshot>;
  getSnapshot(signal?: AbortSignal): Promise<ServiceSnapshot>;
  startService(signal?: AbortSignal): Promise<ServiceSnapshot>;
  stopService(signal?: AbortSignal): Promise<ServiceSnapshot>;
  updateConfiguration(
    update: ConfigurationUpdate,
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot>;
  getManagementIdentity(signal?: AbortSignal): Promise<ManagementIdentity>;
  getMultiAccountState(signal?: AbortSignal): Promise<MultiAccountPublicState>;
  updateManagementIdentity(
    username: string,
    password: string,
    signal?: AbortSignal,
  ): Promise<ManagementApiKeyResponse>;
  getManagementApiKey(signal?: AbortSignal): Promise<ManagementApiKeyResponse>;
  createManagementSession(signal?: AbortSignal): Promise<ManagementSessionResponse>;
  updateMcpConfiguration(
    configuration: McpConfiguration,
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot>;
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
  setPluginEnabled(
    pluginId: string,
    enabled: boolean,
    signal?: AbortSignal,
  ): Promise<PluginSnapshot>;
  updatePluginConfiguration(
    pluginId: string,
    update: PluginConfigurationUpdate,
    signal?: AbortSignal,
  ): Promise<PluginDetails>;
  reloadPlugin(pluginId: string, signal?: AbortSignal): Promise<PluginSnapshot>;
  installPluginPackage(
    packageFile: File,
    signal?: AbortSignal,
  ): Promise<PluginSnapshot>;
  uninstallPlugin(pluginId: string, signal?: AbortSignal): Promise<void>;
  getExtensionPlatformConfiguration(
    signal?: AbortSignal,
  ): Promise<PluginPlatformConfiguration>;
  updateExtensionPlatformConfiguration(
    pluginId: string,
    configuration: PluginUserConfiguration,
    signal?: AbortSignal,
  ): Promise<PluginPlatformConfiguration>;
  removeExtensionPlatformConfiguration(
    pluginId: string,
    signal?: AbortSignal,
  ): Promise<PluginPlatformConfiguration>;
  getExtensionRuntimeSnapshots(
    signal?: AbortSignal,
  ): Promise<ExtensionInstanceSnapshot[]>;
  getExtensionInvocationTraces(
    signal?: AbortSignal,
  ): Promise<ExtensionInvocationTrace[]>;
  clearExtensionInvocationTraces(signal?: AbortSignal): Promise<void>;
  getSsl(signal?: AbortSignal): Promise<SslPublicState>;
  updateSsl(
    update: SslConfiguration,
    signal?: AbortSignal,
  ): Promise<SslPublicState>;
  regenerateSslRoot(signal?: AbortSignal): Promise<SslPublicState>;
  exportSslRoot(format: "pem" | "cer", signal?: AbortSignal): Promise<Blob>;
  importClientCertificate(
    input: ClientCertificateImportSelection,
    signal?: AbortSignal,
  ): Promise<SslPublicState>;
  updateClientCertificate(
    id: string,
    update: ClientCertificateUpdate,
    signal?: AbortSignal,
  ): Promise<SslPublicState>;
  removeClientCertificate(
    id: string,
    signal?: AbortSignal,
  ): Promise<SslPublicState>;
  clearSessions(signal?: AbortSignal): Promise<ServiceSnapshot>;
  getRecording(signal?: AbortSignal): Promise<RecordingResponse>;
  updateRecording(
    update: RecordingUpdate,
    signal?: AbortSignal,
  ): Promise<RecordingResponse>;
  clearRecording(signal?: AbortSignal): Promise<RecordingResponse>;
  listTransactions(
    page?: TransactionPageRequest,
    signal?: AbortSignal,
  ): Promise<TransactionPage>;
  getTransactionDetail(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<TransactionDetail>;
  getRequestBody(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<EncodedBodyResponse>;
  getResponseBody(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<EncodedBodyResponse>;
  getResponseMediaPreview(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<MediaPreviewBody>;
  decodeProtobuf(
    transactionId: string,
    side: "request" | "response",
    signal?: AbortSignal,
  ): Promise<DecodedProtobufView>;
  getProtobufConfiguration(
    signal?: AbortSignal,
  ): Promise<ProtobufConfiguration>;
  updateProtobufConfiguration(
    update: ProtobufConfigurationUpdate,
    signal?: AbortSignal,
  ): Promise<ProtobufConfiguration>;
  uploadProtobufDescriptor(
    upload: ProtobufDescriptorUpload,
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
  composeRequest(
    request: ComposeRequest,
    signal?: AbortSignal,
  ): Promise<ComposeResult>;
  repeatTransaction(
    transactionId: string,
    overrides?: ComposeRequestOverrides,
    signal?: AbortSignal,
  ): Promise<ComposeResult>;
  startAdvancedRepeat(
    request: AdvancedRepeatStartRequest,
    signal?: AbortSignal,
  ): Promise<AdvancedRepeatJob>;
  listAdvancedRepeats(signal?: AbortSignal): Promise<AdvancedRepeatJob[]>;
  getAdvancedRepeat(
    jobId: string,
    signal?: AbortSignal,
  ): Promise<AdvancedRepeatJob>;
  cancelAdvancedRepeat(
    jobId: string,
    signal?: AbortSignal,
  ): Promise<AdvancedRepeatJob>;
  updateBlockList(
    update: BlockListConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updatePacketFilters(
    update: PacketFilterConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateNoCaching(
    update: NoCachingConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateBlockCookies(
    update: BlockCookiesConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateDnsSpoofing(
    update: DnsSpoofingConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateMapLocal(
    update: MapLocalConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  importMapLocalFiles(
    selection: MapLocalImportSelection,
    signal?: AbortSignal,
  ): Promise<MapLocalImportResult>;
  updateMapRemote(
    update: MapRemoteConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateRewrite(
    update: RewriteConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateBreakpoints(
    update: BreakpointsConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateThrottling(
    update: ThrottlingConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateMirror(
    update: MirrorConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  updateAutoSave(
    update: AutoSaveConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState>;
  saveAutoSaveNow(signal?: AbortSignal): Promise<AutoSavePublicState>;
  getReverseProxies(
    signal?: AbortSignal,
  ): Promise<AuxiliaryListenerPublicState>;
  updateReverseProxies(
    update: ReverseProxyEntry[],
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot>;
  getPortForwards(signal?: AbortSignal): Promise<AuxiliaryListenerPublicState>;
  updatePortForwards(
    update: PortForwardEntry[],
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot>;
  listSuspendedBreakpoints(
    signal?: AbortSignal,
  ): Promise<SuspendedBreakpoint[]>;
  continueBreakpoint(
    transactionId: string,
    draft: EditableHttpMessage,
    signal?: AbortSignal,
  ): Promise<void>;
  abortBreakpoint(transactionId: string, signal?: AbortSignal): Promise<void>;
  exportRecording(request: ExportRequest, signal?: AbortSignal): Promise<Blob>;
}

export class HttpControlClient implements ControlClient {
  private readonly baseUrl: string;
  private readonly requestFetch: typeof fetch;

  /**
   * 创建严格 HTTP 控制客户端；默认直连本机守护进程，显式地址仅用于部署覆盖和测试注入。
   *
   * 运行上下文：ServiceProvider 创建 REST 控制面时调用，必须与事件流使用同一份控制基础地址。
   * 参数：baseUrl 为绝对 HTTP(S) 控制地址；requestFetch 为浏览器请求实现，测试可注入替身。
   * 失败语义：baseUrl 含凭据、查询片段、首尾空白或不是绝对 HTTP(S) 地址时抛出 TypeError，拒绝生成不确定请求地址。
   */
  constructor(
    baseUrl?: string,
    requestFetch: typeof fetch = (input, init) => globalThis.fetch(input, init),
  ) {
    this.baseUrl = resolveControlBaseUrl(baseUrl);
    this.requestFetch = requestFetch;
  }

  /**
   * 上报当前浏览器窗口正在展示的页面与稳定资源标识；响应是服务端确认后的活跃窗口集合。
   *
   * 运行上下文：UI 上下文提供器在路由、焦点、选择变化和心跳时调用；失败不会改变业务快照。
   * 参数：update 必须携带窗口内单调 sequence；signal 用于页面卸载时取消在途请求。
   * 失败语义：网络失败或协议漂移会显式拒绝 Promise，调用方只记录有界诊断并等待下次心跳。
   */
  updateUiContext(
    update: UiContextUpdate,
    signal?: AbortSignal,
  ): Promise<UiContextSnapshot> {
    return this.request(
      "/api/v1/ui/context",
      { method: "PUT", body: JSON.stringify(update), signal },
      uiContextSnapshotSchema,
    );
  }

  /** 热启停内置 MCP 并持久化端口；成功响应是包含真实监听结果的完整权威快照。 */
  updateMcpConfiguration(
    configuration: McpConfiguration,
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot> {
    return this.request(
      "/api/v1/mcp",
      {
        method: "PUT",
        body: JSON.stringify(configuration),
        signal,
      },
      serviceSnapshotSchema,
    );
  }

  /** 提交代理进程 DNS 映射规则；主机名保持不变，仅替换新建出站连接使用的目标 IP。 */
  updateDnsSpoofing(
    update: DnsSpoofingConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "dnsSpoofing",
      update,
      dnsSpoofingConfigurationSchema,
      signal,
    );
  }

  /**
   * 获取一次完整服务快照；响应字段漂移会以协议错误显式失败，取消时保留 AbortError。
   */
  getSnapshot(signal?: AbortSignal): Promise<ServiceSnapshot> {
    return this.request(
      "/api/v1/snapshot",
      { method: "GET", signal },
      serviceSnapshotSchema,
    );
  }

  /**
   * 请求启动服务；后端返回的权威快照决定最终界面状态，不在请求期间伪造成功状态。
   */
  startService(signal?: AbortSignal): Promise<ServiceSnapshot> {
    return this.request(
      "/api/v1/service/start",
      { method: "POST", signal },
      serviceSnapshotSchema,
    );
  }

  /**
   * 请求停止服务；调用方取消等待时仅终止控制请求，不自行推断后端生命周期结果。
   */
  stopService(signal?: AbortSignal): Promise<ServiceSnapshot> {
    return this.request(
      "/api/v1/service/stop",
      { method: "POST", signal },
      serviceSnapshotSchema,
    );
  }

  /**
   * 提交完整服务配置；认证口令只存在于本次请求体且不会写入客户端日志。
   */
  updateConfiguration(
    update: ConfigurationUpdate,
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot> {
    const parsedUpdate = configurationUpdateSchema.safeParse(update);
    if (!parsedUpdate.success) {
      throw new ControlClientError(i18n.t("error.web.configurationProtocol"));
    }
    return this.request(
      "/api/v1/configuration",
      {
        method: "PUT",
        body: JSON.stringify(parsedUpdate.data),
        signal,
      },
      serviceSnapshotSchema,
    );
  }

  /** 读取脱敏管理员身份；响应不会携带密码或完整 API Key。 */
  getManagementIdentity(signal?: AbortSignal): Promise<ManagementIdentity> {
    return this.request(
      "/api/v1/multiAccount/identity",
      { method: "GET", signal },
      managementIdentitySchema,
    );
  }

  /** 读取账号服务的局部实时快照；高频概览刷新不触发完整控制快照重建。 */
  getMultiAccountState(signal?: AbortSignal): Promise<MultiAccountPublicState> {
    return this.request(
      "/api/v1/multiAccount",
      { method: "GET", signal },
      publicMultiAccountConfigurationSchema,
    );
  }

  /** 修改管理员凭据并接收本次直接响应中的新 Key；调用方必须限制其内存生命周期。 */
  updateManagementIdentity(
    username: string,
    password: string,
    signal?: AbortSignal,
  ): Promise<ManagementApiKeyResponse> {
    return this.request(
      "/api/v1/multiAccount/identity",
      {
        method: "PUT",
        body: JSON.stringify({ username, password }),
        signal,
      },
      managementApiKeyResponseSchema,
    );
  }

  /** 读取由当前管理员凭据确定性派生的完整 Key；请求不携带密码或其他秘密字段。 */
  getManagementApiKey(signal?: AbortSignal): Promise<ManagementApiKeyResponse> {
    return this.request(
      "/api/v1/multiAccount/apiKey",
      { method: "GET", signal },
      managementApiKeyResponseSchema,
    );
  }

  /** 创建仅用于同源账号管理 iframe 的一次性路径；响应不允许携带主机或端口。 */
  createManagementSession(signal?: AbortSignal): Promise<ManagementSessionResponse> {
    return this.request(
      "/api/v1/multiAccount/managementSession",
      { method: "POST", signal },
      managementSessionResponseSchema,
    );
  }

  /** 刷新运行中进程与已保存路径；响应由严格 schema 校验，避免进程管理页消费漂移字段。 */
  getProcesses(signal?: AbortSignal): Promise<ProcessSelectionSnapshot> {
    return this.request(
      "/api/v1/processes",
      { method: "GET", signal },
      processSelectionSnapshotSchema,
    );
  }

  /** 持久化进程路径选择；后端热更新实时 PID 与 WinDivert 捕获状态，不重启公开代理监听器。 */
  updateProcessSelection(
    update: ProcessSelectionUpdate,
    signal?: AbortSignal,
  ): Promise<ProcessSelectionSnapshot> {
    const parsedUpdate = processSelectionUpdateSchema.safeParse(update);
    if (!parsedUpdate.success) {
      throw new ControlClientError(i18n.t("error.web.configurationProtocol"));
    }
    return this.request(
      "/api/v1/processes",
      { method: "PUT", body: JSON.stringify(parsedUpdate.data), signal },
      processSelectionSnapshotSchema,
    );
  }

  /** 读取插件宿主发现的稳定列表；详情配置按需读取，避免把秘密或大配置塞入常规快照。 */
  listPlugins(signal?: AbortSignal): Promise<PluginSnapshot[]> {
    return this.request(
      "/api/v1/plugins",
      { method: "GET", signal },
      z.array(pluginSnapshotSchema),
    );
  }

  /** 按需读取单插件的声明式配置详情；路径段按 UTF-8 编码，防止 ID 影响路由层级。 */
  getPluginDetails(
    pluginId: string,
    signal?: AbortSignal,
  ): Promise<PluginDetails> {
    return this.request(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}`,
      { method: "GET", signal },
      pluginDetailsSchema,
    );
  }

  /** 切换单插件启停状态；最终状态以宿主返回快照为准，不在浏览器侧预设成功结果。 */
  setPluginEnabled(
    pluginId: string,
    enabled: boolean,
    signal?: AbortSignal,
  ): Promise<PluginSnapshot> {
    return this.request(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}/enabled`,
      {
        method: "PUT",
        body: JSON.stringify({ enabled }),
        signal,
      },
      pluginSnapshotSchema,
    );
  }

  /** 提交声明式插件配置；本地先校验请求包络，字段约束和秘密字段合并始终由后端 manifest 决定。 */
  updatePluginConfiguration(
    pluginId: string,
    update: PluginConfigurationUpdate,
    signal?: AbortSignal,
  ): Promise<PluginDetails> {
    const parsedUpdate = pluginConfigurationUpdateSchema.safeParse(update);
    if (!parsedUpdate.success) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}/configuration`,
      {
        method: "PUT",
        body: JSON.stringify(parsedUpdate.data),
        signal,
      },
      pluginDetailsSchema,
    );
  }

  /** 请求宿主重载已安装插件；禁用插件只返回当前快照，不隐式改变用户选择。 */
  reloadPlugin(
    pluginId: string,
    signal?: AbortSignal,
  ): Promise<PluginSnapshot> {
    return this.request(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}/reload`,
      { method: "POST", signal },
      pluginSnapshotSchema,
    );
  }

  /** 上传有界 .tplugin.zip；二进制正文不转 Base64，避免 DLL 包在浏览器内存中额外复制。 */
  installPluginPackage(
    packageFile: File,
    signal?: AbortSignal,
  ): Promise<PluginSnapshot> {
    if (packageFile.size <= 0 || packageFile.size > maximumPluginPackageBytes) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      "/api/v1/plugins/packages",
      {
        method: "POST",
        body: packageFile,
        headers: { "Content-Type": "application/zip" },
        signal,
      },
      pluginSnapshotSchema,
    );
  }

  /** 删除已停用且无活动连接的插件包；204 语义由 requestEmpty 精确核对。 */
  uninstallPlugin(pluginId: string, signal?: AbortSignal): Promise<void> {
    return this.requestEmpty(
      `/api/v1/plugins/${encodeURIComponent(pluginId)}`,
      {
        method: "DELETE",
        signal,
      },
    );
  }

  /** 读取开放扩展平台的完整持久化配置；浏览器不生成能力授权或运行门禁。 */
  getExtensionPlatformConfiguration(
    signal?: AbortSignal,
  ): Promise<PluginPlatformConfiguration> {
    return this.request(
      "/api/v1/extensions/configuration",
      { method: "GET", signal },
      pluginPlatformConfigurationSchema,
    );
  }

  /** 原子保存单个 Mod 的启停、版本、顺序、订阅、运行参数和自有配置。 */
  updateExtensionPlatformConfiguration(
    pluginId: string,
    configuration: PluginUserConfiguration,
    signal?: AbortSignal,
  ): Promise<PluginPlatformConfiguration> {
    const parsedConfiguration = pluginUserConfigurationSchema.safeParse(configuration);
    if (!parsedConfiguration.success || pluginId.trim().length === 0) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      `/api/v1/extensions/configuration/${encodeURIComponent(pluginId)}`,
      {
        method: "PUT",
        body: JSON.stringify(parsedConfiguration.data),
        signal,
      },
      pluginPlatformConfigurationSchema,
    );
  }

  /** 删除单个 Mod 的宿主配置；插件包和其他 Mod 的配置保持不变。 */
  removeExtensionPlatformConfiguration(
    pluginId: string,
    signal?: AbortSignal,
  ): Promise<PluginPlatformConfiguration> {
    if (pluginId.trim().length === 0) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      `/api/v1/extensions/configuration/${encodeURIComponent(pluginId)}`,
      { method: "DELETE", signal },
      pluginPlatformConfigurationSchema,
    );
  }

  /** 读取所有已发布运行实例的即时状态，不返回第三方配置正文或文件路径。 */
  getExtensionRuntimeSnapshots(
    signal?: AbortSignal,
  ): Promise<ExtensionInstanceSnapshot[]> {
    return this.request(
      "/api/v1/extensions/runtime",
      { method: "GET", signal },
      z.array(extensionInstanceSnapshotSchema),
    );
  }

  /** 读取扩展内核最近的有界调用追踪，供开发者定位阶段、动作、耗时和失败代码。 */
  getExtensionInvocationTraces(
    signal?: AbortSignal,
  ): Promise<ExtensionInvocationTrace[]> {
    return this.request(
      "/api/v1/extensions/traces",
      { method: "GET", signal },
      z.array(extensionInvocationTraceSchema),
    );
  }

  /** 清空扩展调用追踪；运行实例、执行计划和调用状态不受影响。 */
  clearExtensionInvocationTraces(signal?: AbortSignal): Promise<void> {
    return this.requestEmpty("/api/v1/extensions/traces", {
      method: "DELETE",
      signal,
    });
  }

  /**
   * 读取 SSL 主机范围、公开根证书信息与握手统计；私钥字段不属于客户端协议。
   */
  getSsl(signal?: AbortSignal): Promise<SslPublicState> {
    return this.request(
      "/api/v1/ssl",
      { method: "GET", signal },
      sslPublicStateSchema,
    );
  }

  /**
   * 提交完整 SSL 配置；本地先拒绝非法规则结构和无界叶证书缓存值。
   */
  updateSsl(
    update: SslConfiguration,
    signal?: AbortSignal,
  ): Promise<SslPublicState> {
    const parsedUpdate = sslConfigurationSchema.safeParse(update);
    if (!parsedUpdate.success) {
      throw new ControlClientError(i18n.t("error.web.sslProtocol"));
    }
    return this.request(
      "/api/v1/ssl",
      {
        method: "PUT",
        body: JSON.stringify(parsedUpdate.data),
        signal,
      },
      sslPublicStateSchema,
    );
  }

  /**
   * 请求更换根 CA；交互确认由对话框负责，客户端只校验返回公开状态。
   */
  regenerateSslRoot(signal?: AbortSignal): Promise<SslPublicState> {
    return this.request(
      "/api/v1/ssl/ca/generate",
      { method: "POST", signal },
      sslPublicStateSchema,
    );
  }

  /**
   * 下载有界根证书；格式枚举固定，响应过大视为控制协议损坏而不进入下载流程。
   */
  exportSslRoot(format: "pem" | "cer", signal?: AbortSignal): Promise<Blob> {
    return this.requestBoundedBinary(
      `/api/v1/ssl/ca/export?format=${format}`,
      { method: "GET", signal },
      "application/x-pem-file, application/pkix-cert",
      maximumRootCertificateBytes,
    );
  }

  /** 上传客户端身份文件；浏览器生成 multipart boundary，口令不会进入 URL 或日志。 */
  importClientCertificate(
    input: ClientCertificateImportSelection,
    signal?: AbortSignal,
  ): Promise<SslPublicState> {
    const parsedFormat = clientCertificateFormatSchema.safeParse(input.format);
    if (
      !parsedFormat.success ||
      input.name.trim().length === 0 ||
      input.locations.length === 0
    ) {
      throw new ControlClientError(i18n.t("error.web.sslProtocol"));
    }
    const form = new FormData();
    form.append("name", input.name.trim());
    form.append("format", JSON.stringify(parsedFormat.data));
    form.append("enabled", JSON.stringify(input.enabled));
    form.append("locations", JSON.stringify(input.locations));
    form.append("certificate", input.certificate);
    if (input.privateKey !== undefined) {
      form.append("privateKey", input.privateKey);
    }
    form.append("password", input.password ?? "");
    return this.request(
      "/api/v1/ssl/client-certificates",
      { method: "POST", body: form, signal },
      sslPublicStateSchema,
    );
  }

  /** 更新客户端身份的显示名、开关和 Location 规则，不允许替换密钥材料。 */
  updateClientCertificate(
    id: string,
    update: ClientCertificateUpdate,
    signal?: AbortSignal,
  ): Promise<SslPublicState> {
    const parsedUpdate = clientCertificateUpdateSchema.safeParse(update);
    if (!parsedUpdate.success || id.length === 0) {
      throw new ControlClientError(i18n.t("error.web.sslProtocol"));
    }
    return this.request(
      `/api/v1/ssl/client-certificates/${encodeURIComponent(id)}`,
      { method: "PUT", body: JSON.stringify(parsedUpdate.data), signal },
      sslPublicStateSchema,
    );
  }

  /** 删除客户端身份；后端返回新的完整 SSL 状态供 Store 原子替换。 */
  removeClientCertificate(
    id: string,
    signal?: AbortSignal,
  ): Promise<SslPublicState> {
    return this.request(
      `/api/v1/ssl/client-certificates/${encodeURIComponent(id)}`,
      { method: "DELETE", signal },
      sslPublicStateSchema,
    );
  }

  /**
   * 清空已结束会话；运行中会话是否保留以响应快照为准，取消不会改写本地快照。
   */
  clearSessions(signal?: AbortSignal): Promise<ServiceSnapshot> {
    return this.request(
      "/api/v1/sessions",
      { method: "DELETE", signal },
      serviceSnapshotSchema,
    );
  }

  /**
   * 读取当前录制状态与资源限额；AbortSignal 用于页面卸载时中止尚未完成的控制请求。
   */
  getRecording(signal?: AbortSignal): Promise<RecordingResponse> {
    return this.request(
      "/api/v1/recording",
      { method: "GET", signal },
      recordingResponseSchema,
    );
  }

  /**
   * 提交录制状态或限额的部分更新；本地先拒绝未知字段和超出后端固定边界的值。
   */
  updateRecording(
    update: RecordingUpdate,
    signal?: AbortSignal,
  ): Promise<RecordingResponse> {
    const parsedUpdate = recordingUpdateSchema.safeParse(update);
    if (!parsedUpdate.success) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      "/api/v1/recording",
      {
        method: "PUT",
        body: JSON.stringify(parsedUpdate.data),
        signal,
      },
      recordingResponseSchema,
    );
  }

  /**
   * 清空当前录制会话的事务和正文引用；返回 revision 用于裁决随后到达的事件顺序。
   */
  clearRecording(signal?: AbortSignal): Promise<RecordingResponse> {
    return this.request(
      "/api/v1/recording/clear",
      { method: "POST", signal },
      recordingResponseSchema,
    );
  }

  /**
   * 读取有界事务页；后续页必须使用上一响应的 nextOffset 和同一 collectionToken。
   */
  listTransactions(
    page: TransactionPageRequest = {},
    signal?: AbortSignal,
  ): Promise<TransactionPage> {
    const query = this.buildTransactionQuery(page);
    return this.request(
      `/api/v1/transactions${query}`,
      { method: "GET", signal },
      transactionPageSchema,
    );
  }

  /**
   * 按需读取单条事务详情；路径段只编码一次，服务端不存在时保留其 404 失败语义。
   */
  async getTransactionDetail(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<TransactionDetail> {
    const detail = await this.request(
      this.transactionPath(transactionId),
      { method: "GET", signal },
      transactionDetailSchema,
    );
    if (detail.transaction.transactionId !== transactionId) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return detail;
  }

  /**
   * 懒加载请求正文；base64 保持字符串形态，不在控制客户端创建第二份字节数组。
   */
  getRequestBody(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<EncodedBodyResponse> {
    return this.getBody(transactionId, "request", signal);
  }

  /**
   * 懒加载响应正文；取消信号原样传给 fetch，正文未消费完成时可立即终止读取。
   */
  getResponseBody(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<EncodedBodyResponse> {
    return this.getBody(transactionId, "response", signal);
  }

  /**
   * 读取响应媒体的只读虚拟正文；服务端只组合已验证的 Range，原始事务正文保持不变。
   * 未捕获起始片段时以 `incomplete` 成功状态返回，避免把坏分段交给浏览器解码器。
   */
  getResponseMediaPreview(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<MediaPreviewBody> {
    return this.getMediaPreviewDescriptor(transactionId, signal);
  }

  /**
   * 按当前描述符路由解码指定消息侧；服务端返回 decodeError 时仍是成功协议响应，查看器可回退 Hex。
   */
  decodeProtobuf(
    transactionId: string,
    side: "request" | "response",
    signal?: AbortSignal,
  ): Promise<DecodedProtobufView> {
    return this.request(
      `${this.transactionPath(transactionId)}/decode/protobuf?side=${side}`,
      { method: "GET", signal },
      decodedProtobufViewSchema,
    );
  }

  /** 读取描述符登记与路由配置；读取不返回原始字节，也不会改写检查器当前事务。 */
  getProtobufConfiguration(
    signal?: AbortSignal,
  ): Promise<ProtobufConfiguration> {
    return this.request(
      "/api/v1/tools/protobuf",
      { method: "GET", signal },
      protobufConfigurationSchema,
    );
  }

  /** 整体替换 Protobuf 开关与路由；schema 清单只由上传端点追加，避免客户端伪造文件状态。 */
  updateProtobufConfiguration(
    update: ProtobufConfigurationUpdate,
    signal?: AbortSignal,
  ): Promise<ProtobufConfiguration> {
    const parsedUpdate = protobufConfigurationUpdateSchema.safeParse(update);
    if (!parsedUpdate.success) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      "/api/v1/tools/protobuf",
      { method: "PUT", body: JSON.stringify(parsedUpdate.data), signal },
      protobufConfigurationSchema,
    );
  }

  /** 上传用户经文件选择器提供的 FileDescriptorSet；调用方负责在完成后丢弃临时 Base64。 */
  uploadProtobufDescriptor(
    upload: ProtobufDescriptorUpload,
    signal?: AbortSignal,
  ): Promise<ProtobufConfiguration> {
    const parsedUpload = protobufDescriptorUploadSchema.safeParse(upload);
    if (!parsedUpload.success) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      "/api/v1/tools/protobuf/schemas",
      { method: "POST", body: JSON.stringify(parsedUpload.data), signal },
      protobufConfigurationSchema,
    );
  }

  /** 获取 Validate 运行配置；读取不会发送录制正文，也不改变校验器启用状态。 */
  getValidateConfiguration(
    signal?: AbortSignal,
  ): Promise<ValidateConfiguration> {
    return this.request(
      "/api/v1/tools/validate",
      { method: "GET", signal },
      validateConfigurationSchema,
    );
  }

  /**
   * 对已录制响应执行单个校验器；W3C 请求只有调用方传入 onlineUploadConfirmed=true 时才可能外发正文。
   */
  validateResponse(
    transactionId: string,
    request: ValidateRequest,
    signal?: AbortSignal,
  ): Promise<ValidationReport> {
    const parsedRequest = validateRequestSchema.safeParse(request);
    if (!parsedRequest.success) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      `${this.transactionPath(transactionId)}/validate`,
      { method: "POST", body: JSON.stringify(parsedRequest.data), signal },
      validationReportSchema,
    );
  }

  /** 返回事务已有校验报告；报告不包含正文，检查器按需加载后即可安全呈现。 */
  getValidationReports(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<ValidationReport[]> {
    return this.request(
      `${this.transactionPath(transactionId)}/validation`,
      { method: "GET", signal },
      z.array(validationReportSchema).max(16),
    );
  }

  /** 提交编辑后的请求并立即返回新事务标识；网络执行在后端后台继续，结果由事务流更新。 */
  composeRequest(
    request: ComposeRequest,
    signal?: AbortSignal,
  ): Promise<ComposeResult> {
    const parsedRequest = composeRequestSchema.safeParse(request);
    if (!parsedRequest.success) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      "/api/v1/compose",
      { method: "POST", body: JSON.stringify(parsedRequest.data), signal },
      composeResultSchema,
    );
  }

  /** 从只读事务派生重复请求；覆盖项先在客户端校验，路径和正文中的事务标识必须由服务端再次核对。 */
  repeatTransaction(
    transactionId: string,
    overrides?: ComposeRequestOverrides,
    signal?: AbortSignal,
  ): Promise<ComposeResult> {
    const parsedOverrides = composeRequestOverridesSchema.safeParse(
      overrides ?? {},
    );
    if (!parsedOverrides.success || transactionId.trim().length === 0) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      `${this.transactionPath(transactionId)}/repeat`,
      {
        method: "POST",
        body: JSON.stringify({
          transactionId,
          ...(overrides === undefined
            ? {}
            : { overrides: parsedOverrides.data }),
        }),
        signal,
      },
      composeResultSchema,
    );
  }

  /** 创建经显式确认的高级重复作业；并发、次数和间隔均由共享协议边界约束。 */
  startAdvancedRepeat(
    request: AdvancedRepeatStartRequest,
    signal?: AbortSignal,
  ): Promise<AdvancedRepeatJob> {
    const parsedRequest = advancedRepeatStartRequestSchema.safeParse(request);
    if (!parsedRequest.success) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.request(
      "/api/v1/loadTests",
      { method: "POST", body: JSON.stringify(parsedRequest.data), signal },
      advancedRepeatJobSchema,
    );
  }

  /** 读取保留的高级重复作业，列表顺序由后端固定为最近启动优先。 */
  listAdvancedRepeats(signal?: AbortSignal): Promise<AdvancedRepeatJob[]> {
    return this.request(
      "/api/v1/loadTests",
      { method: "GET", signal },
      z.array(advancedRepeatJobSchema).max(64),
    );
  }

  /** 读取单个高级重复作业的实时统计；未知标识保持后端 404 错误语义。 */
  getAdvancedRepeat(
    jobId: string,
    signal?: AbortSignal,
  ): Promise<AdvancedRepeatJob> {
    return this.request(
      `/api/v1/loadTests/${encodeURIComponent(jobId)}`,
      { method: "GET", signal },
      advancedRepeatJobSchema,
    );
  }

  /** 取消高级重复；取消为协作式，返回快照后不会再分派新的迭代。 */
  cancelAdvancedRepeat(
    jobId: string,
    signal?: AbortSignal,
  ): Promise<AdvancedRepeatJob> {
    return this.request(
      `/api/v1/loadTests/${encodeURIComponent(jobId)}/cancel`,
      { method: "POST", signal },
      advancedRepeatJobSchema,
    );
  }

  /** 提交屏蔽列表完整配置，服务端返回统一工具公开状态供调用方刷新。 */
  updateBlockList(
    update: BlockListConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "blockList",
      update,
      blockListConfigurationSchema,
      signal,
    );
  }

  /** 提交无缓存工具完整配置，规则变更立即作用于后续 HTTP 请求。 */
  updateNoCaching(
    update: NoCachingConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "noCaching",
      update,
      noCachingConfigurationSchema,
      signal,
    );
  }

  /** 提交封包滤镜完整配置；服务端先持久化，再原子替换 TCP/UDP 最终写线规则。 */
  updatePacketFilters(
    update: PacketFilterConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "packetFilters",
      update,
      packetFilterConfigurationSchema,
      signal,
    );
  }

  /** 提交 Cookie 剥离配置，服务端统一保证请求和响应方向的顺序。 */
  updateBlockCookies(
    update: BlockCookiesConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "blockCookies",
      update,
      blockCookiesConfigurationSchema,
      signal,
    );
  }

  /** 提交本地映射规则；本机路径仅随请求提交，不写入浏览器持久化存储。 */
  updateMapLocal(
    update: MapLocalConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "mapLocal",
      update,
      mapLocalConfigurationSchema,
      signal,
    );
  }

  /** 将 Chrome 选择的文件集流式上传到后端受管映射根；FormData 保留目录内的相对路径和空文件。 */
  importMapLocalFiles(
    selection: MapLocalImportSelection,
    signal?: AbortSignal,
  ): Promise<MapLocalImportResult> {
    const validFileCount = selection.directory
      ? selection.files.length >= 1 && selection.files.length <= 2_000
      : selection.files.length === 1;
    const validFiles = selection.files.every(
      ({ file, relativePath }) =>
        relativePath.length >= 1 &&
        relativePath.length <= 4_096 &&
        file.size <= 64 * 1024 * 1024,
    );
    if (!validFileCount || !validFiles) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    const body = new FormData();
    body.append("directory", String(selection.directory));
    for (const { file, relativePath } of selection.files) {
      body.append("path", relativePath);
      body.append("file", file, file.name);
    }
    return this.request(
      "/api/v1/tools/mapLocal/import",
      { method: "POST", body, signal },
      mapLocalImportResultSchema,
    );
  }

  /** 提交远程映射规则，规则顺序由后端作为稳定匹配优先级保存。 */
  updateMapRemote(
    update: MapRemoteConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "mapRemote",
      update,
      mapRemoteConfigurationSchema,
      signal,
    );
  }

  /** 提交 Rewrite 集合；客户端先校验形状，正则有效性由后端统一裁决。 */
  updateRewrite(
    update: RewriteConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "rewrite",
      update,
      rewriteConfigurationSchema,
      signal,
    );
  }

  /** 提交断点规则和队列边界，正在挂起的事务保持当前执行时配置。 */
  updateBreakpoints(
    update: BreakpointsConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "breakpoints",
      update,
      breakpointsConfigurationSchema,
      signal,
    );
  }

  /** 提交节流开关、预设选择和自定义速率；控制 API 本身不受该工具影响。 */
  updateThrottling(
    update: ThrottlingConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "throttling",
      update,
      throttlingConfigurationSchema,
      signal,
    );
  }

  /** 提交镜像目录、报文方向与队列策略；控制面更新完成后返回完整工具快照。 */
  updateMirror(
    update: MirrorConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool("mirror", update, mirrorConfigurationSchema, signal);
  }

  /** 提交自动保存触发器和归档策略；写入器立即读取新配置而不要求代理重启。 */
  updateAutoSave(
    update: AutoSaveConfiguration,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    return this.updateTool(
      "autoSave",
      update,
      autoSaveConfigurationSchema,
      signal,
    );
  }

  /** 立即保存当前录制会话；响应包含最近归档结果，失败由统一错误协议返回。 */
  saveAutoSaveNow(signal?: AbortSignal): Promise<AutoSavePublicState> {
    return this.request(
      "/api/v1/tools/autoSave/saveNow",
      { method: "POST", signal },
      autoSavePublicStateSchema,
    );
  }

  /** 返回反向代理规则和实际绑定端点，端口状态始终以服务端快照为准。 */
  getReverseProxies(
    signal?: AbortSignal,
  ): Promise<AuxiliaryListenerPublicState> {
    return this.request(
      "/api/v1/listeners/reverseProxies",
      { method: "GET", signal },
      auxiliaryListenerPublicStateSchema,
    );
  }

  /** 整体替换反向代理规则；服务运行时后端会按生命周期重启并断开旧连接。 */
  updateReverseProxies(
    update: ReverseProxyEntry[],
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot> {
    return this.updateListenerEntries(
      "/api/v1/listeners/reverseProxies",
      update,
      reverseProxyEntrySchema,
      signal,
    );
  }

  /** 返回 TCP 端口转发规则和实际绑定端点，避免客户端从草稿端口推断运行状态。 */
  getPortForwards(signal?: AbortSignal): Promise<AuxiliaryListenerPublicState> {
    return this.request(
      "/api/v1/listeners/portForwards",
      { method: "GET", signal },
      auxiliaryListenerPublicStateSchema,
    );
  }

  /** 整体替换 TCP 端口转发规则；每条连接按字节双向转发且不写入 HTTP 事务。 */
  updatePortForwards(
    update: PortForwardEntry[],
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot> {
    return this.updateListenerEntries(
      "/api/v1/listeners/portForwards",
      update,
      portForwardEntrySchema,
      signal,
    );
  }

  /** 获取当前断点挂起队列；此端点是唯一允许返回编辑草稿正文的控制资源。 */
  listSuspendedBreakpoints(
    signal?: AbortSignal,
  ): Promise<SuspendedBreakpoint[]> {
    return this.request(
      "/api/v1/breakpoints/suspended",
      { method: "GET", signal },
      z.array(suspendedBreakpointSchema).max(1_024),
    );
  }

  /** 将编辑草稿写回指定挂起事务并继续流水线，成功响应固定为无正文。 */
  continueBreakpoint(
    transactionId: string,
    draft: EditableHttpMessage,
    signal?: AbortSignal,
  ): Promise<void> {
    return this.requestEmpty(`${this.breakpointPath(transactionId)}/continue`, {
      method: "POST",
      body: JSON.stringify(draft),
      signal,
    });
  }

  /** 中止指定挂起事务并释放其队列槽位，成功响应固定为无正文。 */
  abortBreakpoint(transactionId: string, signal?: AbortSignal): Promise<void> {
    return this.requestEmpty(`${this.breakpointPath(transactionId)}/abort`, {
      method: "POST",
      signal,
    });
  }

  /** 导出当前或选定事务的 HAR；二进制上限阻止异常会话把浏览器内存耗尽。 */
  exportRecording(request: ExportRequest, signal?: AbortSignal): Promise<Blob> {
    const parsedRequest = exportRequestSchema.safeParse(request);
    if (!parsedRequest.success) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return this.requestBoundedBinary(
      "/api/v1/recording/export",
      { method: "POST", body: JSON.stringify(parsedRequest.data), signal },
      "application/json, application/har+json",
      maximumExportBytes,
    );
  }

  /**
   * 构造事务分页查询；显式 offset 与集合令牌成对出现，offset=0 用于同一稳定集合的首段读取。
   */
  private buildTransactionQuery(page: TransactionPageRequest): string {
    const { offset, limit, collectionToken } = page;
    const validOffset =
      offset === undefined || (Number.isSafeInteger(offset) && offset >= 0);
    const validLimit =
      limit === undefined ||
      (Number.isSafeInteger(limit) &&
        limit >= 1 &&
        limit <= maximumTransactionPageSize);
    const hasCollectionToken = collectionToken !== undefined;
    const validCollectionToken =
      !hasCollectionToken ||
      (typeof collectionToken === "string" &&
        collectionToken.length > 0 &&
        collectionToken.length <= maximumTransactionCollectionTokenCharacters);
    if (
      !validOffset ||
      !validLimit ||
      !validCollectionToken ||
      (offset === undefined && hasCollectionToken) ||
      (offset !== undefined && offset > 0 && !hasCollectionToken) ||
      (offset === 0 && collectionToken === "")
    ) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }

    const fields: string[] = [];
    if (offset !== undefined) {
      fields.push(`offset=${offset}`);
    }
    if (limit !== undefined) {
      fields.push(`limit=${limit}`);
    }
    if (collectionToken !== undefined) {
      fields.push(`collectionToken=${encodeURIComponent(collectionToken)}`);
    }
    return fields.length === 0 ? "" : `?${fields.join("&")}`;
  }

  /**
   * 编码后端生成的事务标识；空值或非法 Unicode 不会形成歧义路径，而是作为客户端协议错误失败。
   */
  private transactionPath(transactionId: string): string {
    if (typeof transactionId !== "string" || transactionId.length === 0) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    try {
      return `/api/v1/transactions/${encodeURIComponent(transactionId)}`;
    } catch (error) {
      throw new ControlClientError(
        i18n.t("error.web.invalidProtocol"),
        null,
        error,
      );
    }
  }

  /** 构造断点队列项目路径；沿用事务标识校验，避免自由路径段穿透控制路由。 */
  private breakpointPath(transactionId: string): string {
    const transactionPath = this.transactionPath(transactionId);
    const encodedTransactionId = transactionPath.slice(
      "/api/v1/transactions/".length,
    );
    return `/api/v1/breakpoints/suspended/${encodedTransactionId}`;
  }

  /** 校验并整体提交一种辅助监听规则；数组上限与后端一致，避免重启服务后才发现无效条目。 */
  private updateListenerEntries<Entry>(
    path: string,
    update: Entry[],
    schema: ZodType<Entry>,
    signal?: AbortSignal,
  ): Promise<ServiceSnapshot> {
    const parsedUpdate = z.array(schema).max(128).safeParse(update);
    if (!parsedUpdate.success) {
      throw new ControlClientError(i18n.t("error.web.configurationProtocol"));
    }
    return this.request(
      path,
      { method: "PUT", body: JSON.stringify(parsedUpdate.data), signal },
      serviceSnapshotSchema,
    );
  }

  /** 校验任一工具配置并提交到统一资源路由，返回完整工具状态而不是局部猜测。 */
  private updateTool<Configuration>(
    toolId: string,
    update: Configuration,
    schema: ZodType<Configuration>,
    signal?: AbortSignal,
  ): Promise<ToolsPublicState> {
    const parsedUpdate = schema.safeParse(update);
    if (!parsedUpdate.success) {
      throw new ControlClientError(i18n.t("error.web.toolsProtocol"));
    }
    return this.request(
      `/api/v1/tools/${toolId}`,
      {
        method: "PUT",
        body: JSON.stringify(parsedUpdate.data),
        signal,
      },
      toolsPublicStateSchema,
    );
  }

  /** 发起成功即无正文的控制动作；状态码错误仍使用统一结构化错误解码。 */
  private async requestEmpty(path: string, init: RequestInit): Promise<void> {
    const response = await this.fetchResponse(path, init, "application/json");
    await this.ensureResponseOk(response);
    if (response.status !== 204) {
      throw new ControlClientError(
        i18n.t("error.web.invalidProtocol"),
        response.status,
      );
    }
  }

  /**
   * 读取指定方向正文并核对响应归属；方向或事务标识漂移时拒绝把正文绑定到当前选择项。
   */
  private async getBody(
    transactionId: string,
    side: "request" | "response",
    signal?: AbortSignal,
  ): Promise<EncodedBodyResponse> {
    const body = await this.request(
      `${this.transactionPath(transactionId)}/${side}/body`,
      { method: "GET", signal },
      encodedBodyResponseSchema,
    );
    if (body.meta.transactionId !== transactionId || body.meta.side !== side) {
      throw new ControlClientError(i18n.t("error.web.invalidProtocol"));
    }
    return body;
  }

  /**
   * 读取媒体预览元数据并校验范围契约；HEAD 不消费正文，随后由媒体元素直接访问同一
   * 惰性二进制 URL，从而避免 fetch.blob 在首帧前聚合完整音视频。缺少起始分段时后端
   * 返回 `incomplete`，该状态不作为网络错误处理。
   */
  private async getMediaPreviewDescriptor(
    transactionId: string,
    signal?: AbortSignal,
  ): Promise<MediaPreviewBody> {
    const path = `${this.transactionPath(transactionId)}/response/media-preview`;
    const response = await this.fetchResponse(
      path,
      { method: "HEAD", signal },
      "audio/*, video/*, application/octet-stream",
    );
    await this.ensureResponseOk(response);
    const status = response.headers.get("x-media-preview-status");
    const capturedBytes = this.parseUnsignedResponseHeader(
      response,
      "x-media-preview-captured-bytes",
    );
    const totalBytes = this.parseUnsignedResponseHeader(
      response,
      "x-media-preview-total-bytes",
    );
    const segmentCount = this.parseUnsignedResponseHeader(
      response,
      "x-media-preview-segment-count",
    );
    const mimeType = response.headers
      .get("content-type")
      ?.split(";", 1)[0]
      .trim();
    if (
      (status !== "complete" &&
        status !== "continuousPrefix" &&
        status !== "incomplete") ||
      capturedBytes > totalBytes ||
      (status === "complete" && capturedBytes !== totalBytes) ||
      (status === "incomplete" && (capturedBytes !== 0 || segmentCount !== 0))
    ) {
      throw new ControlClientError(
        i18n.t("error.web.invalidProtocol"),
        response.status,
      );
    }
    if (status === "incomplete") {
      return {
        status,
        streamUrl: null,
        mimeType: mimeType ?? "application/octet-stream",
        capturedBytes,
        totalBytes,
        segmentCount,
      };
    }
    if (mimeType === undefined || mimeType.length === 0 || segmentCount === 0) {
      throw new ControlClientError(
        i18n.t("error.web.invalidProtocol"),
        response.status,
      );
    }
    return {
      status,
      streamUrl: `${this.baseUrl}${path}`,
      mimeType,
      capturedBytes,
      totalBytes,
      segmentCount,
    };
  }

  /** 读取严格非负安全整数响应头；缺失、空白、指数或小数形式均视为协议损坏。 */
  private parseUnsignedResponseHeader(
    response: Response,
    name: string,
  ): number {
    const value = response.headers.get(name);
    if (value === null || !/^(?:0|[1-9][0-9]*)$/.test(value)) {
      throw new ControlClientError(
        i18n.t("error.web.invalidProtocol"),
        response.status,
      );
    }
    const parsed = Number(value);
    if (!Number.isSafeInteger(parsed)) {
      throw new ControlClientError(
        i18n.t("error.web.invalidProtocol"),
        response.status,
      );
    }
    return parsed;
  }

  /**
   * 执行 JSON 请求并严格校验响应；网络、HTTP 和结构错误保留各自失败语义。
   */
  private async request<Result>(
    path: string,
    init: RequestInit,
    schema: ZodType<Result, ZodTypeDef, unknown>,
  ): Promise<Result> {
    const response = await this.fetchResponse(path, init, "application/json");
    await this.ensureResponseOk(response);

    let payload: unknown;
    try {
      payload = await response.json();
    } catch (error) {
      if (this.isCancellation(error, init.signal)) {
        throw error;
      }
      throw new ControlClientError(
        i18n.t("error.web.invalidJson"),
        response.status,
        error,
      );
    }

    const parsedResult = schema.safeParse(payload);
    if (!parsedResult.success) {
      const invalidPath = parsedResult.error.issues[0]?.path.join(".");
      throw new ControlClientError(
        invalidPath
          ? i18n.t("error.web.invalidField", { path: invalidPath })
          : i18n.t("error.web.invalidProtocol"),
        response.status,
      );
    }
    return parsedResult.data;
  }

  /**
   * 执行有界二进制控制请求；正文只创建一个 Blob，不复制为 base64。
   */
  private async requestBoundedBinary(
    path: string,
    init: RequestInit,
    accept: string,
    maximumBytes: number,
  ): Promise<Blob> {
    const response = await this.fetchResponse(path, init, accept);
    await this.ensureResponseOk(response);
    const declaredLength = Number(response.headers.get("content-length"));
    if (Number.isFinite(declaredLength) && declaredLength > maximumBytes) {
      throw new ControlClientError(
        i18n.t("error.web.invalidProtocol"),
        response.status,
      );
    }
    const binary = await response.blob();
    if (binary.size > maximumBytes) {
      throw new ControlClientError(
        i18n.t("error.web.invalidProtocol"),
        response.status,
      );
    }
    return binary;
  }

  /**
   * 发送控制请求并统一附加同源管理 Cookie、协商语言与 Accept；JSON 和二进制使用显式正文头，FormData 交给浏览器生成 boundary。
   */
  private async fetchResponse(
    path: string,
    init: RequestInit,
    accept: string,
  ): Promise<Response> {
    try {
      return await this.requestFetch(`${this.baseUrl}${path}`, {
        ...init,
        credentials: init.credentials ?? "same-origin",
        headers: {
          Accept: accept,
          "Accept-Language": currentRequestLocale(),
          ...(init.body && !(init.body instanceof FormData)
            ? { "Content-Type": "application/json" }
            : {}),
          ...init.headers,
        },
      });
    } catch (error) {
      if (this.isCancellation(error, init.signal)) {
        throw error;
      }
      throw new ControlClientError(i18n.t("error.web.network"), null, error);
    }
  }

  /**
   * 把非成功响应解析为后端本地化错误；非 JSON 响应只展示受控 HTTP 状态文案。
   */
  private async ensureResponseOk(response: Response): Promise<void> {
    if (response.ok) {
      return;
    }
    const responseText = await response.text();
    let payload: unknown;
    try {
      payload = JSON.parse(responseText);
    } catch {
      payload = null;
    }
    const parsedError = controlErrorSchema.safeParse(payload);
    const detail = parsedError.success
      ? parsedError.data.message
      : i18n.t("error.web.http", { status: response.status });
    throw new ControlClientError(detail, response.status);
  }

  /**
   * 区分主动取消与网络失败；取消必须保留 AbortError，供懒加载调用方静默丢弃过期请求。
   */
  private isCancellation(
    error: unknown,
    signal: AbortSignal | null | undefined,
  ): boolean {
    return (
      signal?.aborted === true ||
      (error instanceof DOMException && error.name === "AbortError")
    );
  }
}
