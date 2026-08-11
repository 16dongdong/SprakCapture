import {
  Activity,
  ArrowDown,
  ArrowUp,
  Binary,
  File,
  FileArchive,
  FileCode2,
  FileImage,
  FileJson2,
  FileText,
  FileType2,
  Folder,
  Globe2,
  LockKeyhole,
  Music,
  Network,
  Package,
  Search,
  Video,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  TransactionDetail,
  TransactionPage,
  TransactionSummary,
} from "../api/protocol";
import { currentRequestLocale } from "../i18n";
import { useServiceStore } from "../state/serviceStore";
import {
  formatTransactionBytes,
  formatTransactionDuration,
  formatStreamDuration,
  presentTransactionProtocol,
  presentTransactionStatus,
  totalTransactionBytes,
  transactionStatusTone,
} from "./transactionPresentation";
import {
  transactionDetailRevision,
  useLiveTransactionDetail,
} from "./useLiveTransactionDetail";
import type { StreamPacketSelection } from "./streamPacketSelection";
import { TransactionContextMenu } from "./transactionContextMenu";
import type { TransactionToolSeed } from "./transactionToolSeed";
import type { ToolDialogId } from "./toolSettingsDialog";
import { TreeToggle } from "./treeToggle";
import { useCompleteTransactionCollection } from "./transactionCollection";

type TransactionFilter =
  "all" | "pending" | "complete" | "failed" | "blocked" | "cancelled";

interface TransactionNavigatorProps {
  transactionPage: TransactionPage;
  selectedTransactionId: string | null;
  selectedPacket?: StreamPacketSelection | null;
  selectedHost: string | null;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  onSelectPacket?(selection: StreamPacketSelection): void;
  onOpenSslSettings?(
    seed: TransactionToolSeed,
    focusClientCertificate?: boolean,
  ): void;
  onOpenToolSettings?(tool: ToolDialogId, seed: TransactionToolSeed): void;
}

interface NavigatorContextActions {
  focusActive: boolean;
  onFocusHost(host: string): void;
  onClearFocus(): void;
  onOpenSslSettings?(
    seed: TransactionToolSeed,
    focusClientCertificate?: boolean,
  ): void;
  onOpenToolSettings?(tool: ToolDialogId, seed: TransactionToolSeed): void;
  onCommandError(message: string): void;
}

interface PathFolder {
  key: string;
  label: string;
  path: string;
  folders: PathFolder[];
  transactions: TransactionSummary[];
  descendantTransactions: TransactionSummary[];
}

interface PathFolderBuilder {
  key: string;
  label: string;
  path: string;
  folders: Map<string, PathFolderBuilder>;
  transactions: TransactionSummary[];
}

interface OriginGroup {
  key: string;
  origin: string;
  transactions: TransactionSummary[];
  root: PathFolder;
}

const trafficHighlightDurationMilliseconds = 1_000;

/**
 * 判断事务是否匹配当前状态筛选；严格枚举每种状态，避免阻止状态被错误归入网络失败。
 */
function matchesStatus(
  transaction: TransactionSummary,
  statusFilter: TransactionFilter,
): boolean {
  return statusFilter === "all" || transaction.status === statusFilter;
}

/**
 * 生成主机展示名；IPv6 地址在含端口的上下文中必须加方括号，保证 URL 与端点文本可直接识别。
 */
function hostLabel(transaction: TransactionSummary): string {
  return transaction.host.includes(":")
    ? `[${transaction.host}]`
    : transaction.host;
}

/**
 * 生成序列视图的端点展示名；始终保留非零端口，避免不同监听目标在高密度视图中被误认为同一端点。
 */
function endpointLabel(transaction: TransactionSummary): string {
  const host = hostLabel(transaction);
  return transaction.port === 0 ? host : `${host}:${transaction.port}`;
}

/**
 * 根据事务的真实 URL 构造树根来源；默认 HTTP/HTTPS/WS/WSS 端口按 URL 约定省略，其他端口保持可见。
 */
function originLabel(transaction: TransactionSummary): string {
  const schemeMatch = /^([a-z][a-z\d+.-]*):\/\//i.exec(transaction.urlDisplay);
  if (schemeMatch === null) {
    return endpointLabel(transaction);
  }
  const scheme = schemeMatch[1].toLocaleLowerCase();
  const defaultPort =
    (scheme === "http" || scheme === "ws") && transaction.port === 80
      ? true
      : (scheme === "https" || scheme === "wss") && transaction.port === 443;
  return `${scheme}://${
    defaultPort ? hostLabel(transaction) : endpointLabel(transaction)
  }`;
}

/**
 * 生成来源分组键；协议、主机和端口共同确定树根，避免同主机上的 HTTP、HTTPS 与 SOCKS 会话被混入同一分支。
 */
function endpointGroupKey(transaction: TransactionSummary): string {
  return `${transaction.protocol}\u0000${transaction.host}\u0000${transaction.port}`;
}

/**
 * 创建路径树构建节点；构建期使用 Map 实现 O(1) 的同级目录合并，渲染前再冻结为有序数组。
 *
 * 运行上下文：仅在接收录制快照时调用，节点在构建完成前不进入 React 状态，避免高频更新期间产生半成品树。
 * 参数：label 是目录显示名；key 是来源内稳定且唯一的路径键。
 * 失败语义：不解析外部输入，始终返回可追加的空节点。
 */
function createPathFolderBuilder(
  label: string,
  key: string,
  path = "",
): PathFolderBuilder {
  return {
    key,
    label,
    path,
    folders: new Map(),
    transactions: [],
  };
}

/**
 * 拆分事务 URL 的路径段；查询参数保留在叶节点名称中，避免同一路径的不同请求相互覆盖。
 *
 * 运行上下文：来源分组与叶节点显示共用同一拆分规则，防止树层级与资源名出现分歧。
 * 参数：transaction 是服务端提供的事务摘要。
 * 失败语义：空路径返回空数组，调用方据此把事务挂到来源根目录。
 */
function transactionPathSegments(transaction: TransactionSummary): string[] {
  return transaction.path.split("/").filter(Boolean);
}

/**
 * 将单个事务挂入来源下的目录树；只维护目录和叶节点，展开状态完全由用户控制。
 *
 * 运行上下文：在单次快照聚合期间顺序执行，root 仅属于当前来源，不跨来源共享。
 * 参数：root 为来源的可变目录树；transaction 为待挂载的事务摘要。
 * 失败语义：路径为空时直接作为根级资源保存，不伪造目录节点。
 */
function appendPathTransaction(
  root: PathFolderBuilder,
  transaction: TransactionSummary,
): void {
  const segments = transactionPathSegments(transaction);
  let currentFolder = root;
  for (const segment of segments.slice(0, -1)) {
    const nextFolder =
      currentFolder.folders.get(segment) ??
      createPathFolderBuilder(
        segment,
        `${currentFolder.key}/${segment}`,
        `${currentFolder.path}/${segment}`,
      );
    currentFolder.folders.set(segment, nextFolder);
    currentFolder = nextFolder;
  }
  currentFolder.transactions.push(transaction);
}

/**
 * 将可变构建节点转换为稳定的渲染快照；目录按当前界面语言排序，叶节点严格保持录制顺序。
 *
 * 运行上下文：所有事务追加完成后执行一次，React 只消费这个不可变快照。
 * 参数：builder 是待冻结目录；requestLocale 用于目录名称的本地化排序。
 * 失败语义：没有子目录或资源时仍返回空集合，交给上层决定是否渲染该来源。
 */
