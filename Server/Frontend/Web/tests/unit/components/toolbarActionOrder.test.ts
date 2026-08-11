import { beforeEach, describe, expect, it } from "vitest";

import {
  defaultToolbarActionOrder,
  moveToolbarAction,
  moveToolbarActionToIndex,
  readToolbarActionOrder,
  reorderToolbarAction,
} from "@/components/toolbarActionOrder";

describe("工具栏动作顺序", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it("拒绝损坏或缺失动作的偏好并恢复完整默认顺序", () => {
    window.localStorage.setItem(
      "capture.toolbar.actionOrder",
      '["recording","recording"]',
    );

    expect(readToolbarActionOrder()).toEqual(defaultToolbarActionOrder);
    expect(
      window.localStorage.getItem("capture.toolbar.actionOrder"),
    ).toBeNull();
  });

  it("拖放与键盘移动均保持动作集合完整且顺序确定", () => {
    const draggedOrder = reorderToolbarAction(
      defaultToolbarActionOrder,
      "tools",
      "recording",
      false,
    );
    expect(draggedOrder).toEqual([
      "tools",
      "recording",
      "clear",
      "refresh",
      "breakpoints",
      "throttling",
      "processes",
      "settings",
    ]);
    expect(moveToolbarAction(draggedOrder, "tools", 1)).toEqual([
      "recording",
      "tools",
      "clear",
      "refresh",
      "breakpoints",
      "throttling",
      "processes",
      "settings",
    ]);
    expect(
      moveToolbarActionToIndex(defaultToolbarActionOrder, "settings", 0),
    ).toEqual([
      "settings",
      "recording",
      "clear",
      "refresh",
      "breakpoints",
      "throttling",
      "tools",
      "processes",
    ]);
  });
});
