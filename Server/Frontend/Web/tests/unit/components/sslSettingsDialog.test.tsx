import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type {
  EventClientCallbacks,
  EventStreamClient,
} from "@/api/eventClient";
import type { SslConfiguration } from "@/api/protocol";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";
import { SslSettingsDialog } from "@/components/sslSettingsDialog";

/**
 * 建立立即可用的事件通道；本组测试只验证控制请求完成后的权威快照刷新。
 */
function createConnectedEventClient(): EventStreamClient {
  return {
    start(callbacks: EventClientCallbacks) {
      callbacks.onConnectionState("connected", "事件流已连接");
    },
    stop() {},
  };
}

describe("SSL 代理设置对话框", () => {
  it("从右键添加入口预填客户端证书主机并定位文件控件", async () => {
    const currentSnapshot = createServiceSnapshot();
    render(
      <ServiceProvider
        controlClient={createControlClientStub(currentSnapshot)}
        eventClient={createConnectedEventClient()}
      >
        <SslSettingsDialog
          focusClientCertificate
          initialLocation={{
            protocol: "https",
            host: "secure.example",
            port: "8443",
            path: "/account",
            query: null,
          }}
          open
          onClose={() => undefined}
        />
      </ServiceProvider>,
    );

    expect(await screen.findByLabelText("名称")).toHaveValue("secure.example");
    expect(screen.getByLabelText("目标主机")).toHaveValue("secure.example");
    expect(screen.getByLabelText("端口（可选）")).toHaveValue(8443);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "P12 / PFX：选择文件" }),
      ).toHaveFocus(),
    );
  });

  it("文件按钮在同一用户手势内直接打开系统选择器", async () => {
    const user = userEvent.setup();
    const currentSnapshot = createServiceSnapshot();
    const { container } = render(
      <ServiceProvider
        controlClient={createControlClientStub(currentSnapshot)}
        eventClient={createConnectedEventClient()}
      >
        <SslSettingsDialog open onClose={() => undefined} />
      </ServiceProvider>,
    );

    await screen.findByText("客户端证书");
    // 设置窗口只保留页脚的取消与应用，标题栏关闭和重复确认动作不得再次出现。
    expect(container.querySelector(".sslDialogHeader button")).toBeNull();
    expect(container.querySelectorAll(".sslDialogFooter > button")).toHaveLength(2);
    const certificateInput = container.querySelector<HTMLInputElement>(
      '.sslClientCertificateForm input[type="file"]',
    );
    if (certificateInput === null) {
      throw new Error("测试夹具缺少客户端证书文件输入");
    }
    const showPicker = vi.fn();
    Object.defineProperty(certificateInput, "showPicker", {
      configurable: true,
      value: showPicker,
    });

    await user.click(
      screen.getByRole("button", { name: "P12 / PFX：选择文件" }),
    );

    expect(showPicker).toHaveBeenCalledOnce();
  });

  it("从事务右键上下文自动填入精确的 SSL 包含位置", async () => {
    const user = userEvent.setup();
    const currentSnapshot = createServiceSnapshot();
    const updateSsl = vi.fn(async () => currentSnapshot.ssl);
    const controlClient = createControlClientStub(currentSnapshot, {
      updateSsl,
    });

    render(
      <ServiceProvider
        controlClient={controlClient}
        eventClient={createConnectedEventClient()}
      >
        <SslSettingsDialog
          initialLocation={{
            protocol: "https",
            host: "secure.example",
            port: "443",
            path: "/api/profile",
            query: "view=full",
          }}
          open
          onClose={() => undefined}
        />
      </ServiceProvider>,
    );

    expect(
      await screen.findByRole("button", { name: "包含主机 1" }),
    ).toHaveTextContent("secure.example");
    await user.click(screen.getByRole("button", { name: "应用" }));
    await waitFor(() => expect(updateSsl).toHaveBeenCalledTimes(1));
    expect(updateSsl).toHaveBeenCalledWith({
      enabled: false,
      includeLocations: [
        {
          protocol: "https",
          host: "secure.example",
          port: "443",
          path: "",
          query: null,
        },
      ],
      excludeLocations: [],
      maxCachedCertificates: 256,
      useClientSni: true,
    });
  });

  it("提交显式主机规则并经二次确认更换根证书", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateSsl = vi.fn(async (configuration: SslConfiguration) => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        ssl: {
          ...currentSnapshot.ssl,
          ...configuration,
        },
      };
      return currentSnapshot.ssl;
    });
    const regenerateSslRoot = vi.fn(async () => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        ssl: {
          ...currentSnapshot.ssl,
          ca: {
            ...currentSnapshot.ssl.ca,
            fingerprintSha256: "AA:BB:CC:DD",
          },
          cachedLeafCount: 0,
        },
      };
      return currentSnapshot.ssl;
    });
    const controlClient = createControlClientStub(currentSnapshot, {
      getSnapshot: async () => currentSnapshot,
      updateSsl,
      regenerateSslRoot,
    });

    render(
      <ServiceProvider
        controlClient={controlClient}
        eventClient={createConnectedEventClient()}
      >
        <SslSettingsDialog open onClose={() => undefined} />
      </ServiceProvider>,
    );

    expect(
      await screen.findByRole("dialog", { name: "SSL 代理设置" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("checkbox", { name: /启用 SSL 代理/ }));
    await user.click(
      screen.getAllByRole("button", { name: "添加规则" })[0],
    );
    await user.type(
      screen.getByRole("textbox", { name: "包含主机" }),
      "*.example.com",
    );
    await user.type(
      screen.getByRole("textbox", { name: "包含主机 端口" }),
      "443",
    );
    await user.click(screen.getByRole("button", { name: "保存规则" }));
    await user.click(screen.getByRole("button", { name: "应用" }));

    await waitFor(() => expect(updateSsl).toHaveBeenCalledTimes(1));
    expect(updateSsl).toHaveBeenCalledWith({
      enabled: true,
      includeLocations: [
        {
          protocol: "https",
          host: "*.example.com",
          port: "443",
          path: "",
          query: null,
        },
      ],
      excludeLocations: [],
      maxCachedCertificates: 256,
      useClientSni: true,
    });

    await user.click(
      screen.getByRole("button", { name: "更换根证书" }),
    );
    expect(
      screen.getByRole("dialog", { name: "更换根证书？" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "更换" }));
    await waitFor(() =>
      expect(regenerateSslRoot).toHaveBeenCalledTimes(1),
    );
    expect(
      screen.queryByRole("dialog", { name: "更换根证书？" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("AA:BB:CC:DD")).toBeInTheDocument();
  });

  it("上传按主机匹配的 PKCS#12 客户端身份且不在快照保留口令", async () => {
    const user = userEvent.setup();
    const currentSnapshot = createServiceSnapshot();
    const importClientCertificate = vi.fn(async () => currentSnapshot.ssl);
    const controlClient = createControlClientStub(currentSnapshot, { importClientCertificate });
    const { container } = render(
      <ServiceProvider controlClient={controlClient} eventClient={createConnectedEventClient()}>
        <SslSettingsDialog open onClose={() => undefined} />
      </ServiceProvider>,
    );

    await screen.findByText("客户端证书");
    await user.type(screen.getByLabelText("名称"), "支付接口身份");
    await user.type(screen.getByLabelText("目标主机"), "api.example.com");
    const certificateFile = new File([new Uint8Array([1, 2, 3])], "client.p12", { type: "application/x-pkcs12" });
    await user.upload(container.querySelector<HTMLInputElement>('.sslClientCertificateForm input[type="file"]')!, certificateFile);
    await user.type(container.querySelector<HTMLInputElement>('.sslClientCertificateForm input[type="password"]')!, "secret");
    const importButton = screen.getByRole("button", { name: "导入身份" });
    await waitFor(() => expect(importButton).toBeEnabled());
    fireEvent.submit(container.querySelector<HTMLFormElement>(".sslClientCertificateForm")!);

    await waitFor(() => expect(importClientCertificate).toHaveBeenCalledOnce());
    expect(importClientCertificate).toHaveBeenCalledWith(expect.objectContaining({
      name: "支付接口身份",
      format: "pkcs12",
      password: "secret",
      certificate: certificateFile,
      locations: [expect.objectContaining({ host: "api.example.com" })],
    }));
    expect(JSON.stringify(currentSnapshot)).not.toContain("secret");
  });
});