function finalizePathFolder(
  builder: PathFolderBuilder,
  requestLocale: string,
): PathFolder {
  const folders = Array.from(builder.folders.values())
    .sort((left, right) => left.label.localeCompare(right.label, requestLocale))
    .map((folder) => finalizePathFolder(folder, requestLocale));
  const transactions = [...builder.transactions].sort(
    (left, right) => left.sequence - right.sequence,
  );
  return {
    key: builder.key,
    label: builder.label,
    path: builder.path,
    folders,
    transactions,
    descendantTransactions: [
      ...transactions,
      ...folders.flatMap((folder) => folder.descendantTransactions),
    ],
  };
}

/**
 * 生成人类可读的资源叶节点名称；根路径保持为 /，查询参数追加到资源名以区分同一路径请求。
 *
 * 运行上下文：结构树需要压缩列宽，完整 URL 继续保留给叶节点提示与右侧检查器。
 * 参数：transaction 是当前资源的事务摘要。
 * 失败语义：路径为空时返回 /，不会显示空白叶节点。
 */
function transactionPathLeafLabel(transaction: TransactionSummary): string {
  const segments = transactionPathSegments(transaction);
  const resourceName = segments.at(-1) ?? "/";
  return transaction.query
    ? `${resourceName}?${transaction.query}`
    : resourceName;
}

/**
 * 按 URL 来源和路径聚合事务；来源是一级节点，目录段是中间节点，资源请求始终作为可选择叶节点。
 *
 * 运行上下文：结构视图在筛选结果变化时重建树；原始流只复用来源分组和排序，子节点由独立流树渲染。
 * 参数：transactions 是已筛选的事务；emptyValue 是 URL 缺失时的来源显示文本。
 * 失败语义：空集合返回空来源列表，不构造占位根节点。
 */
function groupTransactions(
  transactions: TransactionSummary[],
  emptyValue: string,
): OriginGroup[] {
  const groupedOrigins = new Map<
    string,
    {
      key: string;
      origin: string;
      transactions: TransactionSummary[];
      root: PathFolderBuilder;
    }
  >();
  for (const transaction of transactions) {
    const key = endpointGroupKey(transaction);
    const originGroup = groupedOrigins.get(key) ?? {
      key,
      origin: originLabel(transaction) || emptyValue,
      transactions: [],
      root: createPathFolderBuilder("", key),
    };
    originGroup.transactions.push(transaction);
    appendPathTransaction(originGroup.root, transaction);
    groupedOrigins.set(key, originGroup);
  }
  const requestLocale = currentRequestLocale();
  return Array.from(groupedOrigins.values())
    .map((originGroup) => ({
      key: originGroup.key,
      origin: originGroup.origin,
      transactions: [...originGroup.transactions].sort(
        (leftTransaction, rightTransaction) =>
          leftTransaction.sequence - rightTransaction.sequence,
      ),
      root: finalizePathFolder(originGroup.root, requestLocale),
    }))
    .sort((leftOrigin, rightOrigin) =>
      leftOrigin.origin.localeCompare(rightOrigin.origin, requestLocale),
    );
}

/**
 * 按端点聚合原始流，同时保留传输协议和显式端口作为来源标签。
 *
 * 运行上下文：普通 HTTP 树可省略默认端口，原始 TCP/TLS 流必须展示完整连接地址以便定位抓取结果。
 * 参数：transactions 只包含 SOCKS 或透明隧道事务；emptyValue 用于缺少地址的异常摘要。
 * 失败语义：空输入返回空集合；缺少 urlDisplay 时退回普通来源标签。
 */
function groupStreamTransactions(
  transactions: TransactionSummary[],
  emptyValue: string,
): OriginGroup[] {
  const groupedStreams = new Map<string, TransactionSummary[]>();
  for (const transaction of transactions) {
    const streamAddress = streamDisplayAddress(transaction, emptyValue);
    const groupedTransactions = groupedStreams.get(streamAddress) ?? [];
    groupedTransactions.push(transaction);
    groupedStreams.set(streamAddress, groupedTransactions);
  }
  const requestLocale = currentRequestLocale();
  return Array.from(groupedStreams.entries())
    .flatMap(([streamAddress, groupedTransactions]) =>
      groupTransactions(groupedTransactions, emptyValue).map((originGroup) => ({
        ...originGroup,
        key: `${originGroup.key}\u0000${streamAddress}`,
        origin: streamAddress,
      })),
    )
    .sort((leftOrigin, rightOrigin) =>
      leftOrigin.origin.localeCompare(rightOrigin.origin, requestLocale),
    );
}

/**
 * 返回原始流在导航树中的应用协议地址；旧录制里的 tls:// 是传输容器名，统一迁移为用户可识别的 https://。
 *
 * 运行上下文：新后端已经把 ClientHello 连接投影为 HTTPS，兼容转换只服务于升级前仍在内存中的历史摘要。
 * 参数：transaction 提供原始 urlDisplay，emptyValue 用于旧数据缺失地址时的稳定占位。
 * 失败语义：纯文本规范化没有失败分支；TCP、UDP、HTTP、HTTPS、WS 与 WSS 地址保持原样。
 */
function streamDisplayAddress(
  transaction: TransactionSummary,
  emptyValue: string,
): string {
  const address = transaction.urlDisplay || emptyValue;
  return address.startsWith("tls://")
    ? `https://${address.slice("tls://".length)}`
    : address;
}

/**
 * 根据 MIME、协议和扩展名生成资源类型标签键；类型不占用会话行列，而是写入辅助说明与详情区域。
 */
type TransactionResourceKind =
  | "image"
  | "json"
  | "apk"
  | "html"
  | "script"
  | "stylesheet"
  | "archive"
  | "binary"
  | "font"
  | "audio"
  | "video"
  | "stream"
  | "text"
  | "other";

/**
 * 根据 MIME 与扩展名识别资源图标；协议元数据优先于文件名，缺失时再使用扩展名。
 *
 * 运行上下文：结构树叶节点与辅助标签共用此分类，确保图标和提示不会表达不同类型。
 * 参数：transaction 为服务端事务摘要。
 * 失败语义：未知或无扩展名内容返回 other，不根据正文猜测类型。
 */
