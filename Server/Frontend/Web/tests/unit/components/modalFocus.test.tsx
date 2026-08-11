import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef } from "react";
import { describe, expect, it } from "vitest";

import { useModalFocus } from "@/components/modalFocus";

interface FocusFixtureProps {
  open: boolean;
  revision: number;
  onClose(): void;
}

/** 渲染带三个可聚焦控件的最小模态样本，覆盖共用焦点层而不耦合具体工具表单。 */
function FocusFixture({ open, revision, onClose }: FocusFixtureProps) {
  const containerRef = useRef<HTMLElement>(null);
  const initialFocusRef = useRef<HTMLButtonElement>(null);
  const { onKeyDown } = useModalFocus({
    containerRef,
    initialFocusRef,
    open,
  });

  if (!open) {
    return null;
  }
  return (
    <section
      aria-label="焦点测试对话框"
      ref={containerRef}
      role="dialog"
      tabIndex={-1}
      onKeyDown={onKeyDown}
    >
      <button ref={initialFocusRef} type="button">
        关闭
      </button>
      <input aria-label="名称" value={`草稿 ${revision}`} onChange={() => undefined} />
      <button type="button" onClick={onClose}>
        确定
      </button>
    </section>
  );
}

describe("模态焦点层", () => {
  /** 初始焦点、Tab 环和关闭后的恢复都由同一 Hook 管理，配置刷新不会抢占当前字段焦点。 */
  it("在模态内循环焦点并在关闭后恢复打开按钮", async () => {
    const user = userEvent.setup();
    const renderFixture = (open: boolean, revision: number) => (
      <>
        <button type="button">打开</button>
        <FocusFixture open={open} revision={revision} onClose={() => undefined} />
      </>
    );
    const view = render(renderFixture(false, 1));
    const openButton = screen.getByRole("button", { name: "打开" });
    openButton.focus();
    view.rerender(renderFixture(true, 1));
    const closeButton = screen.getByRole("button", { name: "关闭" });
    await waitFor(() => expect(closeButton).toHaveFocus());

    await user.tab({ shift: true });
    expect(screen.getByRole("button", { name: "确定" })).toHaveFocus();
    await user.tab();
    expect(closeButton).toHaveFocus();
    await user.tab();
    expect(screen.getByRole("textbox", { name: "名称" })).toHaveFocus();

    view.rerender(renderFixture(true, 2));
    expect(screen.getByRole("textbox", { name: "名称" })).toHaveFocus();

    view.rerender(renderFixture(false, 2));
    await waitFor(() => expect(openButton).toHaveFocus());
  });
});
