import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
  createTransactionSummary,
} from "#tests/testFixtures";
import { ConnectionStatusBar } from "@/components/connectionStatusBar";
import { TopToolbar } from "@/components/topToolbar";

/**
 * 创建已连接事件流；录制控件测试只观察控制请求返回的权威快照。
 */
function createConnectedEventClient(): EventStreamClient {
  return {
    start(callbacks: EventClientCallbacks) {
      callbacks.onConnectionState("connected", "事件流已连接");
    },
    stop() {},
  };
}

describe("录制工具栏与状态栏", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it("按使用频率排列工具栏并持久化键盘自定义顺序", async () => {
    const snapshot = createServiceSnapshot();
    const { container, unmount } = render(
      <MemoryRouter initialEntries={["/connections"]}>
        <ServiceProvider
          controlClient={createControlClientStub(snapshot)}
          eventClient={createConnectedEventClient()}
        >
          <TopToolbar
            onOpenSslSettings={() => undefined}
            onOpenProtocolSettings={() => undefined}
            onOpenListenerSettings={() => undefined}
            onOpenToolSettings={() => undefined}
          />
        </ServiceProvider>
      </MemoryRouter>,
    );

    await screen.findByRole("toolbar", { name: "可自定义工具栏" });
    expect(
      screen
        .getByRole("link", { name: "连接会话" })
        .querySelector('[data-toolbar-icon="connectionsActive"]'),
    ).not.toBeNull();
    expect(
      screen
        .getByRole("link", { name: "概览" })
        .querySelector('[data-toolbar-icon="overviewInactive"]'),
    ).not.toBeNull();
    expect(
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-toolbar-action]"),
      ).map((element) => element.dataset.toolbarAction),
    ).toEqual([
      "recording",
      "clear",
      "refresh",
      "breakpoints",
      "throttling",
      "tools",
      "processes",
      "settings",
    ]);
    expect(
      screen
        .getByRole("button", { name: "设置" })
        .querySelector(".lucide-settings"),
    ).not.toBeNull();
    expect(
      screen
        .getByRole("button", { name: "调整工具栏顺序" })
        .querySelector('[data-toolbar-icon="reorderOff"]'),
    ).not.toBeNull();

    const toolsButton = screen.getByRole("button", { name: "工具" });
    fireEvent.keyDown(toolsButton, {
      altKey: true,
      key: "ArrowLeft",
    });
    expect(
      window.localStorage.getItem("capture.toolbar.actionOrder"),
    ).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "调整工具栏顺序" }));
    fireEvent.keyDown(toolsButton, {
      altKey: true,
      key: "ArrowLeft",
    });
    expect(
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-toolbar-action]"),
      ).map((element) => element.dataset.toolbarAction),
    ).toEqual([
      "recording",
      "clear",
      "refresh",
      "breakpoints",
      "tools",
      "throttling",
      "processes",
      "settings",
    ]);
    expect(window.localStorage.getItem("capture.toolbar.actionOrder")).toBe(
      '["recording","clear","refresh","breakpoints","tools","throttling","processes","settings"]',
    );

    unmount();
    const persistedView = render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={createControlClientStub(snapshot)}
          eventClient={createConnectedEventClient()}
        >
          <TopToolbar
            onOpenSslSettings={() => undefined}
            onOpenProtocolSettings={() => undefined}
            onOpenListenerSettings={() => undefined}
            onOpenToolSettings={() => undefined}
          />
        </ServiceProvider>
      </MemoryRouter>,
    );
    expect(
      Array.from(
        persistedView.container.querySelectorAll<HTMLElement>(
          "[data-toolbar-action]",
        ),
      ).map((element) => element.dataset.toolbarAction),
    ).toEqual([
      "recording",
      "clear",
      "refresh",
      "breakpoints",
      "tools",
      "throttling",
      "processes",
      "settings",
    ]);
  });

  it("录制写请求期间保留按钮盒模型并阻止重复触发", async () => {
    const user = userEvent.setup();
    const snapshot = createServiceSnapshot();
    const updateRecording = vi.fn(() => new Promise<never>(() => undefined));

    render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={createControlClientStub(snapshot, { updateRecording })}
          eventClient={createConnectedEventClient()}
        >
          <TopToolbar
            onOpenSslSettings={() => undefined}
            onOpenProtocolSettings={() => undefined}
            onOpenListenerSettings={() => undefined}
            onOpenToolSettings={() => undefined}
          />
        </ServiceProvider>
      </MemoryRouter>,
    );

    const toggleButton = await screen.findByRole("button", {
      name: "切换事务录制状态",
    });
    await user.click(toggleButton);
    await waitFor(() =>
      expect(toggleButton).toHaveAttribute("aria-busy", "true"),
    );
    expect(toggleButton).not.toBeDisabled();
    expect(toggleButton).toHaveAttribute("aria-disabled", "true");
    await user.click(toggleButton);
    expect(updateRecording).toHaveBeenCalledOnce();
  });

  it("单一按钮切换录制状态并在独立窗口确认清空事务", async () => {
    const user = userEvent.setup();
    const openWindow = vi.spyOn(window, "open").mockReturnValue(null);
    const transaction = createTransactionSummary();
    const baseSnapshot = createServiceSnapshot();
    let currentSnapshot = createServiceSnapshot({
      recording: {
        ...baseSnapshot.recording,
        transactionCount: 1,
        droppedCount: 2,
      },
      transactions: {
        ...baseSnapshot.transactions,
        total: 1,
        items: [transaction],
      },
    });
    const updateRecording = vi.fn(async () => {
      const nextRevision = currentSnapshot.revision + 1;
      currentSnapshot = {
        ...currentSnapshot,
        revision: nextRevision,
        recording: {
          ...currentSnapshot.recording,
          state: "paused",
        },
        transactions: {
          ...currentSnapshot.transactions,
          revision: nextRevision,
        },
      };
      return {
        serverInstanceId: currentSnapshot.serverInstanceId,
        revision: currentSnapshot.revision,
        recording: currentSnapshot.recording,
      };
    });
    const clearRecording = vi.fn(async () => {
      const nextRevision = currentSnapshot.revision + 1;
      currentSnapshot = {
        ...currentSnapshot,
        revision: nextRevision,
        recording: {
          ...currentSnapshot.recording,
          transactionCount: 0,
        },
        transactions: {
          ...currentSnapshot.transactions,
          revision: nextRevision,
          total: 0,
          items: [],
        },
      };
      return {
        serverInstanceId: currentSnapshot.serverInstanceId,
        revision: currentSnapshot.revision,
        recording: currentSnapshot.recording,
      };
    });
    const controlClient = createControlClientStub(currentSnapshot, {
      getSnapshot: async () => currentSnapshot,
      updateRecording,
      clearRecording,
    });

    render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={controlClient}
          eventClient={createConnectedEventClient()}
        >
          <TopToolbar
            onOpenSslSettings={() => undefined}
            onOpenProtocolSettings={() => undefined}
            onOpenListenerSettings={() => undefined}
            onOpenToolSettings={() => undefined}
          />
          <ConnectionStatusBar />
        </ServiceProvider>
      </MemoryRouter>,
    );

    const toggleButton = await screen.findByRole("button", {
      name: "切换事务录制状态",
    });
    expect(
      toggleButton.querySelector('[data-toolbar-icon="recordingActive"]'),
    ).not.toBeNull();
    expect(
      screen.getByText("正在录制").closest(".recordingStatus"),
    ).toHaveTextContent(/正在录制.*1 条事务.*已丢弃 2 条/);
    await user.click(toggleButton);
    await waitFor(() => expect(updateRecording).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(
        toggleButton.querySelector('[data-toolbar-icon="recordingIdle"]'),
      ).not.toBeNull(),
    );

    const clearButton = screen.getByRole("button", { name: "清空事务" });
    expect(
      clearButton.querySelector('[data-toolbar-icon="clearEnabled"]'),
    ).not.toBeNull();
    await user.click(clearButton);
    expect(openWindow).toHaveBeenCalledWith(
      "/window/dialog/clear-recording?transactionCount=1",
      "app-window-clear-recording",
      expect.stringContaining("width=560,height=300"),
    );
    expect(clearRecording).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    openWindow.mockRestore();
  });

  it("从顶栏直接打开设置并保持工具菜单职责单一", async () => {
    const user = userEvent.setup();
    const openWindow = vi.spyOn(window, "open").mockReturnValue(null);
    const snapshot = createServiceSnapshot();

    render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={createControlClientStub(snapshot)}
          eventClient={createConnectedEventClient()}
        >
          <TopToolbar
            onOpenSslSettings={() => undefined}
            onOpenProtocolSettings={() => undefined}
            onOpenListenerSettings={() => undefined}
            onOpenToolSettings={() => undefined}
          />
        </ServiceProvider>
      </MemoryRouter>,
    );

    await user.click(await screen.findByRole("button", { name: "工具" }));
    expect(
      screen.queryByRole("menuitem", { name: "设置" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: "录制规则集" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "设置" }));
    await waitFor(() =>
      expect(openWindow).toHaveBeenCalledWith(
        "/window/settings/interface",
        "app-window-settings-interface",
        expect.stringContaining("width="),
      ),
    );

    expect(
      screen
        .getByRole("button", { name: "进程选择器" })
        .querySelector(".lucide-list-filter"),
    ).not.toBeNull();
    await user.click(screen.getByRole("button", { name: "进程选择器" }));
    await waitFor(() =>
      expect(openWindow).toHaveBeenCalledWith(
        "/window/dialog/processes",
        "app-window-process-manager",
        expect.stringContaining("width="),
      ),
    );
    openWindow.mockRestore();
  });

  it("已保存免确认偏好时由扫把按钮直接清空", async () => {
    const user = userEvent.setup();
    window.localStorage.setItem(
      "capture.recording.skipClearConfirmation",
      "true",
    );
    const baseSnapshot = createServiceSnapshot();
    const snapshot = createServiceSnapshot({
      recording: {
        ...baseSnapshot.recording,
        transactionCount: 1,
      },
      transactions: {
        ...baseSnapshot.transactions,
        total: 1,
        items: [createTransactionSummary()],
      },
    });
    const clearRecording = vi.fn(async () => ({
      serverInstanceId: snapshot.serverInstanceId,
      revision: snapshot.revision + 1,
      recording: {
        ...snapshot.recording,
        transactionCount: 0,
      },
    }));

    render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={createControlClientStub(snapshot, {
            clearRecording,
          })}
          eventClient={createConnectedEventClient()}
        >
          <TopToolbar
            onOpenSslSettings={() => undefined}
            onOpenProtocolSettings={() => undefined}
            onOpenListenerSettings={() => undefined}
            onOpenToolSettings={() => undefined}
          />
        </ServiceProvider>
      </MemoryRouter>,
    );

    await user.click(await screen.findByRole("button", { name: "清空事务" }));
    await waitFor(() => expect(clearRecording).toHaveBeenCalledOnce());
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
});
