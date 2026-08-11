import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type {
  EventMessage,
  PluginDetails,
  PluginSnapshot,
} from "@/api/protocol";
import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import { PluginManagerPage } from "@/pages/pluginManagerPage";
import i18n from "@/i18n";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";

/** 创建已连接事件流替身，使用例聚焦插件控制面和表单脱敏行为。 */
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

const pluginDetails: PluginDetails = {
  snapshot: pluginSnapshot,
  configSchema: {
    type: "object",
    title: "样例设置",
    description: "仅用于验证表单契约。",
    properties: {
      endpoint: {
        type: "string",
        title: "Endpoint",
        description: "上游地址",
        enum: [],
        default: null,
        format: "",
        xAdvanced: false,
        minimum: null,
        maximum: null,
        minLength: null,
        maxLength: null,
      },
      token: {
        type: "string",
        title: "Token",
        description: "访问令牌",
        enum: [],
        default: null,
        format: "password",
        xAdvanced: false,
        minimum: null,
        maximum: null,
        minLength: null,
        maxLength: null,
      },
    },
    required: ["endpoint", "token"],
    additionalProperties: false,
  },
  configuration: { endpoint: "https://before.test" },
  configuredSecretFields: ["token"],
};

describe("插件管理页面", () => {
  /**
   * 可见安装按钮必须在点击事件内触发文件输入，保证桌面 WebView 可以显示系统文件选择器。
   * 文件输入只服务于原生选择器，必须完全退出辅助功能树，避免浏览器额外暴露无名称控件。
   */
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

    expect(openChooser).toHaveBeenCalledTimes(1);
  });

  /** 秘密字段仅显示已配置状态，保存普通字段时请求体不携带旧秘密值。 */
  it("脱敏展示并保留未修改的密码字段", async () => {
    const user = userEvent.setup();
    const updatePluginConfiguration = vi.fn(async () => pluginDetails);
    const controlClient = createControlClientStub(
      createServiceSnapshot({
        plugins: [pluginSnapshot],
      }),
      {
        getPluginDetails: async () => pluginDetails,
        updatePluginConfiguration,
      },
    );

    render(
      <ServiceProvider
        controlClient={controlClient}
        eventClient={createConnectedEventClient()}
      >
        <PluginManagerPage />
      </ServiceProvider>,
    );

    expect(
      screen.getByRole("heading", { level: 1, name: i18n.t("plugins.title") }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    const endpointInput = await screen.findByRole("textbox", {
      name: /Endpoint/,
    });
    expect(screen.queryByDisplayValue("secret-value")).not.toBeInTheDocument();
    await user.clear(endpointInput);
    await user.type(endpointInput, "https://after.test");
    await user.click(
      screen.getByRole("button", { name: i18n.t("plugins.saveConfiguration") }),
    );

    await waitFor(() =>
      expect(updatePluginConfiguration).toHaveBeenCalledTimes(1),
    );
    expect(updatePluginConfiguration).toHaveBeenCalledWith("sample.plugin", {
      configuration: { endpoint: "https://after.test" },
    });
  });

  /** 验证插件连接计数直接消费 SSE 增量，运行态变化不会放大为插件列表 GET。 */
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
      getPluginDetails: async () => pluginDetails,
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

  it("重载成功后重新读取插件详情与配置架构", async () => {
    const user = userEvent.setup();
    const reloadedDetails: PluginDetails = {
      ...pluginDetails,
      configSchema: {
        ...pluginDetails.configSchema!,
        title: "重载后的设置",
      },
    };
    const getPluginDetails = vi
      .fn<() => Promise<PluginDetails>>()
      .mockResolvedValueOnce(pluginDetails)
      .mockResolvedValueOnce(reloadedDetails);
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
    await screen.findByText(pluginDetails.configSchema!.title);
    await user.click(
      screen.getByRole("button", { name: i18n.t("plugins.reload") }),
    );

    await screen.findByText("重载后的设置");
    expect(reloadPlugin).toHaveBeenCalledWith(pluginSnapshot.id);
    expect(getPluginDetails).toHaveBeenCalledTimes(2);
  });
});
