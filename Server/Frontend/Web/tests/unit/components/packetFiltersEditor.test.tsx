import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it } from "vitest";

import type { PacketFilterConfiguration } from "@/api/protocol";
import { PacketFiltersEditor } from "@/components/packetFiltersEditor";

/**
 * 以真实状态承载滤镜编辑器，验证二级窗口提交的是完整可持久化对象。
 * 失败语义：组件没有发出变更时 output 保持原配置，让测试直接暴露编辑或提交断链。
 */
function EditorHarness() {
  const [configuration, setConfiguration] = useState<PacketFilterConfiguration>(
    { enabled: true, rules: [] },
  );
  return (
    <>
      <PacketFiltersEditor
        configuration={configuration}
        disabled={false}
        onChange={setConfiguration}
      />
      <output data-testid="configuration">
        {JSON.stringify(configuration)}
      </output>
    </>
  );
}

describe("PacketFiltersEditor", () => {
  it(
    "通过字节网格提交稀疏搜索和独立长度替换规则",
    () => {
      render(<EditorHarness />);
      fireEvent.click(screen.getByRole("button", { name: "添加滤镜" }));
      expect(
        Array.from(
          screen.getByLabelText("执行动作").querySelectorAll("option"),
        ).map((option) => option.textContent),
      ).toEqual(["替换", "丢弃", "关闭连接"]);
      fireEvent.click(screen.getByLabelText("替换当前块内全部不重叠命中"));
      fireEvent.click(screen.getByLabelText("命中后继续执行后续规则"));
      expect(
        screen.getByLabelText("替换当前块内全部不重叠命中"),
      ).toBeChecked();
      expect(screen.getByLabelText("命中后继续执行后续规则")).toBeChecked();
      fireEvent.change(screen.getByLabelText("规则名称"), {
        target: { value: "替换握手标记" },
      });
      fireEvent.change(screen.getByLabelText("目标主机"), {
        target: { value: "*.example.com" },
      });
      fireEvent.paste(screen.getByLabelText("搜索 0000"), {
        clipboardData: { getData: () => "01 00 ?? 03 00" },
      });
      fireEvent.change(screen.getByLabelText("执行动作"), {
        target: { value: "modify" },
      });
      fireEvent.click(screen.getByLabelText("替换 0000"));
      fireEvent.paste(screen.getByLabelText("替换 0000"), {
        clipboardData: { getData: () => "01 00 06 03 00 03 03" },
      });
      fireEvent.click(screen.getByRole("button", { name: "应用" }));

      const configuration = JSON.parse(
        screen.getByTestId("configuration").textContent ?? "{}",
      ) as PacketFilterConfiguration;
      expect(configuration.rules).toHaveLength(1);
      expect(configuration.rules[0]).toMatchObject({
        name: "替换握手标记",
        host: "*.example.com",
        pattern: "01 00 ?? 03 00",
        replacement: "01 00 06 03 00 03 03",
        action: "modify",
        replaceAll: true,
        continueMatching: true,
      });
    },
    // 该用例刻意渲染搜索与替换各 512 个真实输入框，jsdom 的全量无障碍查询需要独立预算。
    15_000,
  );
});
