import {
  Activity,
  Braces,
  ChartNoAxesGantt,
  FileText,
  Info,
  Network,
  PanelTop,
} from "lucide-react";
import {
  type CSSProperties,
  type RefObject,
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import type {
  EncodedBodyResponse,
  HeaderField,
  MessageSide,
  TransactionDetail,
  TransactionSummary,
} from "../api/protocol";
import type { MediaPreviewBody } from "../api/controlClient";
import { useServiceStore } from "../state/serviceStore";
import { activateAdjacentTab } from "./tabNavigation";
import {
  type ByteSelectionRange,
  type HexPaneRepresentation,
  byteSelectionFromText,
  readPaneTextSelection,
  textSelectionFromBytes,
} from "./hexSelection";
import { ProtocolInspector } from "./protocolInspector";
import { BodyMediaPreview } from "./bodyMediaPreview";
import { TransactionRepeatActions } from "./transactionRepeatActions";
import type { StreamPacketSelection } from "./streamPacketSelection";
import { useAxisSplitter } from "./useAxisSplitter";
import {
  formatTransactionBytes,
  presentTransactionStatus,
  presentStreamTransport,
  transactionStatusTone,
} from "./transactionPresentation";
import {
  type LiveTransactionDetailState,
  transactionDetailRevision,
  useLiveTransactionDetail,
} from "./useLiveTransactionDetail";
import {
  NotesView,
  OverviewView,
  StreamDirectionChart,
  StreamDirectionOverview,
  StreamDirectionSummary,
  SummaryView,
  TimingChartView,
} from "./transactionInspectorViews";

type InspectorView =
  "overview" | "contents" | "summary" | "chart" | "protocol" | "notes";
type MessageView = "headers" | "preview" | "text" | "json" | "hex";
type DetailState = LiveTransactionDetailState;
type BodyState =
  | { kind: "loading" }
  | { kind: "ready"; body: EncodedBodyResponse }
  | { kind: "media"; preview: MediaPreviewBody }
  | { kind: "error" };

interface TransactionInspectorProps {
  transaction: TransactionSummary | null;
  selectedPacket: StreamPacketSelection | null;
  onPacketUnavailable(selection: StreamPacketSelection): void;
}

const inspectorTabs = [
  { value: "overview", labelKey: "viewer.tabs.overview", icon: Info },
  { value: "contents", labelKey: "viewer.tabs.contents", icon: Network },
  { value: "summary", labelKey: "viewer.tabs.summary", icon: PanelTop },
  { value: "chart", labelKey: "viewer.tabs.chart", icon: ChartNoAxesGantt },
  { value: "protocol", labelKey: "viewer.tabs.protocol", icon: Braces },
  { value: "notes", labelKey: "viewer.tabs.notes", icon: FileText },
] as const;

const messageTabs = [
  { value: "headers", labelKey: "viewer.views.headers" },
  { value: "text", labelKey: "viewer.views.text" },
  { value: "json", labelKey: "viewer.views.json" },
  { value: "hex", labelKey: "viewer.views.hex" },
  { value: "preview", labelKey: "viewer.views.preview" },
] as const;

const contentsDividerHeight = 7;
const hexDividerWidth = 7;
/**
 * 判断异常是否来自主动取消；取消请求是页签/选择切换的正常生命周期，不进入错误界面。
 */
function isAbortError(error: unknown): boolean {
  return error instanceof Error && error.name === "AbortError";
}

/**
 * 复制完整头集合；每个重复头保持原始顺序和独立行，避免调试时丢失线序语义。
 */
async function copyHeaders(headers: HeaderField[]): Promise<void> {
  await navigator.clipboard.writeText(
    headers.map((header) => `${header.name}: ${header.value}`).join("\n"),
  );
}

/**
 * 渲染可搜索请求/响应头；搜索仅影响视图，不改变详情快照或正文加载状态。
 */
function HeadersView({
  headers,
  headersTruncated,
}: {
  headers: HeaderField[];
  headersTruncated: boolean;
}) {
  const { t } = useTranslation();
  const [searchText, setSearchText] = useState("");
  const [copyFailed, setCopyFailed] = useState(false);
  const visibleHeaders = useMemo(() => {
    const normalizedSearch = searchText.trim().toLocaleLowerCase();
    if (!normalizedSearch) {
      return headers;
    }
    return headers.filter((header) =>
      `${header.name}\n${header.value}`
        .toLocaleLowerCase()
        .includes(normalizedSearch),
    );
  }, [headers, searchText]);

  return (
    <div className="headersViewer">
      <div className="messageViewerToolbar">
        <label className="filterSearch">
          <span className="visuallyHidden">
            {t("viewer.headers.searchLabel")}
          </span>
          <input
            type="search"
            placeholder={t("viewer.headers.searchPlaceholder")}
            value={searchText}
            onChange={(event) => setSearchText(event.target.value)}
          />
        </label>
        <button
          disabled={headers.length === 0}
          type="button"
          onClick={() => {
            setCopyFailed(false);
            void copyHeaders(headers).catch(() => setCopyFailed(true));
          }}
        >
          {t("viewer.headers.copy")}
        </button>
      </div>
      {copyFailed && (
        <div className="viewerNotice viewerNotice--error" role="alert">
          {t("viewer.headers.copyFailed")}
        </div>
      )}
      {headersTruncated && (
        <div className="viewerNotice" role="status">
          {t("viewer.headers.headersTruncated")}
        </div>
      )}
      {visibleHeaders.length === 0 ? (
        <div className="emptyState">
          <span>{t("viewer.headers.empty")}</span>
        </div>
      ) : (
        <div className="headersTable" role="table">
          <div className="headersTableHeader" role="row">
            <span role="columnheader">{t("viewer.headers.name")}</span>
            <span role="columnheader">{t("viewer.headers.value")}</span>
          </div>
          {visibleHeaders.map((header, headerIndex) => (
            <div
              className="headersTableRow"
              key={`${header.name}:${headerIndex}`}
              role="row"
            >
              <span role="cell">{header.name}</span>
              <span role="cell">{header.value}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * 从标准 Base64 解码用户明确打开的完整正文。
 *
 * 运行上下文：正文接口只在用户进入正文页签后调用，因此列表与未选中的包不承担解码成本。
 * 参数：body 是后端已校验长度的完整正文响应。
 * 失败语义：协议校验已在控制客户端完成；浏览器拒绝非法 Base64 时让异常进入现有正文加载失败路径。
 */
function decodeBase64Body(base64: string): Uint8Array {
  const decodedBinary = window.atob(base64);
  const bodyBytes = new Uint8Array(decodedBinary.length);
  for (let byteIndex = 0; byteIndex < decodedBinary.length; byteIndex += 1) {
    bodyBytes[byteIndex] = decodedBinary.charCodeAt(byteIndex);
  }
  return bodyBytes;
}

/**
 * 解码服务端已校验的原始正文；该包装保留事务正文是唯一真源的类型语义。
 */
function decodeCompleteBody(body: EncodedBodyResponse): Uint8Array {
  return decodeBase64Body(body.base64);
}

interface HexPaneLineLengths {
  asciiBytes: number;
  hexBytes: number;
}

const defaultHexPaneLineLengths: HexPaneLineLengths = {
  asciiBytes: 48,
  hexBytes: 16,
};
const hexDumpCharacterProbeLength = 16;
const hexDumpPaneHorizontalPaddingPixels = 20;
const hexDumpHexCharactersPerByte = 3;

/**
 * 根据面板可用宽度和等宽字符实际宽度计算一行可容纳的字节数；十六进制额外包含字节间空格。
 *
 * 运行上下文：浏览器缩放、字体替换和检查器分栏拖拽都会改变字符与面板宽度，计算必须基于实际 DOM 尺寸。
 * 参数：paneWidth 是当前面板宽度；characterWidth 是测量得到的单字符宽度；charactersPerByte 是该视图的字符占用。
 * 失败语义：尺寸尚未完成布局时返回 null，调用方保留稳定的默认列数，避免首帧抖动。
 */
function calculateHexPaneByteCount(
  paneWidth: number,
  characterWidth: number,
  charactersPerByte: number,
): number | null {
  if (paneWidth <= 0 || characterWidth <= 0) {
    return null;
  }
  const availableWidth = Math.max(
    0,
    paneWidth - hexDumpPaneHorizontalPaddingPixels,
  );
  return Math.max(
    1,
    Math.floor(availableWidth / (characterWidth * charactersPerByte)),
  );
}

/**
 * 把扁平字节单元按指定列数格式化成单个面板文本；两个面板可独立换行而不压缩彼此的可用宽度。
 *
 * 运行上下文：网络数据工作台的双文本面板中，十六进制与 ASCII 各自填充自己的等宽文本区。
 * 参数：bodyBytes 是正文的顺序字节；bytesPerLine 是当前面板列数；representation 决定输出十六进制或 ASCII。
 * 失败语义：没有字节时返回空文本，不制造额外占位行。
 */
function formatHexPaneContent(
  bodyBytes: Uint8Array,
  bytesPerLine: number,
  representation: "ascii" | "hex",
): string {
  const lines: string[] = [];
  for (let offset = 0; offset < bodyBytes.length; offset += bytesPerLine) {
    const lineBytes = bodyBytes.subarray(offset, offset + bytesPerLine);
    lines.push(
      Array.from(lineBytes, (byte) =>
        representation === "hex"
          ? byte.toString(16).padStart(2, "0")
          : byte >= 32 && byte <= 126
            ? String.fromCharCode(byte)
            : ".",
      ).join(representation === "hex" ? " " : ""),
    );
  }
  return lines.join("\n");
}

interface LinkedHexSelection {
  bytes: ByteSelectionRange;
  source: HexPaneRepresentation;
}

/**
 * 在非原生选择面板中渲染同一字节区间；源面板继续使用浏览器 Selection，目标面板使用 mark 保持字节语义一致。
 *
 * 运行上下文：两侧响应式列数不同，同一字节区间在文本中的起止偏移必须分别计算。
 * 参数：content 是当前面板完整文本；selection 是正文坐标；bytesPerLine 与 representation 描述目标布局。
 * 失败语义：区间无效或超出当前文本时返回原文，不插入空标记。
 */
function renderLinkedHexSelection(
  content: string,
  selection: ByteSelectionRange,
  bytesPerLine: number,
  representation: HexPaneRepresentation,
): ReactNode {
  const textSelection = textSelectionFromBytes(
    selection,
    bytesPerLine,
    representation,
  );
  if (
    textSelection === null ||
    textSelection.startOffset >= content.length ||
    textSelection.endOffset <= textSelection.startOffset
  ) {
    return content;
  }
  const endOffset = Math.min(content.length, textSelection.endOffset);
  return (
    <>
      {content.slice(0, textSelection.startOffset)}
      <mark className="hexDumpLinkedSelection">
        {content.slice(textSelection.startOffset, endOffset)}
      </mark>
      {content.slice(endOffset)}
    </>
  );
}

/**
 * 管理双文本面板的动态列数；沿用 TCP 代理按实际可视宽度重新分行的策略，禁止固定 16 字节挤压窄栏。
 *
 * 运行上下文：观察两个面板的尺寸变化，适配窗口缩放和事务导航分割条拖拽。
 * 参数：无显式参数，返回两个面板与字符探针的引用及当前行宽。
 * 失败语义：ResizeObserver 不可用或尺寸为零时保留默认行宽，正文仍可安全显示。
 */
function useHexPaneLineLengths(): {
  asciiContentRef: RefObject<HTMLPreElement>;
  characterProbeRef: RefObject<HTMLSpanElement>;
  hexContentRef: RefObject<HTMLPreElement>;
  lineLengths: HexPaneLineLengths;
} {
  const asciiContentRef = useRef<HTMLPreElement>(null);
  const characterProbeRef = useRef<HTMLSpanElement>(null);
  const hexContentRef = useRef<HTMLPreElement>(null);
  const [lineLengths, setLineLengths] = useState<HexPaneLineLengths>(
    defaultHexPaneLineLengths,
  );

  useLayoutEffect(() => {
    const updateLineLengths = () => {
      const probeWidth =
        characterProbeRef.current?.getBoundingClientRect().width ?? 0;
      const characterWidth = probeWidth / hexDumpCharacterProbeLength;
      const hexBytes = calculateHexPaneByteCount(
        hexContentRef.current?.clientWidth ?? 0,
        characterWidth,
        hexDumpHexCharactersPerByte,
      );
      const asciiBytes = calculateHexPaneByteCount(
        asciiContentRef.current?.clientWidth ?? 0,
        characterWidth,
        1,
      );
      if (hexBytes === null || asciiBytes === null) {
        return;
      }
      setLineLengths((current) =>
        current.hexBytes === hexBytes && current.asciiBytes === asciiBytes
          ? current
          : { asciiBytes, hexBytes },
      );
    };

    updateLineLengths();
    const observer = new ResizeObserver(updateLineLengths);
    if (hexContentRef.current !== null) {
      observer.observe(hexContentRef.current);
    }
    if (asciiContentRef.current !== null) {
      observer.observe(asciiContentRef.current);
    }
    return () => observer.disconnect();
  }, []);

  return {
    asciiContentRef,
    characterProbeRef,
    hexContentRef,
    lineLengths,
  };
}

/**
 * 渲染双文本正文视图；十六进制与 ASCII 均独立填满可调栏，并按比例同步垂直滚动。
 *
 * 运行上下文：只渲染用户已明确打开的完整正文，不参与正文加载、复制或协议解析。
 * 参数：bodyBytes 是完整顺序字节快照；asciiLabel 与 ariaLabel 提供本地化说明。
 * 失败语义：空正文渲染为空面板；布局测量未就绪时使用默认行宽，不截断字节。
 */
function HexDumpView({
  bodyBytes,
  asciiLabel,
  ariaLabel,
}: {
  bodyBytes: Uint8Array;
  asciiLabel: string;
  ariaLabel: string;
}) {
  const hexDumpRef = useRef<HTMLDivElement>(null);
  const { asciiContentRef, characterProbeRef, hexContentRef, lineLengths } =
    useHexPaneLineLengths();
  const synchronizingScrollRef = useRef(false);
  const selectionFrameRef = useRef<number | null>(null);
  const [linkedSelection, setLinkedSelection] =
    useState<LinkedHexSelection | null>(null);
  const hexSplitter = useAxisSplitter(hexDumpRef, {
    axis: "vertical",
    dividerSize: hexDividerWidth,
  });
  const hexText = useMemo(
    () => formatHexPaneContent(bodyBytes, lineLengths.hexBytes, "hex"),
    [bodyBytes, lineLengths.hexBytes],
  );
  const asciiText = useMemo(
    () => formatHexPaneContent(bodyBytes, lineLengths.asciiBytes, "ascii"),
    [bodyBytes, lineLengths.asciiBytes],
  );

  useEffect(() => {
    /**
     * 把当前原生文本选择投影到正文坐标；按动画帧合并 selectionchange，避免拖动大正文时重复扫描 DOM。
     */
    const updateLinkedSelection = () => {
      selectionFrameRef.current = null;
      const browserSelection = window.getSelection();
      if (browserSelection === null) {
        setLinkedSelection(null);
        return;
      }
      const candidates = [
        {
          pane: hexContentRef.current,
          bytesPerLine: lineLengths.hexBytes,
          representation: "hex" as const,
        },
        {
          pane: asciiContentRef.current,
          bytesPerLine: lineLengths.asciiBytes,
          representation: "ascii" as const,
        },
      ];
      for (const candidate of candidates) {
        if (candidate.pane === null) {
          continue;
        }
        const textSelection = readPaneTextSelection(
          browserSelection,
          candidate.pane,
        );
        if (textSelection === null) {
          continue;
        }
        const bytes = byteSelectionFromText(
          textSelection,
          candidate.bytesPerLine,
          candidate.representation,
          bodyBytes.length,
        );
        setLinkedSelection(
          bytes === null ? null : { bytes, source: candidate.representation },
        );
        return;
      }
      setLinkedSelection(null);
    };

    /** selectionchange 可能在一次指针移动中连续触发，只保留下一绘制帧的一次同步计算。 */
    const scheduleLinkedSelectionUpdate = () => {
      if (selectionFrameRef.current !== null) {
        return;
      }
      selectionFrameRef.current = window.requestAnimationFrame(
        updateLinkedSelection,
      );
    };
    document.addEventListener("selectionchange", scheduleLinkedSelectionUpdate);
    return () => {
      document.removeEventListener(
        "selectionchange",
        scheduleLinkedSelectionUpdate,
      );
      if (selectionFrameRef.current !== null) {
        window.cancelAnimationFrame(selectionFrameRef.current);
        selectionFrameRef.current = null;
      }
    };
  }, [
    asciiContentRef,
    bodyBytes.length,
    hexContentRef,
    lineLengths.asciiBytes,
    lineLengths.hexBytes,
  ]);

  const renderedHexText =
    linkedSelection === null || linkedSelection.source === "hex"
      ? hexText
      : renderLinkedHexSelection(
          hexText,
          linkedSelection.bytes,
          lineLengths.hexBytes,
          "hex",
        );
  const renderedAsciiText =
    linkedSelection === null || linkedSelection.source === "ascii"
      ? asciiText
      : renderLinkedHexSelection(
          asciiText,
          linkedSelection.bytes,
          lineLengths.asciiBytes,
          "ascii",
        );

  /**
   * 按可滚动距离比例同步另一个面板；两侧每行字节数不同，不能直接复制 scrollTop。
   *
   * 运行上下文：用户滚动任意一侧文本面板时调用，另一侧保持同一正文进度。
   * 参数：source 是触发滚动的面板；target 是待同步面板。
   * 失败语义：任一面板尚未挂载或没有可滚动距离时直接返回。
   */
  const synchronizePaneScroll = useCallback(
    (source: HTMLPreElement, target: HTMLPreElement | null) => {
      if (synchronizingScrollRef.current || target === null) {
        return;
      }
      const sourceScrollableHeight = source.scrollHeight - source.clientHeight;
      const targetScrollableHeight = target.scrollHeight - target.clientHeight;
      if (sourceScrollableHeight <= 0 || targetScrollableHeight <= 0) {
        return;
      }
      synchronizingScrollRef.current = true;
      target.scrollTop =
        (source.scrollTop / sourceScrollableHeight) * targetScrollableHeight;
      requestAnimationFrame(() => {
        synchronizingScrollRef.current = false;
      });
    },
    [],
  );

  return (
    <div
      aria-label={ariaLabel}
      className="hexDump"
      data-resizing={hexSplitter.resizing || undefined}
      ref={hexDumpRef}
      style={{
        gridTemplateColumns: `minmax(0, ${hexSplitter.ratio}fr) ${hexDividerWidth}px minmax(0, ${1 - hexSplitter.ratio}fr)`,
      }}
    >
      <span
        aria-hidden="true"
        className="hexDumpCharacterProbe"
        ref={characterProbeRef}
      >
        {"0".repeat(hexDumpCharacterProbeLength)}
      </span>
      <section aria-label={ariaLabel} className="hexDumpPane">
        <header>{ariaLabel}</header>
        <pre
          className="hexDumpPaneContent"
          ref={hexContentRef}
          onPointerDown={() => setLinkedSelection(null)}
          onScroll={(event) =>
            synchronizePaneScroll(event.currentTarget, asciiContentRef.current)
          }
        >
          {renderedHexText}
        </pre>
      </section>
      <div
        aria-label={`${ariaLabel} / ${asciiLabel}`}
        aria-orientation="vertical"
        aria-valuemax={100}
        aria-valuemin={0}
        aria-valuenow={Math.round(hexSplitter.ratio * 100)}
        className="hexDivider"
        role="separator"
        tabIndex={0}
        onKeyDown={hexSplitter.handleKeyDown}
        onPointerCancel={hexSplitter.finishResize}
        onPointerDown={hexSplitter.beginResize}
        onPointerMove={hexSplitter.continueResize}
        onPointerUp={hexSplitter.finishResize}
      >
        <span aria-hidden="true" className="hexDividerGrip" />
      </div>
      <section
        aria-label={asciiLabel}
        className="hexDumpPane hexDumpPane--ascii"
      >
        <header>{asciiLabel}</header>
        <pre
          className="hexDumpPaneContent"
          ref={asciiContentRef}
          onPointerDown={() => setLinkedSelection(null)}
          onScroll={(event) =>
            synchronizePaneScroll(event.currentTarget, hexContentRef.current)
          }
        >
          {renderedAsciiText}
        </pre>
      </section>
    </div>
  );
}

/**
 * 把按需加载的完整正文转换为当前子视图；JSON 始终基于完整字节解析，防止半包误报。
 * `contentType` 中的标准 charset 驱动浏览器 TextDecoder，未知标签回到 UTF-8；十六进制
 * 返回结构化字节供布局渲染，文本与 JSON 返回完整文本，不设置二次 UI 截断。
 */
function renderBodyPreview(
  bodyBytes: Uint8Array,
  contentType: string,
  view: Exclude<MessageView, "headers" | "preview">,
  jsonParseFailed: string,
  jsonFallback: string,
): { kind: "hex"; bodyBytes: Uint8Array } | { kind: "text"; content: string } {
  if (view === "hex") {
    return {
      kind: "hex",
      bodyBytes,
    };
  }
  const charsetMatch = /(?:^|;)\s*charset\s*=\s*["']?([^;"'\s]+)/i.exec(
    contentType,
  );
  let decodedText: string;
  try {
    decodedText = new TextDecoder(charsetMatch?.[1] ?? "utf-8").decode(
      bodyBytes,
    );
  } catch {
    // 未知 charset 不允许阻断正文查看；UTF-8 是 HTTP 文本与 JSON 的统一默认显示编码。
    decodedText = new TextDecoder("utf-8").decode(bodyBytes);
  }
  if (view === "text") {
    return {
      kind: "text",
      content: decodedText,
    };
  }
  try {
    return {
      kind: "text",
      content: JSON.stringify(JSON.parse(decodedText), null, 2),
    };
  } catch {
    return {
      kind: "text",
      content: `${jsonParseFailed}\n${jsonFallback}\n\n${decodedText}`,
    };
  }
}

/**
 * 从聚合正文中截取一个流片段；后端已验证范围，前端再次检查边界以避免损坏快照导致查看器读取越界。
 */
function isolateStreamPacketBody(
  body: EncodedBodyResponse,
  packet: TransactionDetail["requestPackets"][number],
): EncodedBodyResponse {
  const binaryBody = window.atob(body.base64);
  const endOffset = packet.storedOffsetBytes + packet.storedBytes;
  if (endOffset > binaryBody.length) {
    throw new Error("streamPacketRangeOutsideBody");
  }
  const packetBinary = binaryBody.slice(packet.storedOffsetBytes, endOffset);
  return {
    revision: body.revision,
    meta: {
      ...body.meta,
      storedBytes: packet.storedBytes,
      originalBytes: packet.originalBytes,
      truncated: packet.truncated,
    },
    base64: window.btoa(packetBinary),
    decoded: null,
  };
}

/**
 * 按消息侧加载有界正文预览；切换事务、报文或子视图时取消旧请求，确保检查区域只显示当前数据。
 */
function BodyView({
  transactionId,
  side,
  view,
  bodyMeta,
  packet,
  sourceUrl,
}: {
  transactionId: string;
  side: MessageSide;
  view: Exclude<MessageView, "headers">;
  bodyMeta: TransactionDetail["requestBody"];
  packet?: TransactionDetail["requestPackets"][number];
  sourceUrl: string;
}) {
  const { t } = useTranslation();
  const {
    getResponseMediaPreview,
    getTransactionBody,
    snapshot: serviceSnapshot,
  } = useServiceStore();
  const bodyAbortControllerRef = useRef<AbortController | null>(null);
  const bodyRequestGenerationRef = useRef(0);
  const desiredBodyRevisionRef = useRef("");
  const loadedBodyRevisionRef = useRef<string | null>(null);
  const bodyAvailableRef = useRef(bodyMeta !== null);
  const [bodyState, setBodyState] = useState<BodyState>({
    kind: "loading",
  });
  const mediaPreviewRequested =
    view === "preview" &&
    side === "response" &&
    (bodyMeta?.contentType.toLocaleLowerCase().startsWith("audio/") ||
      bodyMeta?.contentType.toLocaleLowerCase().startsWith("video/"));
  bodyAvailableRef.current = bodyMeta !== null;
  const mediaSourceTransaction = serviceSnapshot?.transactions.items.find(
    (candidate) => candidate.transactionId === transactionId,
  );
  const mediaSourceUrl = sourceUrl || mediaSourceTransaction?.urlDisplay || "";
  // 媒体端点会跨 Range 事务聚合同一 URL；同资源的新分片必须驱动预览补读，而无关流量不得触发 GET。
  const mediaAggregateRevision = mediaPreviewRequested
    ? (serviceSnapshot?.transactions.items
        .filter(
          (candidate) =>
            candidate.urlDisplay === mediaSourceUrl &&
            candidate.contentType.toLocaleLowerCase() ===
              bodyMeta?.contentType.toLocaleLowerCase(),
        )
        .map((candidate) => transactionDetailRevision(candidate))
        .join("|") ?? "")
    : "";
  const bodyRevision = JSON.stringify([
    bodyMeta?.contentType ?? null,
    bodyMeta?.encoding ?? null,
    bodyMeta?.originalBytes ?? null,
    bodyMeta?.storedBytes ?? null,
    bodyMeta?.truncated ?? null,
    mediaAggregateRevision,
  ]);
  desiredBodyRevisionRef.current = bodyRevision;
  const loadedBodyPresentation = useMemo(() => {
    if (bodyState.kind !== "ready") {
      return null;
    }
    const displayedBody =
      packet === undefined
        ? bodyState.body
        : isolateStreamPacketBody(bodyState.body, packet);
    // 自动解码只应用于完整应用消息；Hex 与单包视图始终保持抓获到的原始字节，便于逐字节核验。
    const decodedBody =
      packet === undefined && view !== "hex"
        ? displayedBody.decoded ?? null
        : null;
    return {
      body: displayedBody,
      bytes:
        decodedBody === null
          ? decodeCompleteBody(displayedBody)
          : decodeBase64Body(decodedBody.base64),
      contentType:
        decodedBody?.contentType ?? displayedBody.meta.contentType,
    };
  }, [bodyState, packet, view]);
  /**
   * 读取当前正文或媒体聚合视图；同一消息侧只保留一个在途请求，期间到达的新 SSE 代际合并为一次补读。
   * 刷新已经可见的正文时保留旧内容，失败也不闪回占位；首次读取失败才进入明确错误状态。
   */
  const requestLatestBody = useCallback(function requestLatestBody(): void {
    if (
      !bodyAvailableRef.current ||
      bodyAbortControllerRef.current !== null ||
      loadedBodyRevisionRef.current === desiredBodyRevisionRef.current
    ) {
      return;
    }
    const abortController = new AbortController();
    bodyAbortControllerRef.current = abortController;
    const requestGeneration = bodyRequestGenerationRef.current;
    const requestedRevision = desiredBodyRevisionRef.current;
    setBodyState((current) =>
      current.kind === "ready" || current.kind === "media"
        ? current
        : { kind: "loading" },
    );
    const bodyRequest = mediaPreviewRequested
      ? getResponseMediaPreview(transactionId, abortController.signal).then(
          (preview) => ({ kind: "media", preview }) as const,
        )
      : getTransactionBody(transactionId, side, abortController.signal).then(
          (body) => ({ kind: "ready", body }) as const,
        );
    void bodyRequest
      .then((nextState) => {
        if (
          !abortController.signal.aborted &&
          bodyRequestGenerationRef.current === requestGeneration
        ) {
          loadedBodyRevisionRef.current = requestedRevision;
          setBodyState(nextState);
        }
      })
      .catch((error: unknown) => {
        if (
          !isAbortError(error) &&
          bodyRequestGenerationRef.current === requestGeneration
        ) {
          loadedBodyRevisionRef.current = requestedRevision;
          setBodyState((current) =>
            current.kind === "ready" || current.kind === "media"
              ? current
              : { kind: "error" },
          );
        }
      })
      .finally(() => {
        if (bodyAbortControllerRef.current === abortController) {
          bodyAbortControllerRef.current = null;
        }
        if (
          !abortController.signal.aborted &&
          bodyRequestGenerationRef.current === requestGeneration &&
          loadedBodyRevisionRef.current !== desiredBodyRevisionRef.current
        ) {
          requestLatestBody();
        }
      });
  }, [
    getResponseMediaPreview,
    getTransactionBody,
    mediaPreviewRequested,
    side,
    transactionId,
  ]);

  /** 显式重试当前正文；只清除已完成代际，不取消或并发启动已有请求。 */
  const retryBody = useCallback(() => {
    loadedBodyRevisionRef.current = null;
    requestLatestBody();
  }, [requestLatestBody]);

  useLayoutEffect(() => {
    bodyRequestGenerationRef.current += 1;
    loadedBodyRevisionRef.current = null;
    if (bodyAvailableRef.current) {
      setBodyState({ kind: "loading" });
      requestLatestBody();
    }
    return () => {
      bodyRequestGenerationRef.current += 1;
      bodyAbortControllerRef.current?.abort();
      bodyAbortControllerRef.current = null;
    };
    // requestLatestBody 只在事务、方向或普通正文/媒体端点切换时变化；正文增长不触发清理和取消。
  }, [mediaPreviewRequested, requestLatestBody, side, transactionId]);

  useLayoutEffect(() => {
    desiredBodyRevisionRef.current = bodyRevision;
    if (bodyMeta === null) {
      bodyAbortControllerRef.current?.abort();
      bodyAbortControllerRef.current = null;
      loadedBodyRevisionRef.current = null;
      return;
    }
    // 正文和媒体聚合只跟随 SSE 摘要代际补读；不存在固定频率计时器或每事件取消在途请求。
    requestLatestBody();
  }, [bodyMeta, bodyRevision, requestLatestBody]);

  if (bodyMeta === null) {
    return (
      <div className="emptyState">
        <span>{t("viewer.body.noBody")}</span>
      </div>
    );
  }
  if (bodyState.kind === "loading") {
    // 本地控制接口通常在毫秒级返回；保留稳定空白画布可避免短请求闪出“正在加载”再立即消失。
    return <div aria-busy="true" className="bodyViewer" />;
  }
  if (bodyState.kind === "error") {
    return (
      <div className="emptyState">
        <strong>{t("viewer.body.loadFailed")}</strong>
        <button type="button" onClick={retryBody}>
          {t("viewer.retry")}
        </button>
      </div>
    );
  }

  if (bodyState.kind === "media") {
    const previewUrl = bodyState.preview.streamUrl;
    if (previewUrl === null) {
      return (
        <div className="emptyState">
          <strong>{t("viewer.body.mediaSegmentsIncomplete")}</strong>
        </div>
      );
    }
    // 聚合端点原生支持 Range，地址必须在事务生命周期内保持稳定；capturedBytes 只推进可读上限，
    // 浏览器随后发出的范围读取会取得新增字节，而不会替换媒体元素、归零播放位置或重复解码已播内容。
    return (
      <div className="bodyViewer">
        {bodyState.preview.status === "continuousPrefix" && (
          <div className="viewerNotice" role="status">
            {t("viewer.body.mediaContinuousPrefix", {
              captured: formatTransactionBytes(bodyState.preview.capturedBytes),
              total: formatTransactionBytes(bodyState.preview.totalBytes),
            })}
          </div>
        )}
        <BodyMediaPreview
          availableBytes={bodyState.preview.capturedBytes}
          bodyUrl={previewUrl}
          contentEncoding="identity"
          contentType={bodyState.preview.mimeType}
          sourceUrl={sourceUrl}
        />
      </div>
    );
  }
  // 服务快照会高频刷新 Context；正文未变化时必须复用同一字节对象，否则媒体预览会反复
  // 撤销并创建 Blob URL，浏览器每次重新解码图片、音频或视频就会产生可见闪烁。
  const displayedBody = loadedBodyPresentation!.body;
  const bodyBytes = loadedBodyPresentation!.bytes;
  const displayedContentType = loadedBodyPresentation!.contentType;
  const preview =
    view === "preview"
      ? null
      : renderBodyPreview(
          bodyBytes,
          displayedContentType,
          view,
          t("viewer.body.jsonParseFailed"),
          t("viewer.body.jsonFallback"),
        );
  return (
    <div className="bodyViewer">
      {displayedBody.meta.truncated && (
        <div className="viewerNotice" role="status">
          {t("viewer.body.truncated")}
        </div>
      )}
      {view === "preview" ? (
        <BodyMediaPreview
          bodyBytes={bodyBytes}
          contentEncoding={displayedBody.meta.encoding}
          contentType={displayedContentType}
          sourceUrl={sourceUrl}
        />
      ) : preview?.kind === "hex" ? (
        <HexDumpView
          ariaLabel={t("viewer.views.hex")}
          asciiLabel={t("viewer.body.hexAscii")}
          key={`${transactionId}:${side}:${packet?.sequence ?? "body"}:${displayedBody.meta.storedBytes}`}
          bodyBytes={preview.bodyBytes}
        />
      ) : (
        <pre className={`bodyPreview bodyPreview--${view}`}>
          {preview?.content ?? ""}
        </pre>
      )}
    </div>
  );
}

/**
 * 选择消息详情的初始子视图；无头的原始流直接进入 Hex，JSON 与文本正文进入可读视图。
 */
function initialMessageView(
  headers: HeaderField[],
  bodyMeta: TransactionDetail["requestBody"],
): MessageView {
  if (headers.length > 0 || bodyMeta === null) {
    return "headers";
  }
  const contentType = bodyMeta.contentType.toLocaleLowerCase();
  if (
    contentType.startsWith("image/") ||
    contentType.startsWith("audio/") ||
    contentType.startsWith("video/") ||
    contentType.includes("html")
  ) {
    return "preview";
  }
  if (contentType.includes("json")) {
    return "json";
  }
  if (
    contentType.startsWith("text/") ||
    contentType.includes("xml") ||
    contentType.includes("javascript")
  ) {
    return "text";
  }
  return "hex";
}

/**
 * 渲染请求或响应消息检查器；头与正文子视图复用同一份事务详情，存在正文时自动读取。
 */
function MessageViewPanel({
  detail,
  side,
}: {
  detail: TransactionDetail;
  side: MessageSide;
}) {
  const { t } = useTranslation();
  const messagePanelId = useId();
  const requestSide = side === "request";
  const headers = requestSide ? detail.requestHeaders : detail.responseHeaders;
  const bodyMeta = requestSide ? detail.requestBody : detail.responseBody;
  const [view, setView] = useState<MessageView>(() =>
    initialMessageView(headers, bodyMeta),
  );

  useEffect(() => {
    // 事务选择变化时旧子视图没有协议意义；按新报文类型重新选择可读视图，避免二进制流继承文本展示。
    setView(initialMessageView(headers, bodyMeta));
  }, [bodyMeta, detail.transaction.transactionId, headers]);

  return (
    <div className="messageViewer">
      <div
        className="messageViewTabs"
        role="tablist"
        onKeyDown={activateAdjacentTab}
      >
        {messageTabs.map(({ value, labelKey }) => (
          <button
            aria-controls={messagePanelId}
            aria-selected={view === value}
            className={view === value ? "isActive" : ""}
            id={`${messagePanelId}-${value}-tab`}
            key={value}
            onClick={() => setView(value)}
            role="tab"
            tabIndex={view === value ? 0 : -1}
            type="button"
          >
            {t(labelKey)}
          </button>
        ))}
      </div>
      <div
        aria-labelledby={`${messagePanelId}-${view}-tab`}
        className="messageViewContent"
        id={messagePanelId}
        role="tabpanel"
      >
        {view === "headers" ? (
          <HeadersView
            headers={headers}
            headersTruncated={detail.transaction.flags.headersTruncated}
          />
        ) : (
          <BodyView
            bodyMeta={bodyMeta}
            side={side}
            transactionId={detail.transaction.transactionId}
            view={view}
            sourceUrl={detail.transaction.urlDisplay}
          />
        )}
      </div>
    </div>
  );
}

/**
 * 将请求与响应组合为可调高度的上下消息工作区。
 *
 * 运行上下文：仅在详情已加载时渲染；两个消息面板默认停留在头视图，避免用户未展开正文时读取两份正文。
 * 参数：detail 为当前事务的不可变详情快照。
 * 失败语义：正文读取失败由各自的 MessageViewPanel 显示，不影响另一侧报文检查；任一面板可完整收起但分割条始终可恢复。
 */
function ContentsView({ detail }: { detail: TransactionDetail }) {
  const { t } = useTranslation();
  const contentsRef = useRef<HTMLDivElement>(null);
  const contentsSplitter = useAxisSplitter(contentsRef, {
    axis: "horizontal",
    dividerSize: contentsDividerHeight,
  });
  const requestPanePercentage = Math.round(contentsSplitter.ratio * 100);
  const responsePanePercentage = 100 - requestPanePercentage;

  const contentsStyle = {
    "--contents-divider-height": `${contentsDividerHeight}px`,
    gridTemplateRows: `minmax(0, ${contentsSplitter.ratio}fr) var(--contents-divider-height) minmax(0, ${1 - contentsSplitter.ratio}fr)`,
  } as CSSProperties;

  return (
    <div
      className="transactionContentsView"
      data-resizing={contentsSplitter.resizing || undefined}
      ref={contentsRef}
      style={contentsStyle}
    >
      <section
        aria-label={t("viewer.tabs.request")}
        className="messagePane messagePane--request"
      >
        <header>
          <strong>{t("viewer.tabs.request")}</strong>
          <span>
            {detail.transaction.method} {detail.transaction.urlDisplay}
          </span>
        </header>
        <MessageViewPanel detail={detail} side="request" />
      </section>
      <div
        aria-label={t("viewer.tabs.request")}
        aria-orientation="horizontal"
        aria-valuemax={100}
        aria-valuemin={0}
        aria-valuenow={requestPanePercentage}
        aria-valuetext={`${t("viewer.tabs.request")} ${requestPanePercentage}% / ${t("viewer.tabs.response")} ${responsePanePercentage}%`}
        className="contentsDivider"
        role="separator"
        tabIndex={0}
        onKeyDown={contentsSplitter.handleKeyDown}
        onPointerCancel={contentsSplitter.finishResize}
        onPointerDown={contentsSplitter.beginResize}
        onPointerMove={contentsSplitter.continueResize}
        onPointerUp={contentsSplitter.finishResize}
      >
        <span aria-hidden="true" className="contentsDividerGrip" />
      </div>
      <section
        aria-label={t("viewer.tabs.response")}
        className="messagePane messagePane--response"
      >
        <header>
          <strong>{t("viewer.tabs.response")}</strong>
          <span>
            {detail.transaction.statusCode ??
              t("transactions.table.emptyValue")}
          </span>
        </header>
        <MessageViewPanel detail={detail} side="response" />
      </section>
    </div>
  );
}

/**
 * 渲染二级请求或响应节点的六个检查器页签，并保证每个页签承担不同的数据职责。
 *
 * 运行上下文：连接树选中方向节点时使用；内容读取正文，摘要列出片段，图表展示时序，协议页仅解码同方向数据。
 * 参数：detailState 为详情读取状态，selection 固定事务和方向，view 为当前页签。
 * 失败语义：详情加载或读取失败时显示稳定空状态，不复用上一事务内容。
 */
function StreamDirectionInspector({
  detailState,
  selection,
  view,
}: {
  detailState: DetailState;
  selection: StreamPacketSelection;
  view: InspectorView;
}) {
  const { t } = useTranslation();
  if (detailState.kind === "loading" || detailState.kind === "empty") {
    return <div className="emptyState">{t("viewer.detailLoading")}</div>;
  }
  if (detailState.kind === "error") {
    return <div className="emptyState">{t("viewer.detailFailed")}</div>;
  }
  const bodyMeta =
    selection.side === "request"
      ? detailState.detail.requestBody
      : detailState.detail.responseBody;
  if (view === "contents") {
    return (
      <MessageViewPanel detail={detailState.detail} side={selection.side} />
    );
  }
  if (view === "chart") {
    return (
      <StreamDirectionChart detail={detailState.detail} side={selection.side} />
    );
  }
  if (view === "protocol") {
    return (
      <ProtocolInspector
        responseContentType={
          bodyMeta?.contentType ?? detailState.detail.transaction.contentType
        }
        side={selection.side}
        showResponseValidation={false}
        transactionId={selection.transactionId}
      />
    );
  }
  if (view === "notes") {
    return <NotesView notes={detailState.detail.transaction.notes} />;
  }
  if (view === "summary") {
    return (
      <StreamDirectionSummary
        detail={detailState.detail}
        side={selection.side}
      />
    );
  }
  return (
    <StreamDirectionOverview
      detail={detailState.detail}
      side={selection.side}
    />
  );
}

/**
 * 渲染连接树叶节点对应的单包检查器；它只选择一个方向和一段字节范围，不渲染另一侧正文或事务级摘要。
 *
 * 运行上下文：滚动录制可能在事务仍存在时先淘汰旧包，组件必须与最新详情核对序号。
 * 参数：detailState 是当前事务详情，selection 是原包选择，onPacketUnavailable 把失效选择降级到同方向节点。
 * 失败语义：详情请求失败沿用标准失败状态；包已被淘汰时不显示错误页，直接展示仍有效的方向概览。
 */
function StreamPacketInspector({
  detailState,
  onPacketUnavailable,
  selection,
}: {
  detailState: DetailState;
  onPacketUnavailable(selection: StreamPacketSelection): void;
  selection: StreamPacketSelection;
}) {
  const { t } = useTranslation();
  const readyDetail = detailState.kind === "ready" ? detailState.detail : null;
  const packet =
    readyDetail === null
      ? null
      : ((selection.side === "request"
          ? readyDetail.requestPackets
          : readyDetail.responsePackets
        ).find((candidate) => candidate.sequence === selection.sequence) ??
        null);
  const bodyMeta =
    readyDetail === null
      ? null
      : selection.side === "request"
        ? readyDetail.requestBody
        : readyDetail.responseBody;
  const packetUnavailable =
    readyDetail !== null && (packet === null || bodyMeta === null);

  useEffect(() => {
    if (!packetUnavailable) {
      return;
    }
    // 滚动录制会淘汰旧片段；立即降级到同方向聚合节点，不能让已经失效的包占满检查器形成错误页。
    onPacketUnavailable({
      transactionId: selection.transactionId,
      side: selection.side,
      sequence: null,
    });
  }, [
    onPacketUnavailable,
    packetUnavailable,
    selection.side,
    selection.transactionId,
  ]);

  if (detailState.kind === "loading" || detailState.kind === "empty") {
    return (
      <div className="emptyState">
        <span>{t("viewer.detailLoading")}</span>
      </div>
    );
  }
  if (detailState.kind === "error") {
    return (
      <div className="emptyState">
        <strong>{t("viewer.detailFailed")}</strong>
      </div>
    );
  }
  if (packet === null || bodyMeta === null) {
    return (
      <StreamDirectionOverview
        detail={detailState.detail}
        side={selection.side}
      />
    );
  }
  const directionLabel = selection.side === "request" ? "请求" : "响应";
  return (
    <div className="streamPacketInspector">
      <header>
        <div>
          <strong>{directionLabel}</strong>
          <span>第 {packet.sequence} 包</span>
        </div>
        <span>{formatTransactionBytes(packet.originalBytes)}</span>
      </header>
      <BodyView
        bodyMeta={bodyMeta}
        key={`${selection.transactionId}:${selection.side}:${selection.sequence}`}
        packet={packet}
        side={selection.side}
        sourceUrl=""
        transactionId={selection.transactionId}
        view="hex"
      />
    </div>
  );
}

/**
 * 渲染右侧事务检查器；详情请求由选择标识和请求序号隔离，切换事务后不会串入旧详情。
 *
 * 运行上下文：事务、方向和单包节点共用此检查器，详情始终按当前事务标识异步读取。
 * 参数：transaction 与 selectedPacket 表示当前树选择，onPacketUnavailable 负责回写已经被滚动录制淘汰的包选择。
 * 失败语义：请求失败保留可重试状态；单包失效时由子视图回退到同方向聚合节点。
 */
export function TransactionInspector({
  transaction,
  selectedPacket,
  onPacketUnavailable,
}: TransactionInspectorProps) {
  const { t } = useTranslation();
  const inspectorPanelId = useId();
  const { getProcesses } = useServiceStore();
  const [view, setView] = useState<InspectorView>("overview");
  const [retryVersion, setRetryVersion] = useState(0);
  const [clientProcessPresentation, setClientProcessPresentation] = useState<{
    icon: string | null;
    path: string | null;
  }>({ icon: null, path: null });
  const transactionId = transaction?.transactionId ?? null;
  const detailState = useLiveTransactionDetail({
    enabled: transaction !== null,
    retryVersion,
    revision: transaction === null ? "" : transactionDetailRevision(transaction),
    transactionId,
  });
  const isPacketLeaf =
    selectedPacket !== null && selectedPacket.sequence !== null;

  useEffect(() => {
    const processId = transaction?.clientProcessId;
    if (processId === null || processId === undefined) {
      setClientProcessPresentation({ icon: null, path: null });
      return undefined;
    }
    const abortController = new AbortController();
    void getProcesses(abortController.signal)
      .then((snapshot) => {
        const process = snapshot.processes.find(
          (candidate) => candidate.processId === processId,
        );
        if (process === undefined) {
          setClientProcessPresentation({ icon: null, path: null });
          return;
        }
        setClientProcessPresentation({
          icon:
            snapshot.processIcons[process.executablePath.toLowerCase()] ?? null,
          path: process.executablePath,
        });
      })
      .catch((error: unknown) => {
        if (!isAbortError(error)) {
          setClientProcessPresentation({ icon: null, path: null });
        }
      });
    return () => abortController.abort();
  }, [getProcesses, transaction?.clientProcessId]);

  return (
    <section
      className="transactionInspectorPane"
      aria-label={t("viewer.regionLabel")}
    >
      {!isPacketLeaf && (
        <div
          className="compactTabs transactionInspectorTabs"
          role="tablist"
          onKeyDown={activateAdjacentTab}
        >
          {inspectorTabs.map(({ value, labelKey, icon: TabIcon }) => (
            <button
              aria-controls={inspectorPanelId}
              aria-selected={view === value}
              className={view === value ? "isActive" : ""}
              id={`${inspectorPanelId}-${value}-tab`}
              key={value}
              onClick={() => setView(value)}
              role="tab"
              tabIndex={view === value ? 0 : -1}
              type="button"
            >
              <TabIcon aria-hidden="true" size={13} />
              {t(labelKey)}
            </button>
          ))}
        </div>
      )}
      {transaction === null ? (
        <div
          aria-labelledby={`${inspectorPanelId}-${view}-tab`}
          className="emptyState emptyState--inspector"
          id={inspectorPanelId}
          role="tabpanel"
        >
          <Activity aria-hidden="true" size={24} />
          <strong>{t("viewer.emptyTitle")}</strong>
          <span>{t("viewer.emptyHint")}</span>
        </div>
      ) : (
        <>
          <header className="transactionInspectorHeader">
            <div>
              <strong>
                {selectedPacket === null
                  ? transaction.protocol === "socks"
                    ? presentStreamTransport(transaction)
                    : transaction.method
                  : selectedPacket.sequence === null
                    ? selectedPacket.side === "request"
                      ? "请求"
                      : "响应"
                    : "数据包"}
              </strong>
              <span>{transaction.urlDisplay}</span>
            </div>
            {selectedPacket === null && (
              <div className="transactionInspectorHeaderActions">
                <TransactionRepeatActions transaction={transaction} />
                <span
                  className={`statusBadge statusBadge--${transactionStatusTone(
                    transaction.status,
                  )}`}
                >
                  {presentTransactionStatus(transaction, t)}
                </span>
              </div>
            )}
          </header>
          <div
            aria-labelledby={
              !isPacketLeaf ? `${inspectorPanelId}-${view}-tab` : undefined
            }
            className="transactionInspectorBody"
            id={inspectorPanelId}
            role="tabpanel"
          >
            {isPacketLeaf ? (
              <StreamPacketInspector
                detailState={detailState}
                onPacketUnavailable={onPacketUnavailable}
                selection={selectedPacket!}
              />
            ) : selectedPacket !== null ? (
              <StreamDirectionInspector
                detailState={detailState}
                selection={selectedPacket}
                view={view}
              />
            ) : (
              <>
                {view === "overview" && (
                  <OverviewView
                    clientProcessIcon={clientProcessPresentation.icon}
                    clientProcessPath={clientProcessPresentation.path}
                    transaction={transaction}
                  />
                )}
                {view === "contents" && detailState.kind === "loading" && (
                  <div className="emptyState">
                    <span>{t("viewer.detailLoading")}</span>
                  </div>
                )}
                {view === "contents" && detailState.kind === "error" && (
                  <div className="emptyState">
                    <strong>{t("viewer.detailFailed")}</strong>
                    <button
                      type="button"
                      onClick={() =>
                        setRetryVersion((currentVersion) => currentVersion + 1)
                      }
                    >
                      {t("viewer.retry")}
                    </button>
                  </div>
                )}
                {view === "contents" && detailState.kind === "ready" && (
                  <ContentsView
                    detail={detailState.detail}
                    key={`${transactionId}:contents`}
                  />
                )}
                {view === "summary" && (
                  <SummaryView transaction={transaction} />
                )}
                {view === "chart" && (
                  <TimingChartView transaction={transaction} />
                )}
                {view === "protocol" && detailState.kind === "loading" && (
                  <div className="emptyState">
                    <span>{t("viewer.detailLoading")}</span>
                  </div>
                )}
                {view === "protocol" && detailState.kind === "error" && (
                  <div className="emptyState">
                    <strong>{t("viewer.detailFailed")}</strong>
                    <button
                      type="button"
                      onClick={() =>
                        setRetryVersion((currentVersion) => currentVersion + 1)
                      }
                    >
                      {t("viewer.retry")}
                    </button>
                  </div>
                )}
                {view === "protocol" && detailState.kind === "ready" && (
                  <ProtocolInspector
                    key={`${transactionId}:protocol`}
                    responseContentType={
                      detailState.detail.responseBody?.contentType ??
                      detailState.detail.transaction.contentType
                    }
                    transactionId={detailState.detail.transaction.transactionId}
                  />
                )}
                {view === "notes" && <NotesView notes={transaction.notes} />}
              </>
            )}
          </div>
        </>
      )}
    </section>
  );
}
