import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import type { PluginDetails, PluginSnapshot } from "@/api/protocol";
import i18n from "@/i18n";
import { PluginWindowPage } from "@/pages/pluginWindowPage";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";

vi.mock("@/platform/managedWindow", () => ({
  closeCurrentManagedWindow: vi.fn(async () => undefined),
}));

const pluginSnapshot: PluginSnapshot = {
  id: "sample.plugin",
  name: "样例插件",
  version: "1.0.0",
  apiVersion: 1,
  runtime: "native",
  hooks: ["on_stream_data"],
  enabled: true,
  state: "enabled",
  errorCode: null,
  activeConnections: 0,
};

const pluginDetails: PluginDetails = {
  snapshot: pluginSnapshot,
  configSchema: {
    type: "object",
    title: "样例设置",
    description: "独立插件窗口设置。",
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

/**
 * 在内存路由中挂载单插件独立窗口。
 *
 * 运行上下文：测试通过真实 ServiceProvider 验证控制客户端调用和表单生命周期。
 * 参数：details 是宿主返回详情，updatePluginConfiguration 可覆盖保存行为。
 * 失败语义：控制替身抛错时页面应保留窗口状态，测试不会建立网络连接。
 */
function renderPluginWindow(
  details: PluginDetails,
  updatePluginConfiguration = vi.fn(async () => details),
) {
  const controlClient = createControlClientStub(
    createServiceSnapshot({ plugins: [pluginSnapshot] }),
    {
      getPluginDetails: async () => details,
      updatePluginConfiguration,
    },
  );
  render(
    <ServiceProvider controlClient={controlClient}>
      <MemoryRouter initialEntries={["/window/plugin/sample.plugin"]}>
        <Routes>
          <Route
            path="/window/plugin/:pluginId"
            element={<PluginWindowPage />}
          />
        </Routes>
      </MemoryRouter>
    </ServiceProvider>,
  );
  return { updatePluginConfiguration };
}

describe("插件独立窗口", () => {
  /** 秘密字段只显示状态，普通字段保存时不携带旧秘密。 */
  it("加载插件声明 UI 并保存脱敏配置", async () => {
    const user = userEvent.setup();
    const { updatePluginConfiguration } = renderPluginWindow(pluginDetails);

    const endpointInput = await screen.findByRole("textbox", {
      name: /Endpoint/,
    });
    expect(screen.getByRole("heading", { name: pluginSnapshot.name })).toBeVisible();
    expect(screen.queryByDisplayValue("secret-value")).not.toBeInTheDocument();
    await user.clear(endpointInput);
    await user.type(endpointInput, "https://after.test");
    await user.click(
      screen.getByRole("button", {
        name: i18n.t("plugins.saveConfiguration"),
      }),
    );

    await waitFor(() =>
      expect(updatePluginConfiguration).toHaveBeenCalledWith(
        pluginSnapshot.id,
        { configuration: { endpoint: "https://after.test" } },
      ),
    );
  });

  /** 没有 Schema 的插件仍有稳定窗口，但不伪造可编辑 UI。 */
  it("未声明 UI 时显示明确空状态", async () => {
    renderPluginWindow({
      ...pluginDetails,
      configSchema: null,
      configuration: {},
      configuredSecretFields: [],
    });

    expect(await screen.findByText(i18n.t("plugins.noWindowUi"))).toBeVisible();
    expect(
      screen.queryByRole("button", {
        name: i18n.t("plugins.saveConfiguration"),
      }),
    ).not.toBeInTheDocument();
  });
});
