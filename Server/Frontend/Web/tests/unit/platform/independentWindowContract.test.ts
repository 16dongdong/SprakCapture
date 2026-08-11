import { describe, expect, it, vi } from "vitest";

import {
  createIndependentWindowTarget,
  readListenerDialogId,
  readSettingsSection,
  readToolDialogId,
  readTransactionSeed,
  showIndependentWindow,
} from "@/platform/independentWindowContract";
import type { ManagedWindowPlatform } from "@/platform/managedWindow";

/** 创建浏览器平台夹具；contract 测试只观察路由和命名窗口参数，不触发真实弹窗。 */
function createBrowserPlatform(): ManagedWindowPlatform {
  const managedWindow = {
    unminimize: vi.fn(async () => undefined),
    show: vi.fn(async () => undefined),
    setFocus: vi.fn(async () => undefined),
    close: vi.fn(async () => undefined),
  };
  return {
    isDesktop: () => false,
    findManagedWindow: vi.fn(async () => managedWindow),
    createManagedWindow: vi.fn(async () => managedWindow),
    currentManagedWindow: vi.fn(() => managedWindow),
    openBrowserWindow: vi.fn(),
    closeBrowserWindow: vi.fn(),
  };
}

describe("独立业务窗口 contract", () => {
  it("进程选择器使用独立窗口并保持与设置相同的工具栏层级", () => {
    expect(createIndependentWindowTarget({ kind: "processManager" })).toMatchObject(
      {
        label: "app-window-process-manager",
        path: "/window/dialog/processes",
        title: "进程选择器",
        width: 1180,
        height: 760,
      },
    );
  });

  it("设置窗口使用受控前缀和独立路由", () => {
    expect(
      createIndependentWindowTarget({
        kind: "settings",
        section: "interface",
      }),
    ).toMatchObject({
      label: "app-window-settings-interface",
      path: "/window/settings/interface",
      title: "设置",
    });
  });

  it("工具窗口完整往返事务位置与具体路径", () => {
    const target = createIndependentWindowTarget({
      kind: "tool",
      tool: "mapLocal",
      seed: {
        transactionId: "transaction-7",
        contentType: "application/json",
        location: {
          protocol: "https",
          host: "www.example.com",
          port: "443",
          path: "/assets/config.json",
          query: "version=2",
        },
      },
    });
    const parameters = new URL(target.path, "http://localhost").searchParams;

    expect(target.label).toMatch(/^app-window-tool-mapLocal-/);
    expect(readToolDialogId(parameters.get("tool"))).toBe("mapLocal");
    expect(readTransactionSeed(parameters)).toEqual({
      transactionId: "transaction-7",
      contentType: "application/json",
      location: {
        protocol: "https",
        host: "www.example.com",
        port: "443",
        path: "/assets/config.json",
        query: "version=2",
      },
    });
  });

  it("客户端证书入口保留主机种子并使用独立窗口焦点参数", () => {
    const target = createIndependentWindowTarget({
      kind: "ssl",
      focusClientCertificate: true,
      seed: {
        transactionId: "transaction-client-cert",
        contentType: "application/octet-stream",
        location: {
          protocol: "https",
          host: "secure.example",
          port: "443",
          path: "/",
          query: null,
        },
      },
    });
    const parameters = new URL(target.path, "http://localhost").searchParams;

    expect(target.label).toMatch(/^app-window-ssl-/);
    expect(parameters.get("focus")).toBe("clientCertificate");
    expect(readTransactionSeed(parameters)?.location).toMatchObject({
      host: "secure.example",
      port: "443",
    });
  });

  it("拒绝未知设置、监听器与工具参数", () => {
    expect(readSettingsSection("unknown")).toBe("interface");
    expect(readListenerDialogId("udpForward")).toBeNull();
    expect(readToolDialogId("unknown")).toBeNull();
  });

  it("浏览器调用使用命名窗口并保留监听器类型", async () => {
    const platform = createBrowserPlatform();
    await showIndependentWindow(
      { kind: "listener", listener: "portForwards" },
      platform,
    );

    expect(platform.openBrowserWindow).toHaveBeenCalledWith(
      "/window/dialog/listener?listener=portForwards",
      "app-window-listener-portForwards",
      "popup=yes,width=920,height=560,resizable=yes",
    );
  });

  it("为在线校验、插件卸载、重复编辑和清空事务生成真实命令路由", () => {
    expect(
      createIndependentWindowTarget({
        kind: "onlineValidation",
        transactionId: "transaction-8",
        validatorId: "w3cHtmlOnline",
      }).path,
    ).toContain("/window/dialog/online-validation?");
    expect(
      createIndependentWindowTarget({
        kind: "pluginUninstall",
        pluginId: "native-capture",
        pluginName: "本地抓包",
      }).path,
    ).toContain("pluginId=native-capture");
    expect(
      createIndependentWindowTarget({
        kind: "repeat",
        transactionId: "transaction-8",
        mode: "advanced",
      }),
    ).toMatchObject({
      label: expect.stringMatching(/^app-window-repeat-advanced-/),
      title: "高级重复",
    });
    expect(
      createIndependentWindowTarget({
        kind: "clearRecording",
        transactionCount: 24,
      }).path,
    ).toBe("/window/dialog/clear-recording?transactionCount=24");
  });
});
