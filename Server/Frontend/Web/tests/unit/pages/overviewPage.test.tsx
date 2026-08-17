import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import type { EventClientCallbacks, EventStreamClient } from "@/api/eventClient";
import { OverviewPage } from "@/pages/overviewPage";
import { MultiAccountOverview } from "@/components/multiAccountOverview";
import { showIndependentWindow } from "@/platform/independentWindowContract";
import { ServiceProvider } from "@/state/serviceStore";
import { createControlClientStub, createServiceSnapshot } from "#tests/testFixtures";

vi.mock("@/platform/independentWindowContract", async () => {
  const actual = await vi.importActual<typeof import("@/platform/independentWindowContract")>(
    "@/platform/independentWindowContract",
  );
  return {
    ...actual,
    showIndependentWindow: vi.fn(async () => undefined),
  };
});

/** 为概览提供稳定的已连接事件流，避免实时重连定时器干扰页面指标断言。 */
function createConnectedEventClient(): EventStreamClient {
  return {
    start(callbacks: EventClientCallbacks) {
      callbacks.onConnectionState("connected", "事件流已连接");
    },
    stop() {},
  };
}

describe("工作台多账号概览", () => {
  /** 多账号关闭时入口和指标都不占用概览空间。 */
  it("仅在启用多账号后显示账号服务区域", async () => {
    render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={createControlClientStub(createServiceSnapshot())}
          eventClient={createConnectedEventClient()}
        >
          <OverviewPage />
        </ServiceProvider>
      </MemoryRouter>,
    );

    expect(await screen.findByText("工作台概览")).toBeInTheDocument();
    expect(screen.queryByText("SOCKS5 账号服务")).toBeNull();
    expect(screen.queryByText("连接状态")).toBeNull();
    expect(screen.queryByText("控制接口")).toBeNull();
    expect(screen.queryByText("事件流")).toBeNull();
  });

  /** 实时指标必须直接呈现每秒速率，并在同一工作台路由进入账号管理。 */
  it("显示实时账号指标并进入内部管理路由", async () => {
    const user = userEvent.setup();
    const baseline = createServiceSnapshot();
    const snapshot = createServiceSnapshot({
      configuration: {
        ...baseline.configuration,
        multiAccount: {
          ...baseline.configuration.multiAccount,
          enabled: true,
          state: "running",
          summary: {
            onlineAccounts: 3,
            activeConnections: 7,
            uploadBytesPerSecond: 1_024,
            downloadBytesPerSecond: 2_048,
          },
        },
      },
    });
    render(
      <MemoryRouter>
        <ServiceProvider
          controlClient={createControlClientStub(snapshot)}
          eventClient={createConnectedEventClient()}
        >
        <Routes><Route path="/" element={<OverviewPage />} /></Routes>
        </ServiceProvider>
      </MemoryRouter>,
    );

    expect(await screen.findByText("当前在线账号")).toBeInTheDocument();
    expect(screen.getByText("1.00 KiB/s")).toBeInTheDocument();
    expect(screen.getByText("2.00 KiB/s")).toBeInTheDocument();
    expect(screen.getAllByText("活动连接")).toHaveLength(1);
    expect(screen.getByText("活动连接").parentElement).toHaveTextContent("活动连接7");
    expect(screen.getByText("已接受")).toBeInTheDocument();
    expect(screen.queryByText("失败连接")).toBeNull();
    expect(screen.queryByText("上行流量")).toBeNull();
    expect(screen.queryByText("下行流量")).toBeNull();
    expect(screen.queryByText("会话记录")).toBeNull();
    const realtimeUpload = screen.getByText("实时上行带宽").parentElement;
    const realtimeDownload = screen.getByText("实时下行带宽").parentElement;
    expect(realtimeUpload?.nextElementSibling).toBe(realtimeDownload);
    await user.click(screen.getByRole("button", { name: "SOCKS5 管理" }));

    expect(showIndependentWindow).toHaveBeenCalledWith({ kind: "accountManagement" });
  });

  /** 局部轮询必须保持单请求串行，并在功能关闭后同时终止请求和下一次定时读取。 */
  it("按秒刷新局部快照并在禁用时清理轮询", async () => {
    vi.useFakeTimers();
    const baseline = createServiceSnapshot().configuration.multiAccount;
    const enabledConfiguration = {
      ...baseline,
      enabled: true,
      state: "running" as const,
      summary: {
        onlineAccounts: 1,
        activeConnections: 2,
        uploadBytesPerSecond: 128,
        downloadBytesPerSecond: 256,
      },
    };
    const signals: AbortSignal[] = [];
    const readState = vi.fn(async (signal?: AbortSignal) => {
      if (signal) {
        signals.push(signal);
      }
      return enabledConfiguration;
    });
    const { rerender } = render(
      <MultiAccountOverview
        configuration={enabledConfiguration}
        disabled={false}
        acceptedConnections={3}
        readState={readState}
        onOpenManagement={async () => {}}
      />,
    );

    await act(async () => {});
    expect(readState).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(readState).toHaveBeenCalledTimes(2);

    rerender(
      <MultiAccountOverview
        configuration={{ ...enabledConfiguration, enabled: false }}
        disabled={false}
        acceptedConnections={3}
        readState={readState}
        onOpenManagement={async () => {}}
      />,
    );
    expect(signals.every((signal) => signal.aborted)).toBe(true);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000);
    });
    expect(readState).toHaveBeenCalledTimes(2);
    vi.useRealTimers();
  });
});