function transactionResourceKind(
  transaction: TransactionSummary,
): TransactionResourceKind {
  const contentType = transaction.contentType.toLocaleLowerCase();
  const normalizedPath = transaction.path.toLocaleLowerCase();
  if (
    contentType.startsWith("image/") ||
    /\.(?:avif|bmp|gif|ico|jpe?g|png|svg|webp)$/.test(normalizedPath)
  ) {
    return "image";
  }
  if (
    contentType === "application/vnd.android.package-archive" ||
    /\.apk$/.test(normalizedPath)
  ) {
    return "apk";
  }
  if (contentType.includes("json") || /\.(?:json|map)$/.test(normalizedPath)) {
    return "json";
  }
  if (contentType.includes("html") || /\.(?:htm|html)$/.test(normalizedPath)) {
    return "html";
  }
  if (
    contentType.includes("javascript") ||
    /\.(?:cjs|js|mjs|ts|tsx)$/.test(normalizedPath)
  ) {
    return "script";
  }
  if (
    contentType === "text/css" ||
    /\.(?:css|less|scss)$/.test(normalizedPath)
  ) {
    return "stylesheet";
  }
  if (
    contentType.startsWith("font/") ||
    /\.(?:eot|otf|ttf|woff2?)$/.test(normalizedPath)
  ) {
    return "font";
  }
  if (
    contentType.startsWith("audio/") ||
    /\.(?:aac|flac|m4a|mp3|ogg|wav)$/.test(normalizedPath)
  ) {
    return "audio";
  }
  if (
    contentType.startsWith("video/") ||
    /\.(?:m4v|mov|mp4|webm)$/.test(normalizedPath)
  ) {
    return "video";
  }
  if (
    /\.(?:7z|gz|jar|rar|tar|tgz|zip)$/.test(normalizedPath) ||
    /^(?:application\/(?:gzip|java-archive|x-7z-compressed|x-rar-compressed|x-tar|zip))/.test(
      contentType,
    )
  ) {
    return "archive";
  }
  if (
    contentType === "application/octet-stream" ||
    /\.(?:bin|dat|dll|dylib|exe|so)$/.test(normalizedPath)
  ) {
    return "binary";
  }
  if (transaction.protocol === "socks") {
    return "stream";
  }
  if (
    contentType.startsWith("text/") ||
    /\.(?:log|md|txt|xml|yaml|yml)$/.test(normalizedPath)
  ) {
    return "text";
  }
  return "other";
}

/**
 * 将同一媒体 URL 的 HTTP Range 事务折叠为一个持续更新的结构树资源节点。
 *
 * 运行上下文：仅用于 Structure 展示；Sequence 仍保留每个原始事务，正文、头字段和包索引也不合并。
 * 参数 transactions 是当前滚动窗口内的筛选结果。只有明确的 206 音频/视频响应参与折叠，最新分片
 * 作为可选择节点，使右侧媒体端点能够基于 URL 与强 ETag 安全聚合已录制范围。
 */
function collapseMediaRangeTransactions(
  transactions: TransactionSummary[],
): TransactionSummary[] {
  const visibleTransactions: TransactionSummary[] = [];
  const mediaPositions = new Map<string, number>();
  for (const transaction of transactions) {
    const resourceKind = transactionResourceKind(transaction);
    if (
      transaction.statusCode !== 206 ||
      (resourceKind !== "audio" && resourceKind !== "video")
    ) {
      visibleTransactions.push(transaction);
      continue;
    }
    const mediaKey = `${transaction.protocol}\u0000${transaction.urlDisplay}\u0000${transaction.contentType.toLocaleLowerCase()}`;
    const existingPosition = mediaPositions.get(mediaKey);
    if (existingPosition === undefined) {
      mediaPositions.set(mediaKey, visibleTransactions.length);
      visibleTransactions.push(transaction);
      continue;
    }
    const existing = visibleTransactions[existingPosition];
    if (transaction.sequence > existing.sequence) {
      visibleTransactions[existingPosition] = transaction;
    }
  }
  return visibleTransactions;
}

/**
 * 将资源分类映射到既有国际化标签，新增的细分类仍复用用户熟悉的资源术语。
 *
 * 运行上下文：事务提示在每次摘要更新时调用；参数为当前事务摘要。
 * 失败语义：未知类型稳定映射到 other，不返回不存在的翻译键。
 */
function resourceLabelKey(transaction: TransactionSummary): string {
  const kind = transactionResourceKind(transaction);
  if (kind === "html" || kind === "text") {
    return "transactions.resource.document";
  }
  if (kind === "audio" || kind === "video") {
    return "transactions.resource.media";
  }
  if (kind === "apk" || kind === "archive" || kind === "binary") {
    return "transactions.resource.binary";
  }
  return `transactions.resource.${kind}`;
}

/**
 * 渲染紧凑资源图标；颜色类与资源种类保持一一对应，便于在高密度树中快速扫描。
 *
 * 运行上下文：仅作为路径叶节点的装饰图标；参数为对应事务摘要。
 * 失败语义：无法识别的资源使用通用文件图标，不影响叶节点选择。
 */
function TransactionResourceIcon({
  transaction,
}: {
  transaction: TransactionSummary;
}) {
  const kind = transactionResourceKind(transaction);
  const ResourceIcon = {
    image: FileImage,
    json: FileJson2,
    apk: Package,
    html: FileCode2,
    script: FileCode2,
    stylesheet: FileType2,
    archive: FileArchive,
    binary: Binary,
    font: FileType2,
    audio: Music,
    video: Video,
    stream: Network,
    text: FileText,
    other: File,
  }[kind];
  return (
    <ResourceIcon
      aria-hidden="true"
      className={`transactionResourceIcon transactionResourceIcon--${kind}`}
      size={13}
    />
  );
}

/**
 * 生成树叶路径；没有请求路径的原始流统一展示为根路径，避免在叶节点重复父级的 SOCKS 来源地址。
 */
function transactionLocation(transaction: TransactionSummary): string {
  const path = transaction.path || "/";
  return transaction.query ? `${path}?${transaction.query}` : path;
}

interface TransactionRowPresentation {
  ariaLabel: string;
  duration: string;
  endpoint: string;
  location: string;
  statusCode: string;
  statusLabel: string;
  title: string;
}

/**
 * 构造会话行的显示与辅助信息；可见区域只保留状态、目标和耗时，方法、资源类型、协议、字节数仍通过提示和无障碍名称完整提供。
 *
 * 运行上下文：结构树和时序列表共用该钩子，确保同一事务在两个视图中的端点和状态描述一致。
 * 参数：transaction 为服务端快照中的事务摘要。
 * 失败语义：摘要字段缺失时使用国际化空值，不推测未采集的状态码或端口。
 */
function useTransactionRowPresentation(
  transaction: TransactionSummary,
): TransactionRowPresentation {
  const { t } = useTranslation();
  const emptyValue = t("transactions.table.emptyValue");
  const resourceLabel = t(resourceLabelKey(transaction));
  const statusLabel = presentTransactionStatus(transaction, t);
  const protocolLabel = presentTransactionProtocol(transaction, t);
  const endpoint = endpointLabel(transaction);
  const location = transactionLocation(transaction);
  const duration = formatTransactionDuration(transaction);
  const statusCode = String(transaction.statusCode ?? emptyValue);
  const title = [
    `${transaction.method} ${transaction.urlDisplay}`,
    `${t("transactions.table.statusCode")}: ${statusCode} (${statusLabel})`,
    `${t("transactions.table.protocol")}: ${protocolLabel}`,
    `${t("transactions.table.size")}: ${formatTransactionBytes(totalTransactionBytes(transaction))}`,
    `${t("transactions.table.path")}: ${location}`,
    `${t("transactions.resource.other")}: ${resourceLabel}`,
  ].join("\n");

  return {
    ariaLabel: `${transaction.method} ${transaction.urlDisplay} ${resourceLabel}`,
    duration,
    endpoint,
    location,
    statusCode,
    statusLabel,
    title,
  };
}

/**
 * 生成用于识别新流量的轻量签名；正文和头字节、状态或终态时间变化都会触发一次高亮。
 * 参数 transaction 是当前摘要；返回字符串只用于本地快照比较，不参与协议或持久化。
 * 失败语义：摘要字段均为基础类型，拼接过程不会抛出异常。
 */
function trafficSignature(transaction: TransactionSummary): string {
  const sizes = transaction.sizes;
  return [
    sizes.requestHeaderBytes,
    sizes.requestBodyBytes,
    sizes.responseHeaderBytes,
    sizes.responseBodyBytes,
    transaction.status,
    transaction.timings.endAtMilliseconds ?? "",
  ].join(":");
}

