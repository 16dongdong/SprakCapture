import { describe, expect, it } from "vitest";

import {
  byteSelectionFromText,
  readPaneTextSelection,
  textSelectionFromBytes,
} from "@/components/hexSelection";

describe("十六进制与 ASCII 选择联动", () => {
  it("按字节坐标转换两种不同换行布局", () => {
    expect(
      byteSelectionFromText(
        { startOffset: 3, endOffset: 6 },
        4,
        "ascii",
        8,
      ),
    ).toEqual({ startByte: 3, endByte: 5 });
    expect(
      textSelectionFromBytes(
        { startByte: 3, endByte: 5 },
        4,
        "ascii",
      ),
    ).toEqual({ startOffset: 3, endOffset: 6 });
    expect(
      textSelectionFromBytes(
        { startByte: 3, endByte: 5 },
        8,
        "hex",
      ),
    ).toEqual({ startOffset: 9, endOffset: 14 });
  });

  it("只选择字节分隔符时不误高亮相邻字节", () => {
    expect(
      byteSelectionFromText(
        { startOffset: 2, endOffset: 3 },
        16,
        "hex",
        8,
      ),
    ).toBeNull();
    expect(
      byteSelectionFromText(
        { startOffset: 4, endOffset: 5 },
        4,
        "ascii",
        8,
      ),
    ).toBeNull();
  });

  it("从真实 DOM Selection 读取正向和反向选择的统一偏移", () => {
    const pane = document.createElement("pre");
    pane.textContent = "41 42 43";
    document.body.append(pane);
    const textNode = pane.firstChild;
    expect(textNode).not.toBeNull();
    const selection = window.getSelection();
    expect(selection).not.toBeNull();
    selection?.setBaseAndExtent(textNode!, 5, textNode!, 1);

    expect(readPaneTextSelection(selection!, pane)).toEqual({
      startOffset: 1,
      endOffset: 5,
    });

    selection?.removeAllRanges();
    pane.remove();
  });
});
