import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { type ReactNode } from "react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";

import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import { TopToolbar } from "@/components/topToolbar";
import { SettingsPage, type SettingsSection } from "@/pages/settingsPage";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";

/**
 * 创建已连接事件流替身，使页面测试只验证页面导航与表单行为。
 *
 * 运行上下文：ServiceProvider 挂载时订阅事件流，替身立即发布已连接状态。
 * 失败语义：替身不发送业务事件，快照始终由控制客户端替身提供。
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
 * 使用可控服务快照渲染普通工具栏测试内容，路由探针用于断言真实跳转路径。
 *
 * 运行上下文：工具栏通过 useNavigate 打开独立设置页，必须处于路由上下文中。
 * 参数：children 为待验证组件，snapshot 为可选初始服务快照。
 * 失败语义：控制客户端替身不访问真实服务，所有写操作只返回传入快照。
 */
function renderWithService(
  children: ReactNode,
  snapshot = createServiceSnapshot(),
) {
  return render(
    <MemoryRouter>
      <ServiceProvider
        controlClient={createControlClientStub(snapshot)}
        eventClient={createConnectedEventClient()}
      >
        {children}
      </ServiceProvider>
    </MemoryRouter>,
  );
}

/**
 * 在指定设置路径上渲染页面，确保 useParams 与生产路由采用相同契约。
 *
 * 运行上下文：设置页面的当前区域完全由 `/settings/:section` 决定。
 * 参数：section 为待验证设置区域，snapshot 为可选服务快照。
 * 失败语义：控制客户端替身不触发网络请求，页面只消费固定快照。
 */
function renderSettingsPage(
  section: SettingsSection,
  snapshot = createServiceSnapshot(),
  controlClientOverrides: Parameters<typeof createControlClientStub>[1] = {},
) {
  return render(
    <MemoryRouter initialEntries={[`/settings/${section}`]}>
      <ServiceProvider
        controlClient={createControlClientStub(snapshot, controlClientOverrides)}
        eventClient={createConnectedEventClient()}
      >
        <Routes>
          <Route path="/settings/:section?" element={<SettingsPage />} />
        </Routes>
      </ServiceProvider>
    </MemoryRouter>,
  );
}

/**
 * 将当前路由暴露为测试输出，避免以组件内部状态替代浏览器导航断言。
 */
function LocationProbe() {
  const location = useLocation();
  return <output data-testid="current-path">{location.pathname}</output>;
}