/**
 * 跟踪当前快照相对上一快照发生流量变化的事务；每个事务独立重置消散计时器。
 * 运行上下文：事务导航器每次滚动刷新后调用；新事务和已有事务字节变化采用同一高亮路径。
 * 参数 transactions 是当前完整摘要集合。组件卸载时清理全部计时器，不保留后台更新。
 */
function useTrafficHighlights(
  transactions: TransactionSummary[],
): ReadonlySet<string> {
  const previousSignaturesRef = useRef<Map<string, string> | null>(null);
  const timersRef = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const [highlightedIds, setHighlightedIds] = useState<Set<string>>(
    () => new Set(),
  );

  useEffect(() => {
    const nextSignatures = new Map(
      transactions.map((transaction) => [
        transaction.transactionId,
        trafficSignature(transaction),
      ]),
    );
    const previousSignatures = previousSignaturesRef.current;
    previousSignaturesRef.current = nextSignatures;
    if (previousSignatures === null) {
      return;
    }

    const changedIds = transactions
      .filter((transaction) => {
        const previousSignature = previousSignatures.get(
          transaction.transactionId,
        );
        // 新连接没有上一帧签名，也必须与已有连接的字节变化一样高亮后消散。
        return (
          previousSignature === undefined ||
          previousSignature !== nextSignatures.get(transaction.transactionId)
        );
      })
      .map((transaction) => transaction.transactionId);
    if (changedIds.length === 0) {
      return;
    }

    setHighlightedIds((currentIds) => {
      const nextIds = new Set(currentIds);
      changedIds.forEach((transactionId) => nextIds.add(transactionId));
      return nextIds;
    });
    changedIds.forEach((transactionId) => {
      const existingTimer = timersRef.current.get(transactionId);
      if (existingTimer !== undefined) {
        clearTimeout(existingTimer);
      }
      const timer = setTimeout(() => {
        timersRef.current.delete(transactionId);
        setHighlightedIds((currentIds) => {
          if (!currentIds.has(transactionId)) {
            return currentIds;
          }
          const nextIds = new Set(currentIds);
          nextIds.delete(transactionId);
          return nextIds;
        });
      }, trafficHighlightDurationMilliseconds);
      timersRef.current.set(transactionId, timer);
    });
  }, [transactions]);

  useEffect(
    () => () => {
      timersRef.current.forEach((timer) => clearTimeout(timer));
      timersRef.current.clear();
    },
    [],
  );

  return highlightedIds;
}

/**
 * 渲染路径树的可选择事务叶节点；状态点紧邻对应事务，来源 URL 不再汇总子事务失败。
 *
 * 运行上下文：仅用于 HTTP/HTTPS 等基于 URL 的结构树，不承担目录展开职责。
 * 参数：label 为压缩后的资源名；depth 决定树形缩进；其余参数描述当前选择和新流量状态。
 * 失败语义：红色只标记实际失败或阻止的叶节点，不能污染同一来源下的成功事务；用户选择只上报事务 ID。
 */
function TransactionTreeItem({
  transaction,
  label,
  depth,
  selected,
  trafficActive,
  onSelectTransaction,
  contextActions,
}: {
  transaction: TransactionSummary;
  label: string;
  depth: number;
  selected: boolean;
  trafficActive: boolean;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  contextActions: NavigatorContextActions;
}) {
  const row = useTransactionRowPresentation(transaction);
  return (
    <TransactionContextMenu
      {...contextActions}
      // 资源叶节点代表一条确定事务，右键规则必须保留完整路径和查询串，不能退化成主机范围。
      seedPath={transaction.path}
      seedQuery={transaction.query || null}
      transaction={transaction}
    >
      <button
        aria-label={row.ariaLabel}
        className={[
          "transactionTreeItem",
          selected ? "isSelected" : "",
          trafficActive ? "isTrafficActive" : "",
        ]
          .filter(Boolean)
          .join(" ")}
        data-traffic-active={trafficActive ? "true" : undefined}
        onClick={() =>
          onSelectTransaction(transaction.transactionId, transaction)
        }
        onContextMenu={() =>
          onSelectTransaction(transaction.transactionId, transaction)
        }
        style={{ paddingInlineStart: 5 + depth * 14 }}
        title={row.title}
        type="button"
      >
        <TransactionResourceIcon transaction={transaction} />
        <i
          aria-hidden="true"
          className={`transactionStatusDot transactionStatusDot--${transactionStatusTone(transaction.status)}`}
        />
        <span className="transactionTreeTarget">{label}</span>
      </button>
    </TransactionContextMenu>
  );
}

/**
 * 渲染 Charles Sequence 的高密度事务行；列顺序固定为状态、主机端口和耗时。
 *
 * 运行上下文：Sequence 是扁平的时间顺序视图，行只消费摘要数据，不读取正文或创建额外请求。
 * 参数：transaction 为要展示的事务，selected 与 trafficActive 驱动本地视觉状态。
 * 失败语义：状态码和内容类型缺失时显示统一空值；右键工具仍使用该行已记录的路径和查询串。
 */
function TransactionSequenceRow({
  transaction,
  selected,
  trafficActive,
  onSelectTransaction,
  contextActions,
}: {
  transaction: TransactionSummary;
  selected: boolean;
  trafficActive: boolean;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  contextActions: NavigatorContextActions;
}) {
  const row = useTransactionRowPresentation(transaction);
  return (
    <TransactionContextMenu
      {...contextActions}
      seedPath={transaction.path}
      seedQuery={transaction.query || null}
      transaction={transaction}
    >
      <div className="transactionSequenceTableRow" role="row">
        <button
          aria-label={row.ariaLabel}
          className={[
            "transactionSequenceItem",
            selected ? "isSelected" : "",
            trafficActive ? "isTrafficActive" : "",
          ]
            .filter(Boolean)
            .join(" ")}
          data-traffic-active={trafficActive ? "true" : undefined}
          title={row.title}
          type="button"
          onClick={() =>
            onSelectTransaction(transaction.transactionId, transaction)
          }
          onContextMenu={() =>
            onSelectTransaction(transaction.transactionId, transaction)
          }
        >
          <span className="sequenceStatusCell" title={row.statusLabel}>
            <i
              aria-hidden="true"
              className={`transactionStatusDot transactionStatusDot--${transactionStatusTone(transaction.status)}`}
            />
            {row.statusCode}
          </span>
          <span className="sequenceEndpointCell" title={row.endpoint}>
            {row.endpoint}
          </span>
          <span className="transactionDuration" title={row.duration}>
            {row.duration}
          </span>
        </button>
      </div>
    </TransactionContextMenu>
  );
}

/**
 * 渲染单个 URL 目录节点；展开状态只响应用户操作，避免新流量到达时树形布局跳动。
 *
 * 运行上下文：目录状态局限在当前 React 节点，筛选结果变化后由键值自动重建。
 * 参数：folder 是稳定目录快照；depth 控制缩进；selectedTransactionId 只标记当前资源叶节点。
 * 失败语义：目录没有子项时保留可控空节点，不会阻塞同级资源的渲染。
 */
