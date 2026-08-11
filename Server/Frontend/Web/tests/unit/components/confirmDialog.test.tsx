import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { ConfirmDialog } from "@/components/confirmDialog";

/**
 * 提供真实打开和关闭生命周期，验证对话框把焦点恢复到触发按钮。
 */
function ConfirmDialogHarness({ onConfirm }: { onConfirm(): void }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        清空
      </button>
      <ConfirmDialog
        cancelLabel="取消"
        confirmLabel="确认"
        message="确认清空事务？"
        open={open}
        title="清空事务"
        onCancel={() => setOpen(false)}
        onConfirm={onConfirm}
      />
    </>
  );
}

describe("确认对话框", () => {
  it("默认聚焦取消、循环焦点并在关闭后恢复触发器", async () => {
    const user = userEvent.setup();
    render(<ConfirmDialogHarness onConfirm={vi.fn()} />);
    const trigger = screen.getByRole("button", { name: "清空" });

    await user.click(trigger);
    const cancel = screen.getByRole("button", { name: "取消" });
    const confirm = screen.getByRole("button", { name: "确认" });
    expect(cancel).toHaveFocus();

    await user.tab({ shift: true });
    expect(confirm).toHaveFocus();
    await user.tab();
    expect(cancel).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("忙碌时阻止 Tab 离开模态框", () => {
    render(
      <ConfirmDialog
        busy
        cancelLabel="取消"
        confirmLabel="确认"
        message="正在执行"
        open
        title="清空事务"
        onCancel={vi.fn()}
        onConfirm={vi.fn()}
      />,
    );
    expect(
      fireEvent.keyDown(screen.getByRole("dialog"), { key: "Tab" }),
    ).toBe(false);
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveAttribute("aria-busy", "true");
    expect(dialog).toHaveAttribute("data-state", "busy");
    expect(dialog.querySelector(".confirmDialogHeader")).not.toBeNull();
    expect(dialog.querySelector(".confirmDialogBody")).not.toBeNull();
    expect(dialog.querySelector(".confirmDialogActions")).not.toBeNull();
  });
});
