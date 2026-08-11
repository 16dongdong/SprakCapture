import {
  type ClipboardEvent,
  type KeyboardEvent,
  type MouseEvent,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import { maximumPacketFilterBytes } from "./packetFilterLimits";

type ByteGridRow = "pattern" | "replacement";

interface ByteSelection {
  row: ByteGridRow;
  anchor: number;
  focus: number;
}

interface PacketFilterByteGridProps {
  disabled: boolean;
  pattern: string;
  replacement: string | null;
  onPatternChange(value: string): void;
  onReplacementChange(value: string): void;
}

/**
 * 按 WPE 网格习惯显示十六进制偏移：首屏使用 00、01、02，超过 FF 后自然扩展为 100–1FF。
 * 偏移是位置标签而非固定宽度地址，禁止补成 0000 造成无意义占宽。
 */
function formatByteOffset(index: number): string {
  return index.toString(16).toUpperCase().padStart(2, "0");
}

/**
 * 将 WPE、连续十六进制、C 数组、`\\xNN` 和常见十六进制转储统一解析为字节单元。
 * 运行上下文：仅处理用户主动粘贴到本地滤镜编辑器的文本；通配符 `??` 保持原义。
 * 失败语义：任意非分隔残留、奇数半字节或超过 512 字节时返回 null，调用方不会覆盖现有草稿。
 */
export function parsePacketByteClipboard(text: string): string[] | null {
  const preparedLines = text
    .replaceAll("\r", "")
    .split("\n")
    .map((line) => {
      const withoutAscii = line.includes("|")
        ? line.slice(0, line.indexOf("|"))
        : line;
      const withoutOffset = withoutAscii.replace(
        /^\s*(?:0x)?[0-9A-Fa-f]{4,16}(?::\s*|\s{2,})/,
        "",
      );
      return withoutOffset;
    });
  const compact = preparedLines
    .join(" ")
    .replace(/(?:0x|\\x)/gi, "")
    .replace(/[\s,;:{}\[\]()_-]+/g, "");
  if (compact === "" || !/^(?:[0-9A-Fa-f]{2}|\?\?)+$/.test(compact)) {
    return null;
  }
  const bytes = compact
    .match(/[0-9A-Fa-f]{2}|\?\?/g)
    ?.map((byte) => byte.toUpperCase());
  return bytes !== undefined && bytes.length <= maximumPacketFilterBytes
    ? bytes
    : null;
}

/**
 * 把配置字符串拆为网格单元；持久化契约只允许空格分隔，因此这里不承担粘贴格式兼容。
 * 失败语义：草稿中的半字节仍原样显示，最终是否可提交由 isPacketByteGridValueValid 判断。
 */
function splitGridValue(value: string): string[] {
  const trimmed = value.trim();
  return trimmed === "" ? [] : trimmed.split(/\s+/);
}

/**
 * 将后端通配占位转换为编辑器空格；界面只呈现作者输入，不暴露持久化协议的 `??`。
 * 运行上下文：搜索与替换共用该视图转换，空格在回写时仍恢复为通配占位以保持原偏移。
 */
function toEditableGridCells(value: string): string[] {
  return splitGridValue(value).map((cell) => (cell === "??" ? "" : cell));
}

/**
 * 将稀疏网格转换为持久化字节串；内部空洞统一写为通配占位，尾部空格不参与配置。
 * 运行上下文：每次单元格编辑都会调用；替换行空洞必须保留原字节位置，不能压缩后续输入。
 */
function serializeGridCells(cells: readonly string[]): string {
  let lastValueIndex = cells.length - 1;
  while (lastValueIndex >= 0 && cells[lastValueIndex] === "") {
    lastValueIndex -= 1;
  }
  const persistedCells = cells.slice(0, lastValueIndex + 1);
  return persistedCells.map((cell) => cell || "??").join(" ");
}

/**
 * 校验网格草稿是否能直接写入封包滤镜协议；单个半字节在编辑中允许存在，但会阻止提交。
 */
export function isPacketByteGridValueValid(value: string): boolean {
  const cells = splitGridValue(value);
  return (
    cells.length <= maximumPacketFilterBytes &&
    cells.every((cell) => /^(?:[0-9A-Fa-f]{2}|\?\?)$/.test(cell))
  );
}

/** 将选择范围归一为闭区间，避免反向 Shift 选择在复制、剪切时产生不同语义。 */
function selectionBounds(selection: ByteSelection): [number, number] {
  return [
    Math.min(selection.anchor, selection.focus),
    Math.max(selection.anchor, selection.focus),
  ];
}

/**
 * 渲染与 WPE 普通滤镜一致的 0000–01FF 横向双行字节网格；搜索偏移与替换输出分别独立编辑。
 * 键盘支持 Ctrl+C/X/V、Delete、方向键和 Shift 连选；超出窗口宽度时只横向滚动，不拆成额外行。
 * 所有编辑仍输出既有空格分隔配置，不改变后端热更新与持久化协议。
 */
export function PacketFilterByteGrid({
  disabled,
  pattern,
  replacement,
  onPatternChange,
  onReplacementChange,
}: PacketFilterByteGridProps) {
  const { t } = useTranslation();
  const inputRefs = useRef(new Map<string, HTMLInputElement>());
  const pointerFocusRef = useRef(false);
  const [selection, setSelection] = useState<ByteSelection>({
    row: "pattern",
    anchor: 0,
    focus: 0,
  });
  const [clipboardError, setClipboardError] = useState<string | null>(null);
  const patternCells = useMemo(() => toEditableGridCells(pattern), [pattern]);
  const replacementCells = useMemo(
    () => toEditableGridCells(replacement ?? ""),
    [replacement],
  );
  // 固定渲染全部 512 个偏移，作者无需先填满前一段即可直接跳到任意目标位置。
  const visibleCellCount = maximumPacketFilterBytes;

  /** 根据行标识读取当前单元格数组；替换行仅在修改动作可见时存在。 */
  const cellsForRow = (row: ByteGridRow): string[] =>
    row === "pattern" ? patternCells : replacementCells;

  /** 提交一行网格；两行互不改写，内部空格以隐藏通配占位保留偏移。 */
  const commitRow = (row: ByteGridRow, cells: string[]) => {
    const value = serializeGridCells(cells);
    if (row === "pattern") {
      onPatternChange(value);
      return;
    }
    onReplacementChange(value);
  };

  /**
   * 更新单个网格单元；只接受半字节编辑态、完整字节或通配符。
   * 半字节必须保留到作者继续输入或主动修正，不能在自动换格触发的 blur 中补零；否则 blur 闭包会用旧草稿覆盖刚提交的完整字节。
   */
  const updateCell = (row: ByteGridRow, index: number, rawValue: string) => {
    const normalized = rawValue.trim().toUpperCase();
    if (!/^(?:[0-9A-F]{0,2}|\?{0,2})$/.test(normalized)) {
      return;
    }
    const cells = [...cellsForRow(row)];
    while (cells.length <= index) {
      cells.push("");
    }
    cells[index] = normalized;
    commitRow(row, cells);
    if (normalized.length === 2 && index + 1 < visibleCellCount) {
      inputRefs.current.get(`${row}:${index + 1}`)?.focus();
      setSelection({ row, anchor: index + 1, focus: index + 1 });
    }
  };

  /** 将焦点落到指定单元格，并根据 Shift 状态延伸或重建选择。 */
  const selectCell = (
    row: ByteGridRow,
    index: number,
    extendSelection: boolean,
  ) => {
    setSelection((current) =>
      extendSelection && current.row === row
        ? { ...current, focus: index }
        : { row, anchor: index, focus: index },
    );
  };

  /** 清除当前闭区间；内部空洞成为隐藏通配条件，后续字节保持原偏移不前移。 */
  const clearSelection = () => {
    const [start, end] = selectionBounds(selection);
    const cells = [...cellsForRow(selection.row)];
    for (let index = start; index <= end && index < cells.length; index += 1) {
      cells[index] = "";
    }
    commitRow(selection.row, cells);
    inputRefs.current.get(`${selection.row}:${start}`)?.focus();
    setSelection({ row: selection.row, anchor: start, focus: start });
  };

  /** 读取当前选择的规范化 WPE 文本；空选择不会写入剪贴板。 */
  const selectedText = (): string => {
    const [start, end] = selectionBounds(selection);
    return serializeGridCells(
      cellsForRow(selection.row).slice(start, end + 1),
    );
  };

  /** 从系统剪贴板读取多种十六进制格式并从当前单元格开始连续写入。 */
  const pasteText = (text: string) => {
    const pastedCells = parsePacketByteClipboard(text);
    if (pastedCells === null) {
      setClipboardError(t("tools.packetFilters.clipboardInvalid"));
      return;
    }
    const [start] = selectionBounds(selection);
    const rowLimit = maximumPacketFilterBytes;
    const writableCount = Math.min(pastedCells.length, rowLimit - start);
    if (writableCount <= 0) {
      setClipboardError(t("tools.packetFilters.clipboardInvalid"));
      return;
    }
    const cells = [...cellsForRow(selection.row)];
    while (cells.length < start + writableCount) {
      cells.push("");
    }
    cells.splice(start, writableCount, ...pastedCells.slice(0, writableCount));
    commitRow(selection.row, cells);
    const end = start + writableCount - 1;
    setSelection({ row: selection.row, anchor: start, focus: end });
    inputRefs.current.get(`${selection.row}:${end}`)?.focus();
    setClipboardError(null);
  };

  /** 截获网格级复制事件，使跨单元格选择按 WPE 空格格式写入。 */
  const handleCopy = (event: ClipboardEvent<HTMLDivElement>) => {
    const text = selectedText();
    if (text === "") {
      return;
    }
    event.preventDefault();
    event.clipboardData.setData("text/plain", text);
    setClipboardError(null);
  };

  /** 截获网格级剪切事件；写入剪贴板成功后再清除，避免内容无处恢复。 */
  const handleCut = (event: ClipboardEvent<HTMLDivElement>) => {
    if (disabled) {
      return;
    }
    handleCopy(event);
    clearSelection();
  };

  /** 截获原生粘贴事件并交给统一格式解析器，保证多单元格粘贴不会被浏览器压入一个 input。 */
  const handlePaste = (event: ClipboardEvent<HTMLDivElement>) => {
    if (disabled) {
      return;
    }
    event.preventDefault();
    pasteText(event.clipboardData.getData("text/plain"));
  };

  /** 处理删除和方向导航；普通字符输入仍交给每个原生 input 完成。 */
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Delete" && !disabled) {
      event.preventDefault();
      clearSelection();
      return;
    }
    const direction =
      event.key === "ArrowLeft" ? -1 : event.key === "ArrowRight" ? 1 : 0;
    if (direction === 0) {
      return;
    }
    const nextIndex = Math.max(
      0,
      Math.min(visibleCellCount - 1, selection.focus + direction),
    );
    event.preventDefault();
    inputRefs.current.get(`${selection.row}:${nextIndex}`)?.focus();
    selectCell(selection.row, nextIndex, event.shiftKey);
  };

  /** 渲染一行十六进制单元格；替换行始终可编辑，以支持独立长度的输出序列。 */
  const renderByteRow = (row: ByteGridRow) => {
    const cells = cellsForRow(row);
    const [selectionStart, selectionEnd] = selectionBounds(selection);
    const rowLabel = t(
      row === "pattern"
        ? "tools.packetFilters.searchRow"
        : "tools.packetFilters.replacementRow",
    );
    return (
      <tr key={row}>
        <th scope="row">{rowLabel}</th>
        {Array.from({ length: visibleCellCount }, (_unused, index) => {
          const selected =
            selection.row === row &&
            index >= selectionStart &&
            index <= selectionEnd;
          return (
            <td
              className={selected ? "packetByteCell--selected" : undefined}
              key={index}
            >
              <input
                ref={(element) => {
                  const key = `${row}:${index}`;
                  if (element === null) {
                    inputRefs.current.delete(key);
                  } else {
                    inputRefs.current.set(key, element);
                  }
                }}
                aria-label={`${rowLabel} ${formatByteOffset(index)}`}
                disabled={disabled}
                inputMode="text"
                maxLength={2}
                spellCheck={false}
                value={cells[index] ?? ""}
                onChange={(event) => updateCell(row, index, event.target.value)}
                onClick={(event: MouseEvent<HTMLInputElement>) => {
                  selectCell(row, index, event.shiftKey);
                }}
                onFocus={() => {
                  if (!pointerFocusRef.current) {
                    selectCell(row, index, false);
                  }
                }}
                onMouseDown={() => {
                  pointerFocusRef.current = true;
                  queueMicrotask(() => {
                    pointerFocusRef.current = false;
                  });
                }}
              />
            </td>
          );
        })}
      </tr>
    );
  };

  return (
    <div
      className="packetByteEditor"
      data-testid="packet-byte-grid"
      onCopy={handleCopy}
      onCut={handleCut}
      onKeyDown={handleKeyDown}
      onPaste={handlePaste}
    >
      <small className="packetByteHint">
        {t("tools.packetFilters.gridHint")}
      </small>
      <div className="packetByteGridViewport">
        <table className="packetByteGrid">
          <thead>
            <tr>
              <th scope="col">{t("tools.packetFilters.byteOffset")}</th>
              {Array.from({ length: visibleCellCount }, (_unused, index) => (
                <th scope="col" key={index}>
                  {formatByteOffset(index)}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {renderByteRow("pattern")}
            {replacement === null ? null : renderByteRow("replacement")}
          </tbody>
        </table>
      </div>
      {clipboardError !== null && (
        <p className="packetByteClipboardError" role="alert">
          {clipboardError}
        </p>
      )}
    </div>
  );
}
