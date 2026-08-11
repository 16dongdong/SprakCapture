import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { StableLabel } from "@/components/stableLabel";

describe("稳定状态文案", () => {
  it("保留全部候选文案的测量层而只显示当前值", () => {
    const { container } = render(
      <StableLabel
        candidates={["开始录制", "暂停录制", "正在更新录制状态"]}
        value="暂停录制"
      />,
    );

    const measurement = container.querySelector(".stableLabelMeasure");
    const visibleValue = container.querySelector(".stableLabelValue");
    const measureSpans = [
      ...container.querySelectorAll(".stableLabelMeasure > span"),
    ];

    expect(measurement).toHaveAttribute("aria-hidden", "true");
    expect(measureSpans.map((element) => element.getAttribute("data-text"))).toEqual([
      "开始录制",
      "暂停录制",
      "正在更新录制状态",
    ]);
    // 候选不得进入 textContent，避免按钮名叠字
    expect(measureSpans.every((element) => element.textContent === "")).toBe(
      true,
    );
    expect(container.textContent).toBe("暂停录制");
    expect(visibleValue).toHaveTextContent("暂停录制");
  });
});