function PathFolderTree({
  folder,
  depth,
  selectedTransactionId,
  highlightedIds,
  onSelectTransaction,
  contextActions,
}: {
  folder: PathFolder;
  depth: number;
  selectedTransactionId: string | null;
  highlightedIds: ReadonlySet<string>;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  contextActions: NavigatorContextActions;
}) {
  // Charles 的结构树首次进入时只展示来源，目录必须由用户逐层展开，避免大量路径同时撑满导航区。
  const [expanded, setExpanded] = useState(false);
  const folderTransactions = folder.descendantTransactions;
  // 路径树只冻结含叶节点的目录；该不变量保证右键目标与导出范围始终存在。
  const representativeTransaction = folderTransactions[0]!;

  return (
    <section className="transactionPathFolder">
      <TransactionContextMenu
        {...contextActions}
        seedPath={folder.path}
        transaction={representativeTransaction}
        transactionIds={folderTransactions.map(
          (transaction) => transaction.transactionId,
        )}
      >
        <div
          className="transactionPathFolderHeader"
          style={{ paddingInlineStart: 5 + depth * 14 }}
        >
          <TreeToggle
            expanded={expanded}
            label={folder.label}
            onToggle={() => setExpanded((current) => !current)}
          />
          <Folder
            aria-hidden="true"
            className="transactionTreeFolder"
            size={13}
          />
          <span className="transactionTreeTarget" title={folder.label}>
            {folder.label}
          </span>
        </div>
      </TransactionContextMenu>
      {expanded && (
        <div className="transactionPathFolderChildren">
          {folder.folders.map((childFolder) => (
            <PathFolderTree
              depth={depth + 1}
              folder={childFolder}
              highlightedIds={highlightedIds}
              contextActions={contextActions}
              key={childFolder.key}
              onSelectTransaction={onSelectTransaction}
              selectedTransactionId={selectedTransactionId}
            />
          ))}
          {folder.transactions.map((transaction) => (
            <TransactionTreeItem
              depth={depth + 1}
              key={transaction.transactionId}
              label={transactionPathLeafLabel(transaction)}
              onSelectTransaction={onSelectTransaction}
              selected={selectedTransactionId === transaction.transactionId}
              trafficActive={highlightedIds.has(transaction.transactionId)}
              transaction={transaction}
              contextActions={contextActions}
            />
          ))}
        </div>
      )}
    </section>
  );
}

/**
 * 根据来源协议选择根图标；加密来源显示锁，普通 Web 来源显示地球。
 *
 * 运行上下文：HTTP 类来源根渲染时调用；参数为来源中的代表事务。
 * 失败语义：未知协议使用普通 Web 图标，不改变协议解析结果。
 */
function OriginProtocolIcon({
  transaction,
}: {
  transaction: TransactionSummary;
}) {
  const encrypted =
    transaction.protocol === "https" ||
    transaction.protocol === "wss" ||
    transaction.urlDisplay.toLocaleLowerCase().startsWith("https://");
  const ProtocolIcon = encrypted ? LockKeyhole : Globe2;
  return (
    <ProtocolIcon
      aria-hidden="true"
      className="transactionTreeOrigin"
      size={13}
    />
  );
}

/**
 * 渲染 Charles 风格的来源根节点；根节点只承担展开与来源识别，资源选择始终落在路径叶节点。
 *
 * 运行上下文：每个来源独立维护展开状态，新流量仅触发短暂高亮，不改变用户当前层级。
 * 参数：originGroup 是来源及其路径快照；highlightedIds 控制流量高亮；回调上报资源选择。
 * 失败语义：来源下无资源时不渲染子树，不影响其他来源。
 */
function OriginTree({
  originGroup,
  selectedTransactionId,
  highlightedIds,
  onSelectTransaction,
  contextActions,
}: {
  originGroup: OriginGroup;
  selectedTransactionId: string | null;
  highlightedIds: ReadonlySet<string>;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  contextActions: NavigatorContextActions;
}) {
  const { t } = useTranslation();
  // 来源根默认收起；新流量只高亮根节点，不擅自展开并打断用户当前浏览位置。
  const [expanded, setExpanded] = useState(false);
  const trafficActive = originGroup.transactions.some((transaction) =>
    highlightedIds.has(transaction.transactionId),
  );
  const representativeTransaction = originGroup.transactions[0];

  return (
    <section
      className={`transactionHostGroup${trafficActive ? " isTrafficActive" : ""}`}
      data-traffic-active={trafficActive ? "true" : undefined}
    >
      <TransactionContextMenu
        {...contextActions}
        seedPath=""
        transaction={representativeTransaction}
        transactionIds={originGroup.transactions.map(
          (transaction) => transaction.transactionId,
        )}
      >
        <div
          className="transactionHostHeader"
          title={`${originGroup.origin}\n${originGroup.transactions.length} ${t("transactions.context.transactions")}`}
        >
          <TreeToggle
            expanded={expanded}
            label={originGroup.origin}
            onToggle={() => setExpanded((current) => !current)}
          />
          <OriginProtocolIcon transaction={representativeTransaction} />
          <span className="treeEndpoint">{originGroup.origin}</span>
        </div>
      </TransactionContextMenu>
      {expanded && (
        <div className="transactionHostChildren">
          {originGroup.root.folders.map((folder) => (
            <PathFolderTree
              depth={1}
              folder={folder}
              highlightedIds={highlightedIds}
              contextActions={contextActions}
              key={folder.key}
              onSelectTransaction={onSelectTransaction}
              selectedTransactionId={selectedTransactionId}
            />
          ))}
          {originGroup.root.transactions.map((transaction) => (
            <TransactionTreeItem
              depth={1}
              key={transaction.transactionId}
              label={transactionPathLeafLabel(transaction)}
              onSelectTransaction={onSelectTransaction}
              selected={selectedTransactionId === transaction.transactionId}
              trafficActive={highlightedIds.has(transaction.transactionId)}
              transaction={transaction}
              contextActions={contextActions}
            />
          ))}
        </div>
      )}
    </section>
  );
}

type NavigatorViewMode = "structure" | "sequence";

/**
 * 渲染单个方向的可查看片段；选择时传递精确方向和序号，右侧检查器只读取该片段的字节范围。
 *
 * 运行上下文：请求和响应分别占据连接下的一条方向分支，展开后每个叶节点显示方向与原始字节数。
 * 参数：transaction 与 packets 确定片段来源，side 决定箭头和本地化文案，其余参数维持精确选择及右键菜单。
 * 失败语义：空片段集合只显示计数为零的方向节点；该组件不执行正文 I/O，详情读取失败由连接层处理。
 */
