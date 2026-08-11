import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it } from "vitest";

import {
  PacketFilterByteGrid,
  parsePacketByteClipboard,
} from "@/components/packetFilterByteGrid";

/**
 * 以受控状态承载字节网格，确保复制、粘贴和删除都经过与生产表单相同的状态回写路径。
 * 失败语义：任意无效粘贴保持原值，并通过网格内错误信息向作者反馈。
 */
function GridHarness() {
  const [pattern, setPattern] = useState("01 02 03 04");
  const [replacement, setReplacement] = useState("AA BB CC DD");
  return (
    <>
      <PacketFilterByteGrid
        disabled={false}
        pattern={pattern}
        replacement={replacement}
        onPatternChange={setPattern}
        onReplacementChange={setReplacement}
      />
      <output data-testid="pattern">{pattern}</output>
      <output data-testid="replacement">{replacement}</output>
    </>
  );
}

describe("PacketFilterByteGrid", () => {
  it("兼容 WPE、连续十六进制、C 数组、转义串和转储格式", () => {
    expect(parsePacketByteClipboard("01 02 ?? FF")).toEqual([
      "01",
      "02",
      "??",
      "FF",
    ]);
    expect(parsePacketByteClipboard("0102A0FF")).toEqual([
      "01",
      "02",
      "A0",
      "FF",
    ]);
    expect(parsePacketByteClipboard("{ 0x01, 0x02, 0xFE }")).toEqual([
      "01",
      "02",
      "FE",
    ]);
    expect(parsePacketByteClipboard("\\x01\\x02\\xFF")).toEqual([
      "01",
      "02",
      "FF",
    ]);
    expect(
      parsePacketByteClipboard("00000000: 01 02 03 04  05 06 |......|"),
    ).toEqual(["01", "02", "03", "04", "05", "06"]);
    expect(parsePacketByteClipboard("01 GG")).toBeNull();
    expect(parsePacketByteClipboard("001")).toBeNull();
    expect(parsePacketByteClipboard(Array(512).fill("A5").join(" "))).toHaveLength(
      512,
    );
    expect(parsePacketByteClipboard(Array(513).fill("A5").join(" "))).toBeNull();
  });

  it("支持跨单元格选择、复制、删除与从焦点位置粘贴", () => {
    render(<GridHarness />);
    expect(screen.getAllByRole("row")).toHaveLength(3);
    expect(screen.queryAllByRole("button")).toHaveLength(0);
    expect(screen.getAllByRole("textbox")).toHaveLength(1_024);
    expect(screen.getByLabelText("搜索 00")).toHaveAttribute(
      "maxlength",
      "2",
    );
    expect(screen.getByLabelText("搜索 1FF")).not.toBeDisabled();
    expect(screen.getByLabelText("替换 1FF")).not.toBeDisabled();

    fireEvent.click(screen.getByLabelText("搜索 01"));
    fireEvent.click(screen.getByLabelText("搜索 03"), { shiftKey: true });
    const copied: Record<string, string> = {};
    fireEvent.copy(screen.getByTestId("packet-byte-grid"), {
      clipboardData: {
        setData: (type: string, value: string) => {
          copied[type] = value;
        },
      },
    });
    expect(copied["text/plain"]).toBe("02 03 04");

    fireEvent.keyDown(screen.getByTestId("packet-byte-grid"), {
      key: "Delete",
    });
    expect(screen.getByTestId("pattern")).toHaveTextContent("01");
    expect(screen.getByTestId("replacement")).toHaveTextContent("AA BB CC DD");

    fireEvent.paste(screen.getByLabelText("搜索 01"), {
      clipboardData: { getData: () => "10 20 30" },
    });
    expect(screen.getByTestId("pattern")).toHaveTextContent("01 10 20 30");
    expect(screen.getByTestId("replacement")).toHaveTextContent("AA BB CC DD");
  });

  it("允许直接编辑最后一个偏移并将搜索行限制为 512 字节", () => {
    render(<GridHarness />);

    fireEvent.change(screen.getByLabelText("搜索 1FF"), {
      target: { value: "A5" },
    });
    const pattern = screen.getByTestId("pattern").textContent ?? "";
    const bytes = pattern.split(" ");
    expect(bytes).toHaveLength(512);
    expect(bytes.at(-1)).toBe("A5");
  });

  it("输入完整字节并自动移到下一格时不会被失焦事件覆盖为零", () => {
    render(<GridHarness />);
    const byteCell = screen.getByLabelText("搜索 03");

    fireEvent.change(byteCell, { target: { value: "A" } });
    expect(byteCell).toHaveValue("A");
    fireEvent.change(byteCell, { target: { value: "A5" } });
    fireEvent.blur(byteCell);

    expect(screen.getByLabelText("搜索 03")).toHaveValue("A5");
    expect(screen.getByTestId("pattern")).toHaveTextContent("01 02 03 A5");
    expect(document.activeElement).toBe(screen.getByLabelText("搜索 04"));
  });

  it("替换行以空白显示通配位置并保持后续输入偏移", () => {
    render(<GridHarness />);

    fireEvent.click(screen.getByLabelText("替换 00"));
    fireEvent.paste(screen.getByLabelText("替换 00"), {
      clipboardData: { getData: () => "01 00 06 03 00 03 03" },
    });
    expect(screen.getByTestId("replacement")).toHaveTextContent(
      "01 00 06 03 00 03 03",
    );
    expect(screen.getByLabelText("替换 06")).not.toBeDisabled();

    fireEvent.click(screen.getByLabelText("替换 02"));
    fireEvent.keyDown(screen.getByTestId("packet-byte-grid"), {
      key: "Delete",
    });
    expect(screen.getByTestId("replacement")).toHaveTextContent(
      "01 00 ?? 03 00 03 03",
    );
    expect(screen.getByLabelText("替换 02")).toHaveValue("");
    expect(screen.getByLabelText("替换 03")).toHaveValue("03");

    fireEvent.change(screen.getByLabelText("替换 08"), {
      target: { value: "A" },
    });
    expect(screen.getByLabelText("替换 07")).toHaveValue("");
    expect(screen.getByLabelText("替换 08")).toHaveValue("A");
    fireEvent.change(screen.getByLabelText("替换 08"), {
      target: { value: "A5" },
    });
    expect(screen.getByLabelText("替换 07")).toHaveValue("");
    expect(screen.getByLabelText("替换 08")).toHaveValue("A5");
  });
});
