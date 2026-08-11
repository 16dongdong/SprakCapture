import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it } from "vitest";

import type { RecordingRuleConfiguration } from "@/api/protocol";
import { RecordingRulesEditor } from "@/components/recordingRulesEditor";

/** 以真实 React 状态承载编辑器，确保添加窗口与规则排序回写同一配置对象。 */
function EditorHarness() {
  const [configuration, setConfiguration] =
    useState<RecordingRuleConfiguration>({
      enabled: true,
      defaultAction: "record",
      ruleSets: [],
    });
  return (
    <>
      <RecordingRulesEditor
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

describe("RecordingRulesEditor", () => {
  it("通过独立窗口添加可持久化规则并保留动作", () => {
    render(<EditorHarness />);
    fireEvent.click(screen.getByRole("button", { name: "添加规则集" }));
    fireEvent.click(screen.getByRole("button", { name: "添加规则" }));

    fireEvent.change(screen.getByLabelText("匹配条件"), {
      target: { value: "domainSuffix" },
    });
    fireEvent.change(screen.getByLabelText("匹配值"), {
      target: { value: "music.163.com" },
    });
    fireEvent.change(screen.getByLabelText("执行动作"), {
      target: { value: "reject" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存规则" }));

    const configuration = JSON.parse(
      screen.getByTestId("configuration").textContent ?? "{}",
    ) as RecordingRuleConfiguration;
    expect(configuration.ruleSets).toHaveLength(1);
    expect(configuration.ruleSets[0]?.rules[0]).toMatchObject({
      kind: "domainSuffix",
      value: "music.163.com",
      action: "reject",
    });
  });
});