function StreamPacketList({
  transaction,
  packets,
  side,
  selectedPacket,
  onSelectPacket,
  contextActions,
}: {
  transaction: TransactionSummary;
  packets: TransactionDetail["requestPackets"];
  side: "request" | "response";
  selectedPacket: StreamPacketSelection | null;
  onSelectPacket(selection: StreamPacketSelection): void;
  contextActions: NavigatorContextActions;
}) {
  const { t } = useTranslation();
  const DirectionIcon = side === "request" ? ArrowUp : ArrowDown;
  const directionLabel = t(`viewer.tabs.${side}`);
  const [expanded, setExpanded] = useState(false);
  return (
    <section className="streamDirectionGroup">
      <div className="streamDirectionHeader">
        <TreeToggle
          expanded={expanded}
          label={directionLabel}
          onToggle={() => setExpanded((current) => !current)}
        />
        <DirectionIcon aria-hidden="true" size={14} />
        <button
          className={`streamDirectionSelect${
            selectedPacket?.transactionId === transaction.transactionId &&
            selectedPacket.side === side &&
            selectedPacket.sequence === null
              ? " isSelected"
              : ""
          }`}
          onClick={(event) => {
            event.stopPropagation();
            onSelectPacket({
              transactionId: transaction.transactionId,
              side,
              sequence: null,
            });
          }}
          type="button"
        >
          {directionLabel}
        </button>
        <span className="treeCount">{packets.length}</span>
      </div>
      {expanded &&
        packets.map((packet) => {
          const selection = {
            transactionId: transaction.transactionId,
            side,
            sequence: packet.sequence,
          } satisfies StreamPacketSelection;
          const selected =
            selectedPacket?.transactionId === selection.transactionId &&
            selectedPacket.side === selection.side &&
            selectedPacket.sequence === selection.sequence;
          // 旧录制缺少 action 字段时由完整差异反推出替换，升级后无需清空既有事务也能正确显示。
          const effectiveAction =
            packet.action === "forward" && packet.modifications.length > 0
              ? "replace"
              : packet.action;
          const actionLabel =
            effectiveAction === "replace"
              ? "替换"
              : effectiveAction === "drop"
                ? "丢弃"
                : effectiveAction === "close"
                  ? "关闭连接"
                  : "";
          const packetLabel = `${directionLabel}${actionLabel ? ` ${actionLabel}` : ""}`;
          return (
            <TransactionContextMenu
              {...contextActions}
              key={`${side}:${packet.sequence}`}
              transaction={transaction}
            >
              <button
                aria-label={`${packetLabel} ${formatTransactionBytes(packet.originalBytes)}`}
                className={`streamPacketItem${selected ? " isSelected" : ""}`}
                onClick={() => onSelectPacket(selection)}
                onContextMenu={() => onSelectPacket(selection)}
                title={`${packetLabel}：${formatTransactionBytes(packet.originalBytes)}`}
                type="button"
              >
                <span className="streamTreeBranch" aria-hidden="true" />
                <span className={actionLabel ? `streamPacketAction is${effectiveAction}` : undefined}>
                  {packetLabel}
                </span>
                <span>· {formatTransactionBytes(packet.originalBytes)}</span>
              </button>
            </TransactionContextMenu>
          );
        })}
    </section>
  );
}

/**
 * 以一个传输层连接作为树根，客户端地址用于区分同目标并发连接；高频流的正文始终延迟到用户选择叶节点后读取。
 *
 * 运行上下文：来源节点已经显示目标地址，连接节点必须显示真实 `clientAddress`，不能再用 `/` 冒充 URL 路径。
 * 参数：transaction 提供稳定连接标识和客户端端点；connectionIndex 是同目标内从一开始的显示序号；其余参数管理展开与单包选择。
 * 失败语义：详情暂不可用时只隐藏子包列表；来源 URL 不承载状态色，终态统一落在具体客户端连接节点。
 */
