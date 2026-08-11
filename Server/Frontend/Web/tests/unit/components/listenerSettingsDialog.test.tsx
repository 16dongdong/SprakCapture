import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { EventMessage } from "@/api/protocol";
import { ListenerSettingsDialog } from "@/components/listenerSettingsDialog";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";

/** 创建已连接事件流替身，使测试只覆盖监听设置窗口的动作语义。 */
function createConnectedEventClient() {
  return {
    start(callbacks: {
      onConnectionState(state: "connected", message: string): void;
      onMessage?(message: EventMessage): void;
    }) {
      callbacks.onConnectionState("connected", "事件流已连接");
    },
    stop() {},
  };
}

describe("辅助监听设置对话框", () => {
  it("只保留取消和应用，应用成功后继续停留在当前窗口", async () => {
    const user = userEvent.setup();
    const snapshot = createServiceSnapshot();
    const onClose = vi.fn();
    const updateReverseProxies = vi.fn(async () => snapshot);
    const { container } = render(
      <ServiceProvider
        controlClient={createControlClientStub(snapshot, {
          updateReverseProxies,
        })}
        eventClient={createConnectedEventClient()}
      >
        <ListenerSettingsDialog open="reverseProxies" onClose={onClose} />
      </ServiceProvider>,
    );

    await screen.findByText("暂无监听规则。");
    expect(container.querySelector(".toolDialogHeader button")).toBeNull();
    expect(container.querySelectorAll(".toolDialogFooter > button")).toHaveLength(2);

    await user.click(screen.getByRole("button", { name: "应用" }));
    await waitFor(() => expect(updateReverseProxies).toHaveBeenCalledOnce());
    expect(onClose).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "取消" }));
    expect(onClose).toHaveBeenCalledOnce();
  });
});
