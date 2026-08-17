import { describe, expect, it, vi } from "vitest";

import {
  type ManagedWindowPlatform,
  closeCurrentManagedWindow,
  createManagedWindowOptions,
  showManagedRouteWindow,
  showFloatingPanel,
  showMainWindow,
} from "@/platform/managedWindow";

/** 创建可观察的窗口平台夹具，集中复用桌面识别、查询和浏览器弹窗桩。 */
function createPlatformFixture(isDesktop: boolean) {
  const lifecycle: string[] = [];
  const windows = {
    main: {
      unminimize: vi.fn(async () => {
        lifecycle.push("main:unminimize");
      }),
      show: vi.fn(async () => {
        lifecycle.push("main:show");
      }),
      hide: vi.fn(async () => {
        lifecycle.push("main:hide");
      }),
      setFocus: vi.fn(async () => {
        lifecycle.push("main:setFocus");
      }),
      close: vi.fn(async () => {
        lifecycle.push("main:close");
      }),
    },
    floating: {
      unminimize: vi.fn(async () => {
        lifecycle.push("floating:unminimize");
      }),
      show: vi.fn(async () => {
        lifecycle.push("floating:show");
      }),
      hide: vi.fn(async () => {
        lifecycle.push("floating:hide");
      }),
      setFocus: vi.fn(async () => {
        lifecycle.push("floating:setFocus");
      }),
      close: vi.fn(async () => {
        lifecycle.push("floating:close");
      }),
    },
  };
  const managedWindow = windows.main;
  const platform: ManagedWindowPlatform = {
    isDesktop: () => isDesktop,
    findManagedWindow: vi.fn(async (windowLabel: string) => {
      return windowLabel === "floating" ? windows.floating : windows.main;
    }),
    createManagedWindow: vi.fn(async () => managedWindow),
    currentManagedWindow: vi.fn(() => managedWindow),
    openBrowserWindow: vi.fn(() => true),
    closeBrowserWindow: vi.fn(),
  };
  return { lifecycle, managedWindow, windows, platform };
}

describe("受管窗口平台边界", () => {
  it("动态业务窗口使用稳定的非透明原生窗口外观", () => {
    const options = createManagedWindowOptions({
      label: "app-window-protocol",
      path: "/window/dialog/protocol",
      title: "协议工具设置",
      width: 1_080,
      height: 760,
      minWidth: 720,
      minHeight: 520,
    });

    expect(options).toMatchObject({
      preventOverflow: { width: 24, height: 24 },
      focusable: true,
      decorations: true,
      transparent: false,
      shadow: true,
      titleBarStyle: "transparent",
      hiddenTitle: true,
      acceptFirstMouse: true,
      backgroundColor: [238, 240, 243, 255],
      visible: false,
    });
  });

  it("Tauri 运行时只恢复受管 floating 窗口", async () => {
    const fixture = createPlatformFixture(true);

    await showFloatingPanel(fixture.platform);

    expect(fixture.platform.findManagedWindow).toHaveBeenCalledWith("floating");
    expect(fixture.lifecycle).toEqual([
      "main:hide",
      "floating:unminimize",
      "floating:show",
      "floating:setFocus",
    ]);
    expect(fixture.platform.openBrowserWindow).not.toHaveBeenCalled();
  });

  it("普通浏览器只打开 floating 路由弹窗", async () => {
    const fixture = createPlatformFixture(false);

    await showFloatingPanel(fixture.platform);

    expect(fixture.platform.findManagedWindow).not.toHaveBeenCalled();
    expect(fixture.platform.openBrowserWindow).toHaveBeenCalledWith(
      "/floating",
      "floatingPanel",
      "popup=yes,width=340,height=250,resizable=yes",
    );
  });

  it("Tauri 运行时只恢复受管 main 窗口", async () => {
    const fixture = createPlatformFixture(true);

    await showMainWindow(fixture.platform);

    expect(fixture.platform.findManagedWindow).toHaveBeenCalledWith("main");
    expect(fixture.lifecycle).toEqual([
      "floating:hide",
      "main:unminimize",
      "main:show",
      "main:setFocus",
    ]);
    expect(fixture.platform.openBrowserWindow).not.toHaveBeenCalled();
  });

  it("并发切换请求按顺序执行，最终只保留最后一个窗口可见", async () => {
    const fixture = createPlatformFixture(true);

    await Promise.all([
      showFloatingPanel(fixture.platform),
      showMainWindow(fixture.platform),
    ]);

    expect(fixture.lifecycle).toEqual([
      "main:hide",
      "floating:unminimize",
      "floating:show",
      "floating:setFocus",
      "floating:hide",
      "main:unminimize",
      "main:show",
      "main:setFocus",
    ]);
  });

  it("普通浏览器只复用 connections 主窗口", async () => {
    const fixture = createPlatformFixture(false);

    await showMainWindow(fixture.platform);

    expect(fixture.platform.findManagedWindow).not.toHaveBeenCalled();
    expect(fixture.platform.openBrowserWindow).toHaveBeenCalledWith(
      "/connections",
      "mainWindow",
      undefined,
    );
  });

  it("受管窗口缺失时返回包含目标标签的明确错误", async () => {
    const fixture = createPlatformFixture(true);
    fixture.platform.findManagedWindow = vi.fn(async () => null);

    await expect(showMainWindow(fixture.platform)).rejects.toThrow(
      "未找到受管窗口：main",
    );
  });

  it("Tauri 运行时创建并聚焦缺失的动态业务窗口", async () => {
    const fixture = createPlatformFixture(true);
    fixture.platform.findManagedWindow = vi.fn(async () => null);
    const target = {
      label: "app-window-settings-interface" as const,
      path: "/window/settings/interface",
      title: "设置",
      width: 1_120,
      height: 780,
      minWidth: 880,
      minHeight: 620,
    };

    await showManagedRouteWindow(target, fixture.platform);

    expect(fixture.platform.createManagedWindow).toHaveBeenCalledWith(target);
    expect(fixture.lifecycle).toEqual([
      "main:unminimize",
      "main:show",
      "main:setFocus",
    ]);
  });

  it("浏览器调试态用 label 复用动态业务窗口", async () => {
    const fixture = createPlatformFixture(false);
    const target = {
      label: "app-window-protocol" as const,
      path: "/window/dialog/protocol",
      title: "协议工具设置",
      width: 1_080,
      height: 760,
      minWidth: 720,
      minHeight: 520,
    };

    await showManagedRouteWindow(target, fixture.platform);

    expect(fixture.platform.openBrowserWindow).toHaveBeenCalledWith(
      target.path,
      target.label,
      "popup=yes,width=1080,height=760,resizable=yes",
    );
  });

  it("按运行环境关闭当前独立窗口", async () => {
    const desktopFixture = createPlatformFixture(true);
    await closeCurrentManagedWindow(desktopFixture.platform);
    expect(desktopFixture.lifecycle).toEqual(["main:close"]);

    const browserFixture = createPlatformFixture(false);
    await closeCurrentManagedWindow(browserFixture.platform);
    expect(browserFixture.platform.closeBrowserWindow).toHaveBeenCalledOnce();
  });
});