function StreamTransactionTree({
  transaction,
  streamAddress,
  connectionIndex,
  trafficActive,
  selected,
  selectedPacket,
  onSelectTransaction,
  onSelectPacket,
  contextActions,
}: {
  transaction: TransactionSummary;
  streamAddress: string;
  connectionIndex: number;
  trafficActive: boolean;
  selected: boolean;
  selectedPacket: StreamPacketSelection | null;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  onSelectPacket(selection: StreamPacketSelection): void;
  contextActions: NavigatorContextActions;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const row = useTransactionRowPresentation(transaction);
  const detailState = useLiveTransactionDetail({
    enabled: expanded,
    revision: transactionDetailRevision(transaction),
    transactionId: transaction.transactionId,
  });
  const connectionLabel = transaction.clientAddress;
  const processLabel =
    transaction.clientProcessName === null
      ? null
      : `${transaction.clientProcessName}${
          transaction.clientProcessId === null
            ? ""
            : ` · PID ${transaction.clientProcessId}`
        }`;
  const connectionTitle = [
    `${t("viewer.overview.groups.connection")} ${connectionIndex}`,
    connectionLabel,
    processLabel,
    row.title,
    formatStreamDuration(transaction),
  ]
    .filter((value): value is string => value !== null)
    .join("\n");

  const detail = detailState.kind === "ready" ? detailState.detail : null;

  /**
   * 选择流方向或报文前固定其事务摘要；历史连接滑出实时尾页后，右侧检查器仍可保持同一事务。
   */
  const selectStreamPacket = (selection: StreamPacketSelection) => {
    onSelectTransaction(transaction.transactionId, transaction);
    onSelectPacket(selection);
  };

  return (
    <section className="streamConnectionTree">
      <div className="streamTransactionHeader">
        <TreeToggle
          expanded={expanded}
          label={`${streamAddress} ${connectionLabel}`}
          onToggle={() => setExpanded((current) => !current)}
        />
        <TransactionContextMenu
          {...contextActions}
          seedPath=""
          transaction={transaction}
        >
          <button
            aria-label={`${streamAddress} ${connectionLabel} ${row.statusLabel}`}
            className={`streamTransactionStatusItem${selected ? " isSelected" : ""}${trafficActive ? " isTrafficActive" : ""}`}
            data-traffic-active={trafficActive ? "true" : undefined}
            onClick={() =>
              onSelectTransaction(transaction.transactionId, transaction)
            }
            onContextMenu={() =>
              onSelectTransaction(transaction.transactionId, transaction)
            }
            title={connectionTitle}
            type="button"
          >
            <span className="streamTreeBranch" aria-hidden="true" />
            <i
              aria-hidden="true"
              className={`transactionStatusDot transactionStatusDot--${transactionStatusTone(transaction.status)}`}
            />
            <span>{connectionLabel}</span>
          </button>
        </TransactionContextMenu>
      </div>
      {expanded && detail !== null && (
        <div className="streamDirectionList">
          <StreamPacketList
            onSelectPacket={selectStreamPacket}
            packets={detail.requestPackets}
            selectedPacket={selectedPacket}
            side="request"
            transaction={transaction}
            contextActions={contextActions}
          />
          <StreamPacketList
            onSelectPacket={selectStreamPacket}
            packets={detail.responsePackets}
            selectedPacket={selectedPacket}
            side="response"
            transaction={transaction}
            contextActions={contextActions}
          />
        </div>
      )}
    </section>
  );
}

/**
 * 按连接地址聚合原始流事务；来源只显示一次，每条连接在其下独立展开请求、响应和片段。
 *
 * 运行上下文：同一 TLS/CDN 地址可能连续建立多条连接，来源级聚合避免重复地址淹没导航树。
 * 参数：originGroup 提供同端点的事务集合；其余参数维持事务、方向和单片段的精确选择。
 * 失败语义：来源下没有事务时不渲染子项；单条详情读取失败只影响该连接的方向列表。
 */
function StreamOriginTree({
  originGroup,
  selectedTransactionId,
  selectedPacket,
  highlightedIds,
  onSelectTransaction,
  onSelectPacket,
  contextActions,
}: {
  originGroup: OriginGroup;
  selectedTransactionId: string | null;
  selectedPacket: StreamPacketSelection | null;
  highlightedIds: ReadonlySet<string>;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  onSelectPacket(selection: StreamPacketSelection): void;
  contextActions: NavigatorContextActions;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const representativeTransaction = originGroup.transactions[0];
  const trafficActive = originGroup.transactions.some((transaction) =>
    highlightedIds.has(transaction.transactionId),
  );

  return (
    <section className="streamTree">
      <TransactionContextMenu
        {...contextActions}
        seedPath=""
        transaction={representativeTransaction}
        transactionIds={originGroup.transactions.map(
          (transaction) => transaction.transactionId,
        )}
      >
        <div className="streamTreeHeader">
          <TreeToggle
            expanded={expanded}
            label={originGroup.origin}
            onToggle={() => setExpanded((current) => !current)}
          />
          <button
            aria-label={originGroup.origin}
            className={`streamTreeRoot${trafficActive ? " isTrafficActive" : ""}`}
            data-traffic-active={trafficActive ? "true" : undefined}
            onClick={() => setExpanded((current) => !current)}
            title={`${originGroup.origin}\n${originGroup.transactions.length} ${t("transactions.context.transactions")}`}
            type="button"
          >
            <Network
              aria-hidden="true"
              className="streamTransportIcon"
              size={13}
            />
            <strong>{originGroup.origin}</strong>
          </button>
        </div>
      </TransactionContextMenu>
      {expanded && (
        <div className="streamOriginChildren">
          {originGroup.transactions.map((transaction, transactionIndex) => (
            <StreamTransactionTree
              connectionIndex={transactionIndex + 1}
              key={transaction.transactionId}
              onSelectPacket={onSelectPacket}
              onSelectTransaction={onSelectTransaction}
              selectedPacket={selectedPacket}
              selected={selectedTransactionId === transaction.transactionId}
              trafficActive={highlightedIds.has(transaction.transactionId)}
              transaction={transaction}
              streamAddress={originGroup.origin}
              contextActions={contextActions}
            />
          ))}
        </div>
      )}
    </section>
  );
}

/**
 * 渲染来源、路径和原始流三类结构；SOCKS 与 WinDivert 隧道按传输方向展开，HTTP 等事务保持来源到路径的紧凑层级。
 */
function StructureView({
  transactions,
  selectedTransactionId,
  selectedPacket,
  highlightedIds,
  onSelectTransaction,
  onSelectPacket,
  contextActions,
}: {
  transactions: TransactionSummary[];
  selectedTransactionId: string | null;
  selectedPacket: StreamPacketSelection | null;
  highlightedIds: ReadonlySet<string>;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  onSelectPacket(selection: StreamPacketSelection): void;
  contextActions: NavigatorContextActions;
}) {
  const { t } = useTranslation();
  const streamGroups = useMemo(
    () =>
      groupStreamTransactions(
        transactions.filter(
          (transaction) =>
            transaction.protocol === "socks" ||
            transaction.protocol === "tunnel",
        ),
        t("transactions.table.emptyValue"),
      ),
    [t, transactions],
  );
  const groups = useMemo(
    () =>
      groupTransactions(
        collapseMediaRangeTransactions(
          transactions.filter(
            (transaction) =>
              transaction.protocol !== "socks" &&
              transaction.protocol !== "tunnel",
          ),
        ),
        t("transactions.table.emptyValue"),
      ),
    [t, transactions],
  );

  return (
    <div
      className="transactionTree"
      aria-label={t("transactions.navigator.treeLabel")}
    >
      {streamGroups.map((originGroup) => (
        <StreamOriginTree
          key={originGroup.key}
          onSelectPacket={onSelectPacket}
          onSelectTransaction={onSelectTransaction}
          selectedPacket={selectedPacket}
          selectedTransactionId={selectedTransactionId}
          highlightedIds={highlightedIds}
          originGroup={originGroup}
          contextActions={contextActions}
        />
      ))}
      {groups.map((originGroup) => (
        <OriginTree
          originGroup={originGroup}
          key={originGroup.key}
          selectedTransactionId={selectedTransactionId}
          highlightedIds={highlightedIds}
          onSelectTransaction={onSelectTransaction}
          contextActions={contextActions}
        />
      ))}
    </div>
  );
}

/**
 * 按有界窗口的 offset 顺序平铺事务列表，对齐 Sequence 时序浏览工作流。
 * 运行上下文：上游循环索引已经保证稳定顺序，此处直接渲染，避免每个实时事件再次复制并排序整个窗口。
 * 参数 transactions 只能来自当前窗口；选择、高亮和右键动作保持既有交互。空数组渲染空表体。
 */
function SequenceView({
  transactions,
  selectedTransactionId,
  highlightedIds,
  onSelectTransaction,
  contextActions,
}: {
  transactions: TransactionSummary[];
  selectedTransactionId: string | null;
  highlightedIds: ReadonlySet<string>;
  onSelectTransaction(
    transactionId: string,
    transaction: TransactionSummary,
  ): void;
  contextActions: NavigatorContextActions;
}) {
  const { t } = useTranslation();
  return (
    <div
      aria-label={t("transactions.navigator.sequenceLabel")}
      className="transactionSequenceTable"
      role="table"
    >
      <div className="transactionSequenceTableHeader" role="row">
        <span>{t("transactions.table.statusCode")}</span>
        <span>{t("transactions.table.host")}</span>
        <span>{t("transactions.table.duration")}</span>
      </div>
      <div className="transactionSequenceTableBody" role="rowgroup">
        {transactions.map((transaction) => (
          <TransactionSequenceRow
            key={transaction.transactionId}
            transaction={transaction}
            contextActions={contextActions}
            selected={selectedTransactionId === transaction.transactionId}
            trafficActive={highlightedIds.has(transaction.transactionId)}
            onSelectTransaction={onSelectTransaction}
          />
        ))}
      </div>
    </div>
  );
}

interface TransactionFilterBarProps {
  searchText: string;
  statusFilter: TransactionFilter;
  focusHost: boolean;
  focusAvailable: boolean;
  filtersActive: boolean;
  visibleCount: number;
  totalCount: number;
  onSearchChange(value: string): void;
  onStatusChange(value: TransactionFilter): void;
  onFocusChange(value: boolean): void;
  onClear(): void;
}

/**
 * 渲染事务筛选控件；只回传本地视图条件，不直接读取或修改录制集合。
 */
function TransactionFilterBar({
  searchText,
  statusFilter,
  focusHost,
  focusAvailable,
  filtersActive,
  visibleCount,
  totalCount,
  onSearchChange,
  onStatusChange,
  onFocusChange,
  onClear,
}: TransactionFilterBarProps) {
  const { t } = useTranslation();
  return (
    <div className="transactionFilterBar">
      <label
        className="filterSearch"
        title={t("transactions.navigator.searchPlaceholder")}
      >
        <Search aria-hidden="true" size={14} />
        <span className="visuallyHidden">
          {t("transactions.navigator.searchLabel")}
        </span>
        <input
          type="search"
          value={searchText}
          onChange={(event) => onSearchChange(event.target.value)}
        />
      </label>
      <select
        aria-label={t("transactions.navigator.statusLabel")}
        value={statusFilter}
        onChange={(event) =>
          onStatusChange(event.target.value as TransactionFilter)
        }
      >
        <option value="all">{t("transactions.navigator.filterAll")}</option>
        <option value="pending">
          {t("transactions.navigator.filterPending")}
        </option>
        <option value="complete">
          {t("transactions.navigator.filterComplete")}
        </option>
        <option value="failed">
          {t("transactions.navigator.filterFailed")}
        </option>
        <option value="blocked">
          {t("transactions.navigator.filterBlocked")}
        </option>
        <option value="cancelled">
          {t("transactions.navigator.filterCancelled")}
        </option>
      </select>
      <label
        className="focusToggle"
        title={t("transactions.navigator.onlyFocused")}
      >
        <input
          checked={focusHost}
          disabled={!focusAvailable}
          type="checkbox"
          onChange={(event) => onFocusChange(event.target.checked)}
        />
        <span>{t("transactions.navigator.focus")}</span>
      </label>
      {filtersActive && (
        <button className="clearFilterButton" type="button" onClick={onClear}>
          {t("transactions.navigator.clearFilters")}
        </button>
      )}
      <span className="filterCount">
        {t("transactions.selection.count", {
          visible: visibleCount,
          total: totalCount,
        })}
      </span>
    </div>
  );
}

/**
 * 渲染唯一结构导航、搜索和状态筛选；高频快照只更新发生变化的报文高亮集合。
 *
 * 运行上下文：连接工作台左侧常驻渲染，事务树默认收起并响应实时快照。
 * 参数：快照和选择状态来自工作台，设置回调把右键动作连接到主窗口既有编辑器。
 * 失败语义：复制、导出或重复失败时在导航区显示错误，筛选与树浏览继续可用。
 */
export function TransactionNavigator({
  transactionPage,
  selectedTransactionId,
  selectedPacket = null,
  selectedHost,
  onSelectTransaction,
  onSelectPacket = () => undefined,
  onOpenSslSettings,
  onOpenToolSettings,
}: TransactionNavigatorProps) {
  const { t } = useTranslation();
  const [searchText, setSearchText] = useState("");
  const [statusFilter, setStatusFilter] = useState<TransactionFilter>("all");
  const [focusedHost, setFocusedHost] = useState<string | null>(null);
  const [contextCommandError, setContextCommandError] = useState<string | null>(
    null,
  );
  const [viewMode, setViewMode] = useState<NavigatorViewMode>("structure");
  const completeCollection = useCompleteTransactionCollection(transactionPage);
  const highlightedIds = useTrafficHighlights(completeCollection.items);

  useEffect(() => {
    // collectionToken 是事务 offset 语义的代际。清空录制或 FIFO 换代后，旧主机聚焦、
    // 搜索词和状态筛选不能继续作用于新集合，否则新事务虽已录制却会被界面全部隐藏。
    setSearchText("");
    setStatusFilter("all");
    setFocusedHost(null);
    setContextCommandError(null);
  }, [transactionPage.collectionToken]);

  const visibleTransactions = useMemo(() => {
    const requestLocale = currentRequestLocale();
    const normalizedSearch = searchText.trim().toLocaleLowerCase(requestLocale);
    return completeCollection.items.filter((transaction) => {
      if (!matchesStatus(transaction, statusFilter)) {
        return false;
      }
      if (focusedHost !== null && transaction.host !== focusedHost) {
        return false;
      }
      if (!normalizedSearch) {
        return true;
      }
      return [
        transaction.method,
        transaction.host,
        transaction.path,
        transaction.query,
        transaction.urlDisplay,
        transaction.contentType,
        String(transaction.statusCode ?? ""),
        presentTransactionStatus(transaction, t),
        presentTransactionProtocol(transaction, t),
      ].some((value) =>
        value.toLocaleLowerCase(requestLocale).includes(normalizedSearch),
      );
    });
  }, [focusedHost, searchText, statusFilter, t, completeCollection.items]);

  const filtersActive =
    searchText !== "" || statusFilter !== "all" || focusedHost !== null;
  const collectionTruncated = completeCollection.itemsTruncated;

  /**
   * 清空全部本地筛选条件；该操作不触发控制 API，也不修改录制集合。
   */
  const clearFilters = () => {
    setSearchText("");
    setStatusFilter("all");
    setFocusedHost(null);
  };

  /**
   * 构造所有树节点共享的右键动作；聚焦主机保存明确主机值，不随右侧选择变化而漂移。
   */
  const contextActions: NavigatorContextActions = {
    focusActive: focusedHost !== null,
    onFocusHost: (host) => {
      setContextCommandError(null);
      setFocusedHost(host);
    },
    onClearFocus: () => setFocusedHost(null),
    onOpenSslSettings,
    onOpenToolSettings,
    onCommandError: setContextCommandError,
  };

  return (
    <section
      className="transactionNavigatorPane"
      aria-label={t("transactions.navigator.regionLabel")}
    >
      <header className="transactionNavigatorHeader">
        <Network aria-hidden="true" size={14} />
        <div
          className="navigatorViewTabs"
          role="tablist"
          aria-label={t("transactions.navigator.viewMode")}
        >
          <button
            aria-selected={viewMode === "structure"}
            className={viewMode === "structure" ? "isActive" : undefined}
            role="tab"
            type="button"
            onClick={() => setViewMode("structure")}
          >
            {t("transactions.navigator.structure")}
          </button>
          <button
            aria-selected={viewMode === "sequence"}
            className={viewMode === "sequence" ? "isActive" : undefined}
            role="tab"
            type="button"
            onClick={() => setViewMode("sequence")}
          >
            {t("transactions.navigator.sequence")}
          </button>
        </div>
      </header>
      <TransactionFilterBar
        searchText={searchText}
        statusFilter={statusFilter}
        focusHost={focusedHost !== null}
        focusAvailable={selectedHost !== null}
        filtersActive={filtersActive}
        visibleCount={visibleTransactions.length}
        totalCount={completeCollection.total}
        onSearchChange={setSearchText}
        onStatusChange={setStatusFilter}
        onFocusChange={(enabled) =>
          setFocusedHost(enabled ? selectedHost : null)
        }
        onClear={clearFilters}
      />
      <div className="transactionNavigatorBody">
        {completeCollection.loading && (
          <div className="transactionCollectionNotice" role="status">
            {t("transactions.collection.loadingHistory")}
          </div>
        )}
        {completeCollection.loadFailed && (
          <div
            className="transactionCollectionNotice viewerNotice--error"
            role="alert"
          >
            {t("transactions.collection.loadFailed")}
          </div>
        )}
        {collectionTruncated && (
          <div className="transactionCollectionNotice" role="status">
            {t("transactions.collection.itemsTruncated")}
          </div>
        )}
        {contextCommandError !== null && (
          <div
            className="transactionCollectionNotice viewerNotice--error"
            role="alert"
          >
            {contextCommandError}
          </div>
        )}
        <div
          className="transactionNavigatorContent"
          aria-label={
            viewMode === "sequence"
              ? t("transactions.navigator.sequenceLabel")
              : t("transactions.navigator.treeLabel")
          }
        >
          {visibleTransactions.length === 0 ? (
            <div className="emptyState">
              <Activity aria-hidden="true" size={22} />
              <strong>
                {completeCollection.items.length === 0
                  ? t("transactions.navigator.emptyTitle")
                  : t("transactions.navigator.emptyFilteredTitle")}
              </strong>
              <span>
                {completeCollection.items.length === 0
                  ? t("transactions.navigator.emptyHint")
                  : t("transactions.navigator.emptyFilteredHint")}
              </span>
            </div>
          ) : viewMode === "sequence" ? (
            <SequenceView
              transactions={visibleTransactions}
              selectedTransactionId={selectedTransactionId}
              highlightedIds={highlightedIds}
              onSelectTransaction={onSelectTransaction}
              contextActions={contextActions}
            />
          ) : (
            <StructureView
              transactions={visibleTransactions}
              selectedTransactionId={selectedTransactionId}
              selectedPacket={selectedPacket}
              highlightedIds={highlightedIds}
              onSelectTransaction={onSelectTransaction}
              onSelectPacket={onSelectPacket}
              contextActions={contextActions}
            />
          )}
        </div>
      </div>
    </section>
  );
}
