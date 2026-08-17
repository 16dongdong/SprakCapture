import { render, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  UiContextReporterProvider,
  useReportUiDataView,
} from "@/state/uiContextReporter";

/** 在测试页面中上报一条事务选择；组件不直接接触 HTTP 客户端。 */
function SelectedTransaction() {
  useReportUiDataView("contents", {
    kind: "transaction",
    ids: ["transaction-alpha"],
    side: null,
    sequence: null,
  });
  return <span>已选择事务</span>;
}

describe("界面上下文上报器", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  /** 路由和业务选择应合并成同一帧，不得把完整浏览器 URL 或正文发送到控制面。 */
  it("上报连接页当前事务与检查器页签", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockImplementation(async () =>
      new Response(JSON.stringify({ primary: null, contexts: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", requestFetch);

    render(
      <MemoryRouter initialEntries={["/connections"]}>
        <UiContextReporterProvider>
          <SelectedTransaction />
        </UiContextReporterProvider>
      </MemoryRouter>,
    );

    await waitFor(() => {
      const payloads = requestFetch.mock.calls.map((call) =>
        JSON.parse(String(call[1]?.body)),
      );
      expect(payloads).toContainEqual(
        expect.objectContaining({
          page: "connections",
          windowKind: "main",
          view: "contents",
          selection: {
            kind: "transaction",
            ids: ["transaction-alpha"],
            side: null,
            sequence: null,
          },
        }),
      );
    });
    expect(requestFetch).toHaveBeenCalledWith(
      "http://127.0.0.1:17890/api/v1/ui/context",
      expect.objectContaining({ method: "PUT" }),
    );
  });

  /** 插件独立窗口必须把插件 ID 作为稳定 section 上报，MCP 才能区分正在操作的插件。 */
  it("上报插件独立窗口及当前插件标识", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockImplementation(async () =>
      new Response(JSON.stringify({ primary: null, contexts: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", requestFetch);

    render(
      <MemoryRouter initialEntries={["/window/plugin/sample.plugin"]}>
        <UiContextReporterProvider>
          <span>插件窗口</span>
        </UiContextReporterProvider>
      </MemoryRouter>,
    );

    await waitFor(() => {
      const payloads = requestFetch.mock.calls.map((call) =>
        JSON.parse(String(call[1]?.body)),
      );
      expect(payloads).toContainEqual(
        expect.objectContaining({
          page: "plugins",
          section: "sample.plugin",
          windowKind: "independent",
        }),
      );
    });
  });
});
