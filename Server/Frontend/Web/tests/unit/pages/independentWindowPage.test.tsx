import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { IndependentDialogWindowPage } from "@/pages/independentWindowPage";

const storeMocks = vi.hoisted(() => ({
  advancedRepeats: [] as Array<Record<string, unknown>>,
  clearRecording: vi.fn(),
  getAdvancedRepeat: vi.fn(),
  getTransactionBody: vi.fn(),
  getTransactionDetail: vi.fn(),
  repeatTransaction: vi.fn(),
  startAdvancedRepeat: vi.fn(),
  uninstallPlugin: vi.fn(),
  validateResponse: vi.fn(),
}));
const windowMocks = vi.hoisted(() => ({ close: vi.fn() }));
const eventMocks = vi.hoisted(() => ({ publish: vi.fn() }));

vi.mock("@/state/serviceStore", () => ({
  useServiceStore: () => ({
    actionPending: false,
    activeAction: null,
    lastError: null,
    snapshot: {
      advancedRepeats: storeMocks.advancedRepeats,
      recording: { transactionCount: 1 },
    },
    suspendedBreakpoints: [],
    cancelAdvancedRepeat: vi.fn(),
    clearRecording: storeMocks.clearRecording,
    getAdvancedRepeat: storeMocks.getAdvancedRepeat,
    getTransactionBody: storeMocks.getTransactionBody,
    getTransactionDetail: storeMocks.getTransactionDetail,
    getLiveTransactionDetail: storeMocks.getTransactionDetail,
    repeatTransaction: storeMocks.repeatTransaction,
    startAdvancedRepeat: storeMocks.startAdvancedRepeat,
    uninstallPlugin: storeMocks.uninstallPlugin,
    validateResponse: storeMocks.validateResponse,
  }),
}));

vi.mock("@/platform/managedWindow", () => ({
  closeCurrentManagedWindow: windowMocks.close,
}));

vi.mock("@/platform/independentWindowEvents", () => ({
  publishIndependentWindowResult: eventMocks.publish,
}));

/** 在内存路由中渲染指定独立窗口，确保查询参数经过真实页面边界解析。 */
function renderIndependentWindow(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route
          path="/window/dialog/:dialogKind"
          element={<IndependentDialogWindowPage />}
        />
      </Routes>
    </MemoryRouter>,
  );
}

