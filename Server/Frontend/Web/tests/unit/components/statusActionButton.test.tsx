import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import {
  type ControlClient,
  ControlClientError,
} from "@/api/controlClient";
import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";
import { ConnectionStatusBar } from "@/components/connectionStatusBar";
import { StatusActionButton } from "@/components/statusActionButton";

/**
 * 模拟单一权威控制端；启停调用会推进 revision 并返回新快照。
 */
function createStatefulControlClient(): {
  client: ControlClient;
  startService: ReturnType<typeof vi.fn>;
  stopService: ReturnType<typeof vi.fn>;
} {
  let currentSnapshot = createServiceSnapshot();
  const startService = vi.fn(async () => {
    currentSnapshot = createServiceSnapshot({
      revision: currentSnapshot.revision + 1,
      serviceState: "running",
      listeners: {
        ...currentSnapshot.listeners,
        socks5: {
          ...currentSnapshot.listeners.socks5,
          state: "running",
          boundEndpoint: "127.0.0.1:1080",
        },
      },
    });
    return currentSnapshot;
  });
  const stopService = vi.fn(async () => {
    currentSnapshot = createServiceSnapshot({
      revision: currentSnapshot.revision + 1,
      serviceState: "stopped",
      listeners: {
        ...currentSnapshot.listeners,
        socks5: {
          ...currentSnapshot.listeners.socks5,
          state: "stopped",
          boundEndpoint: null,
        },
      },
    });
    return currentSnapshot;
  });
  const client = createControlClientStub(currentSnapshot, {
    getSnapshot: async () => currentSnapshot,
    startService,
    stopService,
    updateConfiguration: async () => currentSnapshot,
    clearSessions: async () => currentSnapshot,
  });
  return { client, startService, stopService };
}

/**
 * 模拟已连接事件流；测试只验证控制动作，不注入额外快照。
 */
function createConnectedEventClient(): EventStreamClient {
  return {
    start(callbacks: EventClientCallbacks) {
      callbacks.onConnectionState("connected", "事件流已连接");
    },
    stop() {},
  };
}

/**
 * 创建连接失败依赖；快照请求保持失败，界面不得生成默认服务状态。
 */
function createDisconnectedDependencies(): {
  controlClient: ControlClient;
  eventClient: EventStreamClient;
} {
  const connectionError = new ControlClientError("控制服务未连接", null);
  const snapshot = createServiceSnapshot();
  return {
    controlClient: createControlClientStub(snapshot, {
      getSnapshot: async () => {
        throw connectionError;
      },
      startService: async () => {
        throw connectionError;
      },
      stopService: async () => {
        throw connectionError;
      },
      updateConfiguration: async () => {
        throw connectionError;
      },
      clearSessions: async () => {
        throw connectionError;
      },
    }),
    eventClient: {
      start(callbacks: EventClientCallbacks) {
        callbacks.onConnectionState("disconnected", "事件流未连接");
      },
      stop() {},
    },
  };
}

describe("单一状态启停动作", () => {
  it("同一个按钮按后端状态在启动与停止间切换", async () => {
    const user = userEvent.setup();
    const { client, startService, stopService } =
      createStatefulControlClient();
    render(
      <ServiceProvider
        controlClient={client}
        eventClient={createConnectedEventClient()}
      >
        <StatusActionButton />
      </ServiceProvider>,
    );

    const startButton = await screen.findByRole("button", {
      name: "启动服务",
    });
    await user.click(startButton);

    await waitFor(() => {
      expect(startService).toHaveBeenCalledTimes(1);
      expect(
        screen.getByRole("button", { name: "停止服务" }),
      ).toBeEnabled();
    });

    await user.click(screen.getByRole("button", { name: "停止服务" }));

    await waitFor(() => {
      expect(stopService).toHaveBeenCalledTimes(1);
      expect(
        screen.getByRole("button", { name: "启动服务" }),
      ).toBeEnabled();
    });
  });

  it("后端缺失时保持未连接且不提供可点击动作", async () => {
    const dependencies = createDisconnectedDependencies();
    render(
      <ServiceProvider {...dependencies}>
        <StatusActionButton />
        <ConnectionStatusBar />
      </ServiceProvider>,
    );

    expect(
      await screen.findByRole("button", { name: "控制服务未连接" }),
    ).toBeDisabled();
    expect(
      screen.getAllByTitle("控制服务未连接").length,
    ).toBeGreaterThan(0);
  });

  it("服务写入期间保留停止图标，避免将稳定图标错误旋转为加载提示", async () => {
    const user = userEvent.setup();
    const runningSnapshot = createServiceSnapshot({ serviceState: "running" });
    const stoppedSnapshot = createServiceSnapshot({
      revision: runningSnapshot.revision + 1,
      serviceState: "stopped",
    });
    let resolveStop!: (snapshot: typeof stoppedSnapshot) => void;
    const stopResult = new Promise<typeof stoppedSnapshot>((resolve) => {
      resolveStop = resolve;
    });
    const stopService = vi.fn(() => stopResult);
    const controlClient = createControlClientStub(runningSnapshot, {
      getSnapshot: async () => runningSnapshot,
      stopService,
    });

    render(
      <ServiceProvider
        controlClient={controlClient}
        eventClient={createConnectedEventClient()}
      >
        <StatusActionButton />
      </ServiceProvider>,
    );

    const stopButton = await screen.findByRole("button", {
      name: "停止服务",
    });
    await user.click(stopButton);

    expect(stopService).toHaveBeenCalledOnce();
    expect(stopButton).toBeDisabled();
    expect(stopButton.querySelector(".isSpinning")).toBeNull();

    await act(async () => {
      resolveStop(stoppedSnapshot);
      await stopResult;
    });

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "启动服务" })).toBeEnabled(),
    );
  });
});
