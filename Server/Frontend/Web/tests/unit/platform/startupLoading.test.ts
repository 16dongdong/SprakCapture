import { describe, expect, it } from "vitest";

import { dismissStartupLoading } from "@/platform/startupLoading";

describe("启动加载层", () => {
  it("首次业务界面提交后淡出并在过渡结束时移除", () => {
    document.body.innerHTML = '<div id="startupLoading"></div>';
    const startupLoading = document.getElementById("startupLoading");
    expect(startupLoading).not.toBeNull();

    dismissStartupLoading();
    dismissStartupLoading();

    expect(startupLoading).toHaveClass("isReady");
    expect(startupLoading).toHaveAttribute("data-state", "leaving");
    startupLoading?.dispatchEvent(new Event("transitionend"));
    expect(document.getElementById("startupLoading")).toBeNull();
  });

  it("启动层缺失时保持幂等", () => {
    document.body.replaceChildren();
    expect(() => dismissStartupLoading()).not.toThrow();
  });
});