describe("设置页面导航", () => {
  it("MCP 开关通过独立热更新端点提交且不显示代理重启提示", async () => {
    const user = userEvent.setup();
    const updateMcpConfiguration = vi.fn(async () =>
      createServiceSnapshot({
        mcp: {
          configuration: { enabled: true, port: 17_891 },
          running: true,
          endpoint: "http://127.0.0.1:17891/mcp",
          lastError: null,
        },
      }),
    );
    renderSettingsPage(
      "mcp",
      createServiceSnapshot({ serviceState: "running" }),
      { updateMcpConfiguration },
    );

    await user.click(
      await screen.findByRole("checkbox", { name: /^启用 MCP/ }),
    );

    expect(updateMcpConfiguration).toHaveBeenCalledWith(
      { enabled: true, port: 17_891 },
    );
    expect(screen.queryByText("应用配置会强制断开当前代理连接并重启服务。"))
      .not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "应用配置" })).not.toBeInTheDocument();
  });

  it("点击工具菜单外区域或按 Escape 会立即收起临时浮层", async () => {
    const user = userEvent.setup();
    renderWithService(
      <>
        <TopToolbar
          onOpenSslSettings={() => undefined}
          onOpenProtocolSettings={() => undefined}
          onOpenListenerSettings={() => undefined}
          onOpenToolSettings={() => undefined}
        />
        <button type="button">页面空白操作</button>
      </>,
    );

    const openButton = screen.getByRole("button", { name: "工具" });
    await user.click(openButton);
    expect(screen.getByRole("menu")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "页面空白操作" }));
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(openButton).toHaveAttribute("aria-expanded", "false");

    await user.click(openButton);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("点击顶栏设置打开独立设置窗口，而不是改写主窗口路由", async () => {
    const user = userEvent.setup();
    const openWindow = vi.spyOn(window, "open").mockReturnValue(null);
    renderWithService(
      <>
        <TopToolbar
          onOpenSslSettings={() => undefined}
          onOpenProtocolSettings={() => undefined}
          onOpenListenerSettings={() => undefined}
          onOpenToolSettings={() => undefined}
        />
        <LocationProbe />
      </>,
    );

    await user.click(screen.getByRole("button", { name: "设置" }));

    expect(openWindow).toHaveBeenCalledWith(
      "/window/settings/interface",
      "app-window-settings-interface",
      expect.stringContaining("width=1120,height=780"),
    );
    expect(screen.getByTestId("current-path")).toHaveTextContent("/");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    openWindow.mockRestore();
  });

  it("点击插件菜单会进入独立管理页，而不是打开模态对话框", async () => {
    const user = userEvent.setup();
    renderWithService(
      <>
        <TopToolbar
          onOpenSslSettings={() => undefined}
          onOpenProtocolSettings={() => undefined}
          onOpenListenerSettings={() => undefined}
          onOpenToolSettings={() => undefined}
        />
        <LocationProbe />
      </>,
    );

    await user.click(screen.getByRole("button", { name: "工具" }));
    await user.click(screen.getByRole("menuitem", { name: "插件" }));

    expect(screen.getByTestId("current-path")).toHaveTextContent("/plugins");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("左侧页面导航只渲染当前编辑区域，而不堆叠全部服务配置", async () => {
    const user = userEvent.setup();
    renderSettingsPage("listener");

    expect(
      await screen.findByRole("textbox", { name: "监听地址" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("combobox", { name: "认证模式" }),
    ).toBeInTheDocument();

    const navigation = screen.getByRole("navigation", { name: "设置" });
    await user.click(within(navigation).getByRole("link", { name: "二级代理" }));

    expect(
      screen.queryByRole("combobox", { name: "认证模式" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "协议" })).toBeInTheDocument();
    expect(
      screen.queryByRole("textbox", { name: "监听地址" }),
    ).not.toBeInTheDocument();
  });

  it("服务运行时页面仍允许编辑，并明确提示应用配置将强制重启", async () => {
    renderSettingsPage(
      "listener",
      createServiceSnapshot({ serviceState: "running" }),
    );

    expect(
      await screen.findByRole("textbox", { name: "监听地址" }),
    ).toBeEnabled();
    expect(
      screen.getByRole("button", { name: "应用配置" }),
    ).toBeEnabled();
    expect(
      screen.getByText("应用配置会强制断开当前代理连接并重启服务。"),
    ).toBeInTheDocument();
  });

  it("代理设置包含客户端认证且不再显示进程捕获", async () => {
    const snapshot = createServiceSnapshot();
    const { container } = renderSettingsPage("listener", snapshot);

    await screen.findByText("HTTP / SOCKS5 融合监听");
    expect(
      screen.getByRole("checkbox", { name: /启动时自动开启服务/ }),
    ).not.toBeChecked();
    expect(screen.getByText("认证模式")).toBeInTheDocument();
    expect(screen.queryByText("WinDivert 进程捕获")).not.toBeInTheDocument();
    expect(container.querySelector('a[href="/settings/authentication"]')).toBeNull();
  });

  it("代理设置提交时持久化启动自动开启服务开关", async () => {
    const user = userEvent.setup();
    const snapshot = createServiceSnapshot();
    const updateConfiguration = vi.fn(async () => snapshot);
    renderSettingsPage("listener", snapshot, { updateConfiguration });

    await user.click(
      await screen.findByRole("checkbox", { name: /启动时自动开启服务/ }),
    );
    await user.click(screen.getByRole("button", { name: "应用配置" }));

    expect(updateConfiguration).toHaveBeenCalledWith(
      expect.objectContaining({ startServiceOnLaunch: true }),
    );
  });

  it("远程管理设置提交唯一 Web 端点且不改写保存的单账号认证", async () => {
    const user = userEvent.setup();
    const snapshot = createServiceSnapshot();
    const updateConfiguration = vi.fn(async () => snapshot);
    const { container } = renderSettingsPage("multiAccount", snapshot, {
      updateConfiguration,
    });

    await user.click(
      await screen.findByRole("checkbox", { name: /启用远程管理/ }),
    );
    fireEvent.change(screen.getByRole("spinbutton", { name: "远程管理端口" }), {
      target: { value: "19091", valueAsNumber: 19_091 },
    });
    expect(container.querySelector(":invalid")).toBeNull();
    await user.click(screen.getByRole("button", { name: "应用配置" }));

    expect(updateConfiguration).toHaveBeenCalledWith(
      expect.objectContaining({
        authenticationMode: "none",
        multiAccount: {
          enabled: true,
          remoteHost: "0.0.0.0",
          remotePort: 19_091,
        },
      }),
    );
  });

  it("设置页不再提供账号管理页面入口", async () => {
    const baseline = createServiceSnapshot();
    const snapshot = createServiceSnapshot({
      configuration: {
        ...baseline.configuration,
        multiAccount: {
          ...baseline.configuration.multiAccount,
          enabled: true,
          state: "running",
        },
      },
    });
    renderSettingsPage("multiAccount", snapshot);

    expect(await screen.findAllByDisplayValue("Admin")).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "打开账号管理页面" })).toBeNull();
  });

  it("管理员设置直接保存新凭据且不展示独立 Key 轮换入口", async () => {
    const user = userEvent.setup();
    const baseline = createServiceSnapshot();
    const snapshot = createServiceSnapshot({
      configuration: {
        ...baseline.configuration,
        multiAccount: {
          ...baseline.configuration.multiAccount,
          enabled: true,
          state: "running",
        },
      },
    });
    const updateManagementIdentity = vi.fn(async (username: string) => ({
      identity: {
        username,
        credentialRevision: 2,
        apiKeyPrefix: "sak_v1_new_••••",
        apiKeyCreatedAt: snapshot.revision,
      },
      apiKey: "sak_v1_new_secret",
    }));
    renderSettingsPage("multiAccount", snapshot, { updateManagementIdentity });

    expect(await screen.findAllByDisplayValue("Admin")).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "轮换 API Key" })).toBeNull();
    await user.clear(screen.getByLabelText("新管理员账号"));
    await user.type(screen.getByLabelText("新管理员账号"), "Operator");
    await user.type(screen.getByLabelText("新管理员密码"), "NewPassword");
    await user.click(screen.getByRole("button", { name: "保存管理员配置" }));

    expect(updateManagementIdentity).toHaveBeenCalledWith("Operator", "NewPassword");
    expect(await screen.findByRole("button", { name: "复制 API Key" })).toBeInTheDocument();
  });

  it("远程管理运行时应用普通配置不要求填写新管理员密码", async () => {
    const user = userEvent.setup();
    const baseline = createServiceSnapshot();
    const snapshot = createServiceSnapshot({
      configuration: {
        ...baseline.configuration,
        multiAccount: {
          ...baseline.configuration.multiAccount,
          enabled: true,
          state: "running",
        },
      },
    });
    const updateConfiguration = vi.fn(async () => snapshot);
    const { container } = renderSettingsPage("multiAccount", snapshot, {
      updateConfiguration,
    });

    await screen.findAllByDisplayValue("Admin");
    expect(screen.getByLabelText("新管理员密码")).not.toBeRequired();
    expect(container.querySelector(":invalid")).toBeNull();
    await user.click(screen.getByRole("button", { name: "应用配置" }));

    expect(updateConfiguration).toHaveBeenCalledOnce();
  });

  it("获取当前 API Key 时不收集或发送管理员密码", async () => {
    const user = userEvent.setup();
    const baseline = createServiceSnapshot();
    const snapshot = createServiceSnapshot({
      configuration: {
        ...baseline.configuration,
        multiAccount: {
          ...baseline.configuration.multiAccount,
          enabled: true,
          state: "running",
        },
      },
    });
    const getManagementApiKey = vi.fn(async () => ({
      identity: {
        username: "Admin",
        credentialRevision: 1,
        apiKeyPrefix: "sak_v1_test_••••",
        apiKeyCreatedAt: snapshot.revision,
      },
      apiKey: "sak_v1_test_secret",
    }));
    renderSettingsPage("multiAccount", snapshot, { getManagementApiKey });

    await user.click(await screen.findByRole("button", { name: "获取并复制 API Key" }));

    expect(getManagementApiKey).toHaveBeenCalledWith();
    expect(screen.queryByLabelText("当前密码")).toBeNull();
    expect(await screen.findByDisplayValue("sak_v1_test_secret")).toBeInTheDocument();
  });

  it("明确清空二级代理口令时提交空字符串而不是保留标记", async () => {
    const user = userEvent.setup();
    const baseline = createServiceSnapshot();
    const snapshot = createServiceSnapshot({
      configuration: {
        ...baseline.configuration,
        upstreamProxy: {
          ...baseline.configuration.upstreamProxy,
          enabled: false,
          hasPassword: true,
        },
      },
    });
    const updateConfiguration = vi.fn(async () => snapshot);
    const { container } = renderSettingsPage("upstreamProxy", snapshot, {
      updateConfiguration,
    });
    await waitFor(() => {
      expect(
        container.querySelector('input[autocomplete="new-password"]'),
      ).not.toBeNull();
    });
    const submit = container.querySelector<HTMLButtonElement>(
      'button[type="submit"]',
    );
    expect(submit).not.toBeNull();

    await user.click(
      screen.getByRole("button", { name: "清除已保存口令" }),
    );
    await user.click(submit!);

    expect(updateConfiguration).toHaveBeenCalledWith(
      expect.objectContaining({
        upstreamProxy: expect.objectContaining({ password: "" }),
      }),
    );
  });
});
