import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import { ProcessManagerPage } from "@/pages/processManagerPage";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";

const connectedEvents: EventStreamClient = {
  start(callbacks) {
    callbacks.onConnectionState("connected", "connected");
  },
  stop() {},
};

/** 验证进程筛选、连续添加和按路径提交；PID 仅用于展示，不进入持久化请求。 */
describe("进程管理页", () => {
  it("筛选进程并保存可执行路径", async () => {
    const user = userEvent.setup();
    const processSnapshot = {
      enabled: false,
      selectedPaths: [] as string[],
      resolvedProcessIds: [] as number[],
      processIcons: {
        "c:\\apps\\alpha.exe": "data:image/png;base64,YWxwaGE=",
      },
      processes: [
        {
          processId: 120,
          name: "alpha.exe",
          executablePath: "C:\\Apps\\alpha.exe",
        },
        {
          processId: 220,
          name: "beta.exe",
          executablePath: "C:\\Apps\\beta.exe",
        },
      ],
    };
    const updateProcessSelection = vi.fn(async (update) => ({
      ...processSnapshot,
      enabled: update.enabled,
      selectedPaths: update.selectedPaths,
      resolvedProcessIds: [120],
    }));
    const { container } = render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={createControlClientStub(createServiceSnapshot(), {
            getProcesses: async () => processSnapshot,
            updateProcessSelection,
          })}
          eventClient={connectedEvents}
        >
          <ProcessManagerPage />
        </ServiceProvider>
      </MemoryRouter>,
    );

    await user.type(await screen.findByPlaceholderText("按名称、路径或 PID 筛选"), "alpha");
    const selector = screen.getByRole("button", { name: "运行中进程" });
    await user.click(selector);
    expect(screen.queryByText(/beta\.exe/u)).not.toBeInTheDocument();
    await user.click(screen.getByText("C:\\Apps\\alpha.exe"));
    await user.click(screen.getByRole("button", { name: "添加进程" }));

    await waitFor(() => {
      expect(updateProcessSelection).toHaveBeenCalledWith(
        {
          enabled: true,
          selectedPaths: ["C:\\Apps\\alpha.exe"],
        },
        undefined,
      );
    });
    expect(screen.getByText("C:\\Apps\\alpha.exe")).toBeInTheDocument();
    expect(
      container.querySelector('img[src^="data:image/png;base64,"]'),
    ).not.toBeNull();
  });

  it("捕获 PID 变化后由 WebSocket 事件刷新进程视图", async () => {
    let eventCallbacks: EventClientCallbacks | null = null;
    const liveEvents: EventStreamClient = {
      /** 保存测试连接回调，用于模拟后端主动推送而不引入轮询计时器。 */
      start(callbacks) {
        eventCallbacks = callbacks;
        callbacks.onConnectionState("connected", "connected");
      },
      /** 测试卸载不持有真实套接字，因此停止操作只清理回调引用。 */
      stop() {
        eventCallbacks = null;
      },
    };
    const processSnapshot = {
      enabled: true,
      selectedPaths: ["C:\\Apps\\alpha.exe"],
      resolvedProcessIds: [120],
      processIcons: {},
      processes: [
        {
          processId: 120,
          name: "alpha.exe",
          executablePath: "C:\\Apps\\alpha.exe",
        },
      ],
    };
    const getProcesses = vi.fn(async () => processSnapshot);
    const serviceSnapshot = createServiceSnapshot({
      configuration: {
        ...createServiceSnapshot().configuration,
        processCapture: {
          enabled: true,
          processIds: [120],
          proxyPort: 1080,
        },
      },
      processCapture: {
        ...createServiceSnapshot().processCapture,
        configuredProcessIds: [120],
        running: true,
      },
    });
    render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={createControlClientStub(serviceSnapshot, {
            getProcesses,
          })}
          eventClient={liveEvents}
        >
          <ProcessManagerPage />
        </ServiceProvider>
      </MemoryRouter>,
    );

    await waitFor(() => expect(getProcesses).toHaveBeenCalledTimes(1));
    act(() => {
      eventCallbacks?.onMessage({
        type: "processCapture",
        serverInstanceId: serviceSnapshot.serverInstanceId,
        revision: serviceSnapshot.revision + 1,
        processCapture: {
          ...serviceSnapshot.processCapture,
          configuredProcessIds: [120, 121],
        },
      });
    });

    await waitFor(() => expect(getProcesses).toHaveBeenCalledTimes(2));
  });
});
