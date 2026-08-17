import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { EventMessage, PluginSnapshot } from "@/api/protocol";
import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import i18n from "@/i18n";
import { PluginManagerPage } from "@/pages/pluginManagerPage";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";

const windowMocks = vi.hoisted(() => ({ show: vi.fn() }));

vi.mock("@/platform/independentWindowContract", () => ({
  showIndependentWindow: windowMocks.show,
}));

/**
 * 创建已连接事件流替身。
 *
 * 运行上下文：页面测试只观察插件快照和用户操作，不建立真实网络连接。
 * 失败语义：测试主动注入的消息由调用方保存 callbacks 后发送。
 */
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

const pluginSnapshot: PluginSnapshot = {
  id: "sample.plugin",
  name: "样例插件",
  version: "1.0.0",
  apiVersion: 1,
  runtime: "native",
  hooks: ["on_stream_data"],
  enabled: false,
  state: "disabled",
  errorCode: null,
  activeConnections: 0,
};

describe("插件管理页面", () => {
  /** 可见安装按钮必须在点击事件内触发隐藏文件输入。 */
  it("点击安装插件包会直接打开文件选择器且不暴露重复控件", async () => {
    const user = userEvent.setup();
    const controlClient = createControlClientStub(createServiceSnapshot(), {
      listPlugins: async () => [],
    });

    const { container } = render(
      <ServiceProvider
        controlClient={controlClient}
        eventClient={createConnectedEventClient()}
      >
        <PluginManagerPage />
      </ServiceProvider>,
    );

    const packageInput =
      container.querySelector<HTMLInputElement>('input[type="file"]');
    expect(packageInput).not.toBeNull();
    expect(packageInput).toHaveAttribute("hidden");
    const openChooser = vi.spyOn(packageInput!, "click");

    await user.click(
      screen.getByRole("button", { name: i18n.t("plugins.install") }),
    );

    expect(openChooser).toHaveBeenCalledOnce();
  });

  /** 插件主页面只保留窗口入口，配置表单不得重复挂载。 */
  it("通过唯一按钮打开插件独立窗口且主页面不嵌入配置表单", async () => {
    const user = userEvent.setup();
    windowMocks.show.mockResolvedValue(undefined);
    const controlClient = createControlClientStub(
      createServiceSnapshot({ plugins: [pluginSnapshot] }),
    );

    render(
      <ServiceProvider
        controlClient={controlClient}
        eventClient={createConnectedEventClient()}
      >
        <PluginManagerPage />
      </ServiceProvider>,
    );

    await user.click(
      await screen.findByRole("button", {
        name: i18n.t("plugins.openWindow"),
      }),
    );

    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(windowMocks.show).toHaveBeenCalledWith({
      kind: "plugin",
      pluginId: pluginSnapshot.id,
      pluginName: pluginSnapshot.name,
    });
  });

  /** 连接计数由 SSE 增量更新，不放大为插件列表轮询。 */
  it("实时呈现插件连接计数且不重新查询列表", async () => {
    let callbacks: EventClientCallbacks | null = null;
    const eventClient: EventStreamClient = {
      start(nextCallbacks) {
        callbacks = nextCallbacks;
        nextCallbacks.onConnectionState("connected", "事件流已连接");
      },
      stop() {},
    };
    const listPlugins = vi.fn(async () => [pluginSnapshot]);
    const initialSnapshot = createServiceSnapshot({
      plugins: [pluginSnapshot],
    });
    const controlClient = createControlClientStub(initialSnapshot, {
      listPlugins,
    });

    const { container } = render(
      <ServiceProvider controlClient={controlClient} eventClient={eventClient}>
        <PluginManagerPage />
      </ServiceProvider>,
    );
    await screen.findByRole("heading", { name: pluginSnapshot.name });
    expect(
      container.querySelectorAll(".pluginMetadata dd").item(3),
    ).toHaveTextContent("0");

    act(() => {
      callbacks?.onMessage({
        type: "plugins",
        serverInstanceId: initialSnapshot.serverInstanceId,
        revision: 2,
        plugins: [{ ...pluginSnapshot, activeConnections: 7 }],
      });
    });

    await waitFor(() =>
      expect(
        container.querySelectorAll(".pluginMetadata dd").item(3),
      ).toHaveTextContent("7"),
    );
    expect(listPlugins).not.toHaveBeenCalled();
  });

  /** 重载只管理生命周期，插件配置仍归独立窗口所有。 */
  it("重载动作不在主页面读取插件配置", async () => {
    const user = userEvent.setup();
    const getPluginDetails = vi.fn();
    const reloadPlugin = vi.fn(async () => pluginSnapshot);
    const controlClient = createControlClientStub(
      createServiceSnapshot({ plugins: [pluginSnapshot] }),
      { getPluginDetails, reloadPlugin },
    );

    render(
      <ServiceProvider
        controlClient={controlClient}
        eventClient={createConnectedEventClient()}
      >
        <PluginManagerPage />
      </ServiceProvider>,
    );
    await screen.findByRole("heading", { name: pluginSnapshot.name });
    await user.click(
      screen.getByRole("button", { name: i18n.t("plugins.reload") }),
    );

    await waitFor(() => expect(reloadPlugin).toHaveBeenCalledOnce());
    expect(reloadPlugin).toHaveBeenCalledWith(pluginSnapshot.id);
    expect(getPluginDetails).not.toHaveBeenCalled();
  });
});
