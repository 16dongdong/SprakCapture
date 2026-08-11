import { act, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";

import type { ControlClient } from "@/api/controlClient";
import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import { App } from "@/app";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";

/**
 * 创建仅供路由渲染使用的已连接依赖；全部方法返回同一权威快照。
 */
function createRouteDependencies(): {
  controlClient: ControlClient;
  eventClient: EventStreamClient;
} {
  const stoppedSnapshot = createServiceSnapshot();
  const snapshot = createServiceSnapshot({
    serviceState: "running",
    processCapture: {
      running: true,
      configuredProcessIds: [1200, 2400],
      trackedFlows: 3,
      acceptedConnections: 5,
      redirectedPackets: 42,
      restoredPackets: 38,
      bytesUp: 2048,
      bytesDown: 8192,
      lastError: null,
    },
    listeners: {
      ...stoppedSnapshot.listeners,
      socks5: {
        ...stoppedSnapshot.listeners.socks5,
        state: "running",
        boundEndpoint: "127.0.0.1:1080",
      },
    },
  });
  return {
    controlClient: createControlClientStub(snapshot),
    eventClient: {
      start(callbacks: EventClientCallbacks) {
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    },
  };
}

describe("悬浮窗路由", () => {
  it("只渲染共享状态面板且不加载主窗口导航", async () => {
    const dependencies = createRouteDependencies();
    render(
      <MemoryRouter initialEntries={["/floating"]}>
        <ServiceProvider {...dependencies}>
          <App />
        </ServiceProvider>
      </MemoryRouter>,
    );

    expect(await screen.findByText("SOCKS5 127.0.0.1:1080")).toBeVisible();
    expect(screen.getByText("流量捕获服务")).toBeVisible();
    expect(screen.getByText("运行中")).toBeVisible();
    expect(
      screen.getByRole("button", { name: "停止服务" }),
    ).toBeEnabled();
    expect(screen.queryByRole("navigation", { name: "主导航" })).toBeNull();
  });

  it("概览页只保留顶栏的唯一启停按钮", async () => {
    const dependencies = createRouteDependencies();
    render(
      <MemoryRouter initialEntries={["/overview"]}>
        <ServiceProvider {...dependencies}>
          <App />
        </ServiceProvider>
      </MemoryRouter>,
    );

    expect(
      await screen.findAllByRole("button", { name: "停止服务" }),
    ).toHaveLength(1);
    expect(screen.getByText("活动连接").parentElement).toHaveTextContent(
      "活动连接4",
    );
    expect(screen.getByText("已接受").parentElement).toHaveTextContent(
      "已接受7",
    );
    expect(screen.getByText("上行流量").parentElement).toHaveTextContent(
      "上行流量3.50 KiB",
    );
    expect(screen.getByText("下行流量").parentElement).toHaveTextContent(
      "下行流量12.00 KiB",
    );
    expect(screen.getByText("会话记录").parentElement).toHaveTextContent(
      "会话记录7",
    );
  });

  it("概览页逐帧呈现事件流指标而不等待定时刷新", async () => {
    const snapshot = createServiceSnapshot({
      serviceState: "running",
      processCapture: {
        running: true,
        configuredProcessIds: [1200],
        trackedFlows: 0,
        acceptedConnections: 0,
        redirectedPackets: 0,
        restoredPackets: 0,
        bytesUp: 0,
        bytesDown: 0,
        lastError: null,
      },
    });
    let eventCallbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(callbacks) {
        eventCallbacks = callbacks;
        callbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    render(
      <MemoryRouter initialEntries={["/overview"]}>
        <ServiceProvider
          controlClient={createControlClientStub(snapshot)}
          eventClient={eventClient}
        >
          <App />
        </ServiceProvider>
      </MemoryRouter>,
    );

    await screen.findByText("工作台概览");
    await waitFor(() => expect(eventCallbacks).not.toBeNull());
    act(() => {
      eventCallbacks?.onMessage({
        type: "metrics",
        serverInstanceId: snapshot.serverInstanceId,
        revision: 2,
        metrics: {
          ...snapshot.metrics,
          acceptedConnections: 9,
          activeConnections: 4,
          failedConnections: 2,
          bytesUp: 2048,
          bytesDown: 8192,
        },
      });
    });

    expect(screen.getByText("活动连接").parentElement).toHaveTextContent(
      "活动连接4",
    );
    expect(screen.getByText("已接受").parentElement).toHaveTextContent(
      "已接受9",
    );
    expect(screen.getByText("失败连接").parentElement).toHaveTextContent(
      "失败连接2",
    );
    expect(screen.getByText("上行流量").parentElement).toHaveTextContent(
      "上行流量2.00 KiB",
    );
    expect(screen.getByText("下行流量").parentElement).toHaveTextContent(
      "下行流量8.00 KiB",
    );

    act(() => {
      eventCallbacks?.onMessage({
        type: "processCapture",
        serverInstanceId: snapshot.serverInstanceId,
        revision: 3,
        processCapture: {
          ...snapshot.processCapture,
          trackedFlows: 3,
          acceptedConnections: 5,
          bytesUp: 1024,
          bytesDown: 4096,
        },
      });
    });

    expect(screen.getByText("活动连接").parentElement).toHaveTextContent(
      "活动连接7",
    );
    expect(screen.getByText("已接受").parentElement).toHaveTextContent(
      "已接受14",
    );
    expect(screen.getByText("上行流量").parentElement).toHaveTextContent(
      "上行流量3.00 KiB",
    );
    expect(screen.getByText("下行流量").parentElement).toHaveTextContent(
      "下行流量12.00 KiB",
    );
  });
});
