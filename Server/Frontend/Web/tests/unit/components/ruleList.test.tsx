import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { moveListItem, RuleList } from "@/components/ruleList";

describe("规则列表", () => {
  /** 顺序调整必须产生新数组且保留未移动项，规则匹配优先级不能依赖删除重建。 */
  it("移动指定规则而不修改原数组", () => {
    const original = ["第一条", "第二条", "第三条"];

    expect(moveListItem(original, 0, 2)).toEqual(["第二条", "第三条", "第一条"]);
    expect(original).toEqual(["第一条", "第二条", "第三条"]);
  });

  /** 上下移动按钮具有序号化名称和边界禁用态，屏幕阅读器与鼠标均能准确调整优先级。 */
  it("通过可访问按钮发出规则重排请求", async () => {
    const user = userEvent.setup();
    const onMove = vi.fn();
    render(
      <RuleList
        addLabel="添加"
        disabled={false}
        emptyHint="暂无"
        itemLabel={(_, item) => item}
        items={["第一条", "第二条"]}
        moveDownLabel="下移"
        moveUpLabel="上移"
        removeLabel="删除"
        selectedIndex={0}
        title="规则"
        onAdd={() => undefined}
        onMove={onMove}
        onRemove={() => undefined}
        onSelect={() => undefined}
      />,
    );

    expect(screen.getByRole("button", { name: "上移 1" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "下移 1" }));
    expect(onMove).toHaveBeenCalledWith(0, 1);
  });
});