describe("独立命令窗口", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.localStorage.clear();
    windowMocks.close.mockResolvedValue(undefined);
    storeMocks.getTransactionDetail.mockResolvedValue({
      transaction: {
        transactionId: "transaction-9",
        method: "GET",
        urlDisplay: "https://www.example.com/resource",
      },
      requestHeaders: [],
    });
    storeMocks.getTransactionBody.mockResolvedValue({ base64: "" });
    storeMocks.repeatTransaction.mockResolvedValue({
      transactionId: "repeated-1",
    });
    storeMocks.advancedRepeats = [];
    storeMocks.startAdvancedRepeat.mockResolvedValue({
      jobId: "00000000-0000-4000-8000-000000000001",
      state: "queued",
      plan: {
        name: "GET https://www.example.com/resource",
        base: {
          method: "GET",
          url: "https://www.example.com/resource",
          headers: [],
          bodyBase64: "",
          viaProxy: true,
        },
        concurrency: 1,
        totalIterations: 10,
        intervalMilliseconds: 0,
        recordEach: true,
        stopOnError: false,
      },
      startedAtMilliseconds: 1,
      finishedAtMilliseconds: null,
      completedIterations: 0,
      successCount: 0,
      failureCount: 0,
      latencyMilliseconds: { min: 0, max: 0, p50: 0, p95: 0, p99: 0 },
      lastError: null,
    });
  });

  it("无效窗口使用固定标题、正文和底栏轨道反馈", () => {
    renderIndependentWindow("/window/dialog/unknown");

    const feedback = screen.getByRole("alert");
    expect(feedback).toHaveClass("independentWindowFeedback");
    expect(
      feedback.querySelector(".independentWindowFeedbackHeader"),
    ).not.toBeNull();
    expect(
      feedback.querySelector(".independentWindowFeedbackBody"),
    ).not.toBeNull();
    expect(
      feedback.querySelector(".independentWindowFeedbackActions"),
    ).not.toBeNull();
  });

  it("没有挂起消息时展示明确状态而不是空白窗口", () => {
    renderIndependentWindow("/window/dialog/breakpoint-hit");
    expect(screen.getByRole("status")).toHaveTextContent("没有待处理的断点");
  });

  it("确认在线校验后调用真实事务校验并发布结果", async () => {
    storeMocks.validateResponse.mockResolvedValue({
      validatorId: "w3cHtmlOnline",
      issues: [],
      validatedAtMilliseconds: 1,
    });
    renderIndependentWindow(
      "/window/dialog/online-validation?transactionId=transaction-9&validatorId=w3cHtmlOnline",
    );

    fireEvent.click(screen.getAllByRole("button")[1]);

    await waitFor(() =>
      expect(storeMocks.validateResponse).toHaveBeenCalledWith(
        "transaction-9",
        {
          validatorId: "w3cHtmlOnline",
          onlineUploadConfirmed: true,
        },
      ),
    );
    expect(eventMocks.publish).toHaveBeenCalledWith({
      kind: "onlineValidation",
      transactionId: "transaction-9",
    });
  });

  it("确认插件卸载后调用宿主命令并关闭窗口", async () => {
    storeMocks.uninstallPlugin.mockResolvedValue(true);
    renderIndependentWindow(
      "/window/dialog/plugin-uninstall?pluginId=native-capture&pluginName=%E6%9C%AC%E5%9C%B0%E6%8A%93%E5%8C%85",
    );

    fireEvent.click(screen.getAllByRole("button")[1]);

    await waitFor(() =>
      expect(storeMocks.uninstallPlugin).toHaveBeenCalledWith("native-capture"),
    );
    expect(eventMocks.publish).toHaveBeenCalledWith({
      kind: "pluginUninstall",
      pluginId: "native-capture",
    });
    expect(windowMocks.close).toHaveBeenCalledOnce();
  });

  it("编辑重复窗口加载原事务并提交完整覆盖请求", async () => {
    renderIndependentWindow(
      "/window/dialog/repeat?transactionId=transaction-9&mode=edit",
    );

    await screen.findByDisplayValue("https://www.example.com/resource");
    fireEvent.click(screen.getByRole("button", { name: /发送|send/i }));

    await waitFor(() =>
      expect(storeMocks.repeatTransaction).toHaveBeenCalledWith(
        "transaction-9",
        expect.objectContaining({
          method: "GET",
          url: "https://www.example.com/resource",
          viaProxy: true,
        }),
      ),
    );
    expect(windowMocks.close).toHaveBeenCalledOnce();
  });

  it("高级重复通过实时快照更新进度且不轮询作业端点", async () => {
    const rendered = renderIndependentWindow(
      "/window/dialog/repeat?transactionId=transaction-9&mode=advanced",
    );
    await screen.findByDisplayValue("https://www.example.com/resource");
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "我确认此任务会发送以上配置的请求。",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "开始执行" }));
    await screen.findByText("排队中");

    storeMocks.advancedRepeats = [
      {
        ...(await storeMocks.startAdvancedRepeat.mock.results[0]?.value),
        state: "running",
        completedIterations: 4,
        successCount: 4,
      },
    ];
    rendered.rerender(
      <MemoryRouter
        initialEntries={[
          "/window/dialog/repeat?transactionId=transaction-9&mode=advanced",
        ]}
      >
        <Routes>
          <Route
            path="/window/dialog/:dialogKind"
            element={<IndependentDialogWindowPage />}
          />
        </Routes>
      </MemoryRouter>,
    );

    await screen.findByText("执行中");
    expect(screen.getByText("4 / 10 (40%)")).toBeVisible();
    expect(storeMocks.getAdvancedRepeat).not.toHaveBeenCalled();
  });

  it("独立清理确认窗口调用真实清理命令并保存免提醒偏好", async () => {
    storeMocks.clearRecording.mockResolvedValue(undefined);
    renderIndependentWindow(
      "/window/dialog/clear-recording?transactionCount=1",
    );

    fireEvent.click(
      screen.getByRole("checkbox", { name: "下次清空时不再提醒" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "清空" }));

    await waitFor(() =>
      expect(storeMocks.clearRecording).toHaveBeenCalledOnce(),
    );
    expect(
      window.localStorage.getItem("capture.recording.skipClearConfirmation"),
    ).toBe("true");
  });
});
