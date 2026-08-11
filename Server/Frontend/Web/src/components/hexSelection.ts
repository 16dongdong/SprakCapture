/** 描述十六进制查看器中可参与联动选择的文本表示。 */
export type HexPaneRepresentation = "ascii" | "hex";

/** 描述半开字节区间；startByte 包含，endByte 不包含。 */
export interface ByteSelectionRange {
  startByte: number;
  endByte: number;
}

/** 描述单个文本面板内的半开字符区间。 */
export interface TextSelectionRange {
  startOffset: number;
  endOffset: number;
}

/**
 * 计算字符偏移所覆盖的第一个字节；分隔空格或换行本身不对应任何字节。
 *
 * 运行上下文：浏览器 Selection 提供的是面板文本偏移，而检查器联动必须转换为原始正文的字节坐标。
 * 参数：offset 为选择起点；bytesPerLine 为当前响应式列数；representation 指定面板格式。
 * 失败语义：偏移落在行分隔符时返回下一个字节位置，调用方最终可得到空区间。
 */
function firstCoveredByte(
  offset: number,
  bytesPerLine: number,
  representation: HexPaneRepresentation,
): number {
  if (representation === "hex") {
    const byteIndex = Math.floor(offset / 3);
    return offset % 3 >= 2 ? byteIndex + 1 : byteIndex;
  }
  const textColumnsPerLine = bytesPerLine + 1;
  const lineIndex = Math.floor(offset / textColumnsPerLine);
  const columnIndex = offset % textColumnsPerLine;
  return columnIndex >= bytesPerLine
    ? (lineIndex + 1) * bytesPerLine
    : lineIndex * bytesPerLine + columnIndex;
}

/**
 * 计算选择终点之前最后覆盖字节的后继位置；仅覆盖分隔符时与起点计算收敛为空区间。
 *
 * 参数：offset 为半开选择终点，其余参数与 firstCoveredByte 一致。
 * 失败语义：零偏移返回零，禁止通过负索引读取上一个字符。
 */
function endCoveredByte(
  offset: number,
  bytesPerLine: number,
  representation: HexPaneRepresentation,
): number {
  if (offset <= 0) {
    return 0;
  }
  if (representation === "hex") {
    return Math.ceil(offset / 3);
  }
  const textColumnsPerLine = bytesPerLine + 1;
  const lastSelectedOffset = offset - 1;
  const lineIndex = Math.floor(lastSelectedOffset / textColumnsPerLine);
  const columnIndex = lastSelectedOffset % textColumnsPerLine;
  return columnIndex >= bytesPerLine
    ? (lineIndex + 1) * bytesPerLine
    : lineIndex * bytesPerLine + columnIndex + 1;
}

/**
 * 将面板字符选择转换为正文的稳定字节区间。
 *
 * 运行上下文：十六进制每字节占两个字符并带一个分隔符，ASCII 每字节占一个字符并按行插入换行。
 * 参数：selection 是归一化字符区间；totalBytes 用于限制不完整末行和异常浏览器偏移。
 * 失败语义：折叠选择或只覆盖分隔符时返回 null，不产生误导性的联动高亮。
 */
export function byteSelectionFromText(
  selection: TextSelectionRange,
  bytesPerLine: number,
  representation: HexPaneRepresentation,
  totalBytes: number,
): ByteSelectionRange | null {
  if (
    bytesPerLine <= 0 ||
    totalBytes <= 0 ||
    selection.endOffset <= selection.startOffset
  ) {
    return null;
  }
  const startByte = Math.min(
    totalBytes,
    Math.max(
      0,
      firstCoveredByte(
        selection.startOffset,
        bytesPerLine,
        representation,
      ),
    ),
  );
  const endByte = Math.min(
    totalBytes,
    Math.max(
      startByte,
      endCoveredByte(selection.endOffset, bytesPerLine, representation),
    ),
  );
  return endByte === startByte ? null : { startByte, endByte };
}

/**
 * 将正文的字节区间映射回指定面板文本；目标区间同时覆盖内部空格或换行，保持连续高亮。
 *
 * 参数：selection 是半开字节区间；bytesPerLine 与目标面板当前布局一致。
 * 失败语义：空字节区间返回 null，调用方继续渲染普通文本。
 */
export function textSelectionFromBytes(
  selection: ByteSelectionRange,
  bytesPerLine: number,
  representation: HexPaneRepresentation,
): TextSelectionRange | null {
  if (bytesPerLine <= 0 || selection.endByte <= selection.startByte) {
    return null;
  }
  if (representation === "hex") {
    return {
      startOffset: selection.startByte * 3,
      endOffset: (selection.endByte - 1) * 3 + 2,
    };
  }
  const startOffset =
    selection.startByte + Math.floor(selection.startByte / bytesPerLine);
  const lastByte = selection.endByte - 1;
  return {
    startOffset,
    endOffset: lastByte + Math.floor(lastByte / bytesPerLine) + 1,
  };
}

/**
 * 读取浏览器 Selection 在指定面板中的归一化字符偏移。
 *
 * 运行上下文：面板正文保持为普通文本节点，使原生鼠标和键盘选择行为不被自定义字节控件破坏。
 * 参数：selection 是当前文档选择；pane 是十六进制或 ASCII 的 pre 元素。
 * 失败语义：跨面板、折叠或不属于该面板的选择返回 null。
 */
export function readPaneTextSelection(
  selection: Selection,
  pane: HTMLPreElement,
): TextSelectionRange | null {
  const anchorNode = selection.anchorNode;
  const focusNode = selection.focusNode;
  if (
    selection.isCollapsed ||
    anchorNode === null ||
    focusNode === null ||
    !pane.contains(anchorNode) ||
    !pane.contains(focusNode)
  ) {
    return null;
  }

  /** 计算从面板起点到 Selection 端点的可见字符数量，兼容文本节点和标记节点边界。 */
  const measureOffset = (node: Node, offset: number): number => {
    const range = document.createRange();
    range.selectNodeContents(pane);
    range.setEnd(node, offset);
    return range.toString().length;
  };
  const anchorOffset = measureOffset(anchorNode, selection.anchorOffset);
  const focusOffset = measureOffset(focusNode, selection.focusOffset);
  return {
    startOffset: Math.min(anchorOffset, focusOffset),
    endOffset: Math.max(anchorOffset, focusOffset),
  };
}
