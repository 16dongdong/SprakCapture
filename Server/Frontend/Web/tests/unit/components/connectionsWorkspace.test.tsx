import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EncodedBodyResponse,
  TransactionDetail,
  TransactionSummary,
} from "@/api/protocol";
import {
  createServiceSnapshot,
  createTransactionSummary,
} from "#tests/testFixtures";
import { ConnectionsWorkspace } from "@/components/connectionsWorkspace";
import { TransactionNavigator } from "@/components/transactionNavigator";

const storeMocks = vi.hoisted(() => ({
  decodeProtobuf: vi.fn(),
  getTransactionDetail: vi.fn(),
  getTransactionBody: vi.fn(),
  getProcesses: vi.fn(),
  listTransactions: vi.fn(),
  getValidateConfiguration: vi.fn(),
  getValidationReports: vi.fn(),
  exportRecording: vi.fn(),
  refresh: vi.fn(),
  repeatTransaction: vi.fn(),
  snapshot: null as ReturnType<typeof createServiceSnapshot> | null,
  validateResponse: vi.fn(),
}));

vi.mock("@/state/serviceStore", () => ({
  useServiceStore: () => ({
    snapshot: storeMocks.snapshot,
    lastError: null,
    refresh: storeMocks.refresh,
    decodeProtobuf: storeMocks.decodeProtobuf,
    getTransactionDetail: storeMocks.getTransactionDetail,
    getLiveTransactionDetail: storeMocks.getTransactionDetail,
    getTransactionBody: storeMocks.getTransactionBody,
    getProcesses: storeMocks.getProcesses,
    listTransactions: storeMocks.listTransactions,
    getValidateConfiguration: storeMocks.getValidateConfiguration,
    getValidationReports: storeMocks.getValidationReports,
    exportRecording: storeMocks.exportRecording,
    validateResponse: storeMocks.validateResponse,
    repeatTransaction: storeMocks.repeatTransaction,
  }),
}));

/**
 * 创建覆盖默认端口、非默认端口和多种资源类型的事务，用于验证结构分组与检查器选择。
 */
function createWorkspaceTransactions(): TransactionSummary[] {
  return [
    createTransactionSummary({
      transactionId: "transaction-one",
      sequence: 1,
      host: "api.example",
      port: 80,
      path: "/v1/users/one",
      urlDisplay: "http://api.example/v1/users/one",
      clientProcessName: "client.exe",
      clientProcessId: 42,
    }),
    createTransactionSummary({
      transactionId: "transaction-two",
      sequence: 2,
      method: "POST",
      host: "api.example",
      port: 8080,
      path: "/v1/users/two",
      urlDisplay: "http://api.example:8080/v1/users/two",
      contentType: "application/json",
      sizes: {
        requestHeaderBytes: 120,
        requestBodyBytes: 12,
        responseHeaderBytes: 130,
        responseBodyBytes: 18,
      },
    }),
    createTransactionSummary({
      transactionId: "transaction-three",
      sequence: 3,
      protocol: "https",
      host: "secure.example",
      port: 443,
      path: "/health",
      urlDisplay: "https://secure.example/health",
      status: "pending",
      statusCode: null,
    }),
  ];
}

/**
 * 按来源到目录的顺序展开 Charles 结构树；每层初始收起，因此测试必须复现真实用户的逐级操作。
 */
async function expandTreePath(
  user: ReturnType<typeof userEvent.setup>,
  ...nodeLabels: string[]
): Promise<void> {
  for (const nodeLabel of nodeLabels) {
    await user.click(screen.getByRole("button", { name: `展开 ${nodeLabel}` }));
  }
}

beforeEach(() => {
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({
      matches: false,
      media: "(max-width: 960px)",
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })),
  );
  vi.stubGlobal(
    "ResizeObserver",
    vi.fn(() => ({
      observe: vi.fn(),
      unobserve: vi.fn(),
      disconnect: vi.fn(),
    })),
  );
  const transactions = createWorkspaceTransactions();
  storeMocks.getProcesses.mockResolvedValue({
    enabled: true,
    selectedPaths: ["C:\\Apps\\client.exe"],
    resolvedProcessIds: [42],
    processes: [
      {
        processId: 42,
        name: "client.exe",
        executablePath: "C:\\Apps\\client.exe",
      },
    ],
    processIcons: {
      "c:\\apps\\client.exe": "data:image/png;base64,aWNvbg==",
    },
  });
  const baseSnapshot = createServiceSnapshot();
  storeMocks.snapshot = createServiceSnapshot({
    recording: {
      ...baseSnapshot.recording,
      transactionCount: transactions.length,
    },
    transactions: {
      ...baseSnapshot.transactions,
      total: transactions.length,
      items: transactions,
    },
  });
  storeMocks.getTransactionDetail.mockImplementation(
    async (transactionId: string): Promise<TransactionDetail> => {
      const transaction = transactions.find(
        (candidate) => candidate.transactionId === transactionId,
      );
      if (transaction === undefined) {
        throw new Error("测试事务不存在");
      }
      const rawStream = transactionId === "transaction-one";
      return {
        revision: 1,
        transaction,
        requestHeaders: rawStream
          ? []
          : [{ name: "content-type", value: transaction.contentType }],
        responseHeaders: [{ name: "server", value: "fixture" }],
        requestBody: {
          transactionId,
          side: "request",
          contentType: rawStream
            ? "application/octet-stream"
            : "application/json",
          encoding: rawStream ? "binary" : "identity",
          storedBytes: 11,
          originalBytes: 11,
          truncated: false,
        },
        requestPackets: [],
        responsePackets: [],
        responseBody: null,
      };
    },
  );
  storeMocks.exportRecording.mockReset().mockResolvedValue(new Blob(["{}"]));
  storeMocks.repeatTransaction
    .mockReset()
    .mockResolvedValue({ transactionId: "repeated" });
  storeMocks.getTransactionBody.mockImplementation(
    async (
      transactionId: string,
      side: "request" | "response",
    ): Promise<EncodedBodyResponse> => ({
      revision: 1,
      meta: {
        transactionId,
        side,
        contentType: "application/json",
        encoding: "identity",
        storedBytes: 11,
        originalBytes: 11,
        truncated: false,
      },
      base64: window.btoa('{"ok":true}'),
    }),
  );
  storeMocks.refresh.mockReset();
  storeMocks.listTransactions.mockReset();
  storeMocks.decodeProtobuf.mockReset().mockResolvedValue({
    messageType: null,
    json: null,
    decodeError: "protobufDisabled",
  });
  storeMocks.getValidateConfiguration.mockReset().mockResolvedValue({
    enabled: true,
    validators: [{ id: "htmlWellFormed", enabled: true }],
    allowOnlineValidators: false,
    w3cEndpoint: "https://validator.w3.org/nu/?out=json",
  });
  storeMocks.getValidationReports.mockReset().mockResolvedValue([]);
  storeMocks.validateResponse.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("事务工作台", () => {


  it("同一媒体 URL 的 Range 分片在结构视图聚合为一个持续更新资源", async () => {
    const basePage = createServiceSnapshot().transactions;
    const mediaSegments = [1, 2].map((sequence) =>
      createTransactionSummary({
        transactionId: `media-segment-${sequence}`,
        sequence,
        protocol: "https",
        method: "GET",
        host: "media.example",
        port: 443,
        path: "/song.mp4",
        urlDisplay: "https://media.example/song.mp4",
        statusCode: 206,
        contentType: "audio/mp4",
      }),
    );
    render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          total: mediaSegments.length,
          limit: mediaSegments.length,
          items: mediaSegments,
        }}
      />,
    );

    await userEvent.click(
      screen.getByRole("button", { name: /https:\/\/media\.example/ }),
    );
    expect(screen.getAllByText("song.mp4")).toHaveLength(1);

    await userEvent.click(screen.getByRole("tab", { name: /序列|搴忓垪/ }));
    expect(
      screen.getAllByRole("button", {
        name: /https:\/\/media\.example\/song\.mp4/,
      }),
    ).toHaveLength(2);
  });
  it("自动补齐实时快照省略的较早事务", async () => {
    const basePage = createServiceSnapshot().transactions;
    const olderTransaction = createTransactionSummary({
      transactionId: "older-transaction",
      sequence: 1,
      host: "history.example",
      path: "/older",
      urlDisplay: "http://history.example/older",
    });
    const latestTransaction = createTransactionSummary({
      transactionId: "latest-transaction",
      sequence: 2,
      host: "latest.example",
      path: "/latest",
      urlDisplay: "http://latest.example/latest",
    });
    storeMocks.listTransactions.mockResolvedValue({
      ...basePage,
      collectionToken: "stable-collection",
      total: 2,
      offset: 0,
      limit: 1,
      hasPrevious: false,
      hasMore: true,
      nextOffset: 1,
      truncated: true,
      items: [olderTransaction],
    });
    render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "stable-collection",
          total: 2,
          offset: 1,
          limit: 1,
          hasPrevious: true,
          hasMore: false,
          nextOffset: null,
          truncated: true,
          items: [latestTransaction],
        }}
      />,
    );

    await waitFor(() =>
      expect(storeMocks.listTransactions).toHaveBeenCalledWith(
        { offset: 0, limit: 1, collectionToken: "stable-collection" },
        expect.any(AbortSignal),
      ),
    );
    await waitFor(() =>
      expect(screen.queryByText("正在加载更早的事务…")).not.toBeInTheDocument(),
    );
    expect(
      screen.queryByText("事务集合大于当前可见范围。"),
    ).not.toBeInTheDocument();
    expect(screen.getByTitle(/http:\/\/history\.example/)).toBeVisible();
    expect(screen.getByTitle(/http:\/\/latest\.example/)).toBeVisible();
  });

  it("超过五百条的历史自动补齐到同一个滚动集合", async () => {
    const basePage = createServiceSnapshot().transactions;
    const transactions = Array.from({ length: 501 }, (_, itemIndex) => {
      const sequence = itemIndex + 1;
      return createTransactionSummary({
        transactionId: `window-${sequence}`,
        sequence,
        host: "window.example",
        path: `/${sequence}`,
        urlDisplay: `http://window.example/${sequence}`,
      });
    });
    storeMocks.listTransactions.mockResolvedValue({
      ...basePage,
      collectionToken: "window-generation",
      total: 501,
      offset: 0,
      limit: 1,
      hasPrevious: false,
      hasMore: true,
      nextOffset: 1,
      truncated: true,
      items: [transactions[0]],
    });
    render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "window-generation",
          total: 501,
          offset: 1,
          limit: 500,
          hasPrevious: true,
          hasMore: false,
          nextOffset: null,
          truncated: false,
          items: transactions.slice(1),
        }}
      />,
    );

    await waitFor(() =>
      expect(storeMocks.listTransactions).toHaveBeenCalledWith(
        { offset: 0, limit: 1, collectionToken: "window-generation" },
        expect.any(AbortSignal),
      ),
    );
    await userEvent.click(screen.getByRole("tab", { name: "序列" }));
    await waitFor(() =>
      expect(
        screen.getByRole("button", {
          name: "GET http://window.example/1 文档",
        }),
      ).toBeVisible(),
    );
    expect(
      screen.getByRole("button", {
        name: "GET http://window.example/501 文档",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "更早" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "较新" }),
    ).not.toBeInTheDocument();
  });

  it("实时尾部越过五百条后直接追加到当前滚动集合", async () => {
    const basePage = createServiceSnapshot().transactions;
    const transactions = Array.from({ length: 501 }, (_, itemIndex) => {
      const sequence = itemIndex + 1;
      return createTransactionSummary({
        transactionId: `stable-page-${sequence}`,
        sequence,
        host: "stable-page.example",
        path: `/${sequence}`,
        urlDisplay: `http://stable-page.example/${sequence}`,
      });
    });
    const { rerender } = render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "stable-live-page",
          total: 500,
          offset: 0,
          limit: 500,
          hasPrevious: false,
          hasMore: false,
          nextOffset: null,
          truncated: false,
          items: transactions.slice(0, 500),
        }}
      />,
    );

    rerender(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "stable-live-page",
          total: 501,
          offset: 500,
          limit: 1,
          hasPrevious: true,
          hasMore: false,
          nextOffset: null,
          truncated: false,
          items: [transactions[500]],
        }}
      />,
    );

    await userEvent.click(screen.getByRole("tab", { name: "序列" }));
    expect(
      await screen.findByRole("button", {
        name: "GET http://stable-page.example/501 文档",
      }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", {
        name: "GET http://stable-page.example/1 文档",
      }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "较新" }),
    ).not.toBeInTheDocument();
  });

  it("高流量尾部追加沿用分页代际且不会重启正在进行的历史加载", async () => {
    const basePage = createServiceSnapshot().transactions;
    const olderTransaction = createTransactionSummary({
      transactionId: "older-stable",
      sequence: 1,
      host: "history.example",
      path: "/stable",
      urlDisplay: "http://history.example/stable",
    });
    const latestTransaction = createTransactionSummary({
      transactionId: "latest-stable",
      sequence: 2,
      host: "latest.example",
      path: "/stable",
      urlDisplay: "http://latest.example/stable",
    });
    const appendedTransaction = createTransactionSummary({
      transactionId: "appended-stable",
      sequence: 3,
      host: "append.example",
      path: "/stable",
      urlDisplay: "http://append.example/stable",
    });
    let resolveHistoryPage:
      | ((page: typeof basePage) => void)
      | undefined;
    storeMocks.listTransactions.mockReturnValue(
      new Promise((resolve) => {
        resolveHistoryPage = resolve;
      }),
    );
    const { rerender } = render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "append-stable-generation",
          total: 2,
          offset: 1,
          limit: 1,
          hasPrevious: true,
          hasMore: false,
          nextOffset: null,
          truncated: true,
          items: [latestTransaction],
        }}
      />,
    );
    await waitFor(() => expect(storeMocks.listTransactions).toHaveBeenCalledTimes(1));

    rerender(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "append-stable-generation",
          total: 3,
          offset: 2,
          limit: 1,
          hasPrevious: true,
          hasMore: false,
          nextOffset: null,
          truncated: true,
          items: [appendedTransaction],
        }}
      />,
    );
    expect(storeMocks.listTransactions).toHaveBeenCalledTimes(1);
    expect(screen.getByTitle(/http:\/\/append\.example/)).toBeVisible();

    resolveHistoryPage?.({
      ...basePage,
      collectionToken: "append-stable-generation",
      total: 3,
      offset: 0,
      limit: 1,
      hasPrevious: false,
      hasMore: true,
      nextOffset: 1,
      truncated: true,
      items: [olderTransaction],
    });
    await waitFor(() =>
      expect(screen.queryByText("正在加载更早的事务…")).not.toBeInTheDocument(),
    );
    expect(screen.queryByText("较早事务加载失败。")).not.toBeInTheDocument();
    expect(screen.getByTitle(/http:\/\/history\.example/)).toBeVisible();
    expect(screen.getByTitle(/http:\/\/append\.example/)).toBeVisible();
  });

  it("同一分页代际的实时快照跳跃时只补齐中间 offset 缺口", async () => {
    const basePage = createServiceSnapshot().transactions;
    const firstTransaction = createTransactionSummary({
      transactionId: "jump-first",
      sequence: 1,
      host: "jump.example",
      path: "/1",
      urlDisplay: "http://jump.example/1",
    });
    const middleTransactions = [2, 3, 4].map((sequence) =>
      createTransactionSummary({
        transactionId: `jump-middle-${sequence}`,
        sequence,
        host: "jump.example",
        path: `/${sequence}`,
        urlDisplay: `http://jump.example/${sequence}`,
      }),
    );
    const latestTransaction = createTransactionSummary({
      transactionId: "jump-latest",
      sequence: 5,
      host: "jump.example",
      path: "/5",
      urlDisplay: "http://jump.example/5",
    });
    storeMocks.listTransactions.mockResolvedValue({
      ...basePage,
      collectionToken: "jump-stable-generation",
      total: 5,
      offset: 1,
      limit: 3,
      hasPrevious: true,
      hasMore: true,
      nextOffset: 4,
      truncated: true,
      items: middleTransactions,
    });
    const { rerender } = render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "jump-stable-generation",
          total: 1,
          offset: 0,
          limit: 1,
          hasPrevious: false,
          hasMore: false,
          nextOffset: null,
          truncated: false,
          items: [firstTransaction],
        }}
      />,
    );
    expect(storeMocks.listTransactions).not.toHaveBeenCalled();

    rerender(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "jump-stable-generation",
          total: 5,
          offset: 4,
          limit: 1,
          hasPrevious: true,
          hasMore: false,
          nextOffset: null,
          truncated: true,
          items: [latestTransaction],
        }}
      />,
    );

    await waitFor(() =>
      expect(storeMocks.listTransactions).toHaveBeenCalledWith(
        {
          offset: 1,
          limit: 3,
          collectionToken: "jump-stable-generation",
        },
        expect.any(AbortSignal),
      ),
    );
    await waitFor(() =>
      expect(screen.queryByText("正在加载更早的事务…")).not.toBeInTheDocument(),
    );
    expect(storeMocks.listTransactions).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("较早事务加载失败。")).not.toBeInTheDocument();
    expect(screen.getByText("显示 5 / 5 条")).toBeVisible();
  });

  it("历史首请求瞬态失败后退避重试并按短页连续推进到完整集合", async () => {
    const basePage = createServiceSnapshot().transactions;
    const transactions = [1, 2, 3, 4, 5].map((sequence) =>
      createTransactionSummary({
        transactionId: `retry-${sequence}`,
        sequence,
        host: "retry.example",
        path: `/${sequence}`,
        urlDisplay: `http://retry.example/${sequence}`,
      }),
    );
    storeMocks.listTransactions.mockImplementation(
      ({ offset }: { offset?: number }) => {
        const requestIndex = storeMocks.listTransactions.mock.calls.length;
        if (requestIndex === 1) {
          return Promise.reject(new Error("temporaryControlUnavailable"));
        }
        if (requestIndex === 2 && offset === 1) {
          return Promise.resolve({
            ...basePage,
            collectionToken: "retry-stable-generation",
            total: 5,
            offset: 1,
            limit: 3,
            hasPrevious: true,
            hasMore: true,
            nextOffset: 2,
            truncated: true,
            items: [transactions[1]!],
          });
        }
        if (requestIndex === 3 && offset === 2) {
          return Promise.resolve({
            ...basePage,
            collectionToken: "retry-stable-generation",
            total: 5,
            offset: 2,
            limit: 2,
            hasPrevious: true,
            hasMore: true,
            nextOffset: 4,
            truncated: true,
            items: transactions.slice(2, 4),
          });
        }
        return Promise.reject(new Error("unexpectedDuplicateHistoryRequest"));
      },
    );
    const { rerender } = render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "retry-stable-generation",
          total: 1,
          offset: 0,
          limit: 1,
          hasPrevious: false,
          hasMore: false,
          nextOffset: null,
          truncated: false,
          items: [transactions[0]!],
        }}
      />,
    );
    rerender(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "retry-stable-generation",
          total: 5,
          offset: 4,
          limit: 1,
          hasPrevious: true,
          hasMore: false,
          nextOffset: null,
          truncated: true,
          items: [transactions[4]!],
        }}
      />,
    );

    await waitFor(() =>
      expect(screen.getByText("较早事务加载失败。")).toBeVisible(),
    );
    await waitFor(
      () => expect(storeMocks.listTransactions).toHaveBeenCalledTimes(3),
      { timeout: 1_000 },
    );
    await waitFor(() =>
      expect(screen.queryByText("较早事务加载失败。")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("显示 5 / 5 条")).toBeVisible();
    expect(storeMocks.listTransactions.mock.calls.map(([query]) => query.offset)).toEqual([
      1, 1, 2,
    ]);
    await new Promise((resolve) => window.setTimeout(resolve, 250));
    expect(storeMocks.listTransactions).toHaveBeenCalledTimes(3);
  });

  it("FIFO 切换分页代际后忽略旧请求晚到的冲突错误", async () => {
    const basePage = createServiceSnapshot().transactions;
    const retainedTransaction = createTransactionSummary({
      transactionId: "retained-after-fifo",
      sequence: 2,
      host: "retained.example",
      path: "/fifo",
      urlDisplay: "http://retained.example/fifo",
    });
    const latestTransaction = createTransactionSummary({
      transactionId: "latest-after-fifo",
      sequence: 3,
      host: "latest.example",
      path: "/fifo",
      urlDisplay: "http://latest.example/fifo",
    });
    let rejectOldPage: ((reason: Error) => void) | undefined;
    storeMocks.listTransactions.mockImplementation(
      ({ collectionToken }: { collectionToken?: string }) => {
        if (collectionToken === "before-fifo") {
          return new Promise((_, reject) => {
            rejectOldPage = reject;
          });
        }
        return Promise.resolve({
          ...basePage,
          collectionToken: "after-fifo",
          total: 2,
          offset: 0,
          limit: 1,
          hasPrevious: false,
          hasMore: true,
          nextOffset: 1,
          truncated: true,
          items: [retainedTransaction],
        });
      },
    );
    const { rerender } = render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "before-fifo",
          total: 2,
          offset: 1,
          hasPrevious: true,
          truncated: true,
          items: [latestTransaction],
        }}
      />,
    );
    await waitFor(() => expect(storeMocks.listTransactions).toHaveBeenCalledTimes(1));

    rerender(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "after-fifo",
          total: 2,
          offset: 1,
          hasPrevious: true,
          truncated: true,
          items: [latestTransaction],
        }}
      />,
    );
    rejectOldPage?.(new Error("transactionsCollectionChanged"));

    await waitFor(() => expect(storeMocks.listTransactions).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(screen.queryByText("正在加载更早的事务…")).not.toBeInTheDocument(),
    );
    expect(screen.queryByText("较早事务加载失败。")).not.toBeInTheDocument();
    expect(screen.getByTitle(/http:\/\/retained\.example/)).toBeVisible();
  });

  it("搜索框隐藏占位文字并通过可访问名称保留输入语义", () => {
    const transactionPage = createServiceSnapshot().transactions;
    render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={transactionPage}
      />,
    );

    const searchInput = screen.getByRole("searchbox", { name: "搜索事务" });
    expect(searchInput).not.toHaveAttribute("placeholder");
    expect(searchInput.closest(".filterSearch")).toBeVisible();
    expect(searchInput.closest(".filterSearch")).toHaveAttribute(
      "title",
      "搜索方法、主机、路径或状态",
    );
  });

  it("严格模式下同代 WebSocket 事务到达后立即显示而无需刷新", async () => {
    const basePage = createServiceSnapshot().transactions;
    const liveTransaction = createTransactionSummary({
      transactionId: "strict-live-transaction",
      host: "live.example",
      path: "/websocket-now",
      urlDisplay: "https://live.example/websocket-now",
    });
    const renderNavigator = (items: TransactionSummary[], revision: number) => (
      <StrictMode>
        <TransactionNavigator
          onSelectTransaction={vi.fn()}
          selectedHost={null}
          selectedTransactionId={null}
          transactionPage={{
            ...basePage,
            revision,
            collectionToken: "strict-live-generation",
            total: items.length,
            items,
          }}
        />
      </StrictMode>
    );
    const { rerender } = render(renderNavigator([], basePage.revision));

    rerender(renderNavigator([liveTransaction], basePage.revision + 1));

    expect(await screen.findByTitle(/live\.example/)).toBeVisible();
    expect(screen.getByText("显示 1 / 1 条")).toBeVisible();
    expect(storeMocks.listTransactions).not.toHaveBeenCalled();
  });

  it("清空事务换代后清除旧筛选并立即显示新录制", async () => {
    const user = userEvent.setup();
    const oldTransaction = createTransactionSummary({
      transactionId: "before-clear",
      host: "before-clear.example",
      urlDisplay: "https://before-clear.example/old",
    });
    const newTransaction = createTransactionSummary({
      transactionId: "after-clear",
      host: "after-clear.example",
      urlDisplay: "https://after-clear.example/new",
    });
    const basePage = createServiceSnapshot().transactions;
    const { rerender } = render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={oldTransaction.host}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "recording:before-clear",
          total: 1,
          items: [oldTransaction],
        }}
      />,
    );

    await user.click(screen.getByRole("checkbox", { name: "聚焦" }));
    await user.type(screen.getByRole("searchbox", { name: "搜索事务" }), "old");

    rerender(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={oldTransaction.host}
        selectedTransactionId={null}
        transactionPage={{
          ...basePage,
          collectionToken: "recording:after-clear",
          total: 1,
          items: [newTransaction],
        }}
      />,
    );

    expect(await screen.findByTitle(/after-clear\.example/)).toBeVisible();
    expect(screen.getByRole("searchbox", { name: "搜索事务" })).toHaveValue("");
    expect(screen.getByRole("checkbox", { name: "聚焦" })).not.toBeChecked();
  });

  it("在结构中选择事务并自动加载请求正文", async () => {
    const user = userEvent.setup();
    render(<ConnectionsWorkspace />);

    await expandTreePath(user, "http://api.example:8080", "v1", "users");
    const transactionRow = screen.getByRole("button", {
      name: /POST http:\/\/api\.example:8080\/v1\/users\/two JSON/,
    });
    await user.click(transactionRow);

    const inspector = screen.getByRole("region", {
      name: "事务检查器",
    });
    expect(
      within(inspector).getAllByText("http://api.example:8080/v1/users/two")[0],
    ).toBeVisible();

    await user.click(within(inspector).getByRole("tab", { name: "内容" }));
    await waitFor(() =>
      expect(within(inspector).getByText("content-type")).toBeVisible(),
    );
    const requestPanel = within(inspector).getByRole("region", {
      name: "请求",
    });
    await user.click(within(requestPanel).getByRole("tab", { name: "文本" }));
    expect(screen.queryByText("正在加载正文")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(within(inspector).getByText('{"ok":true}')).toBeVisible(),
    );
    expect(inspector.querySelector(".bodyMetadata")).toBeNull();
    expect(
      within(inspector).queryByRole("button", { name: "复制正文" }),
    ).not.toBeInTheDocument();
    expect(storeMocks.getTransactionBody).toHaveBeenCalledWith(
      "transaction-two",
      "request",
      expect.any(AbortSignal),
    );
    storeMocks.getTransactionBody.mockClear();
    // 正文已经进入内存后，文本、JSON 与 Hex 只切换本地表示，不得再次访问控制接口。
    await user.click(within(requestPanel).getByRole("tab", { name: "JSON" }));
    await user.click(
      within(requestPanel).getByRole("tab", { name: "十六进制" }),
    );
    await user.click(within(requestPanel).getByRole("tab", { name: "文本" }));
    expect(storeMocks.getTransactionBody).not.toHaveBeenCalled();

    // 内容页同时维护请求和响应面板；切换原始流后，请求面板必须按新事务重置为十六进制并重新读取正文。
    await expandTreePath(user, "http://api.example", "v1", "users");
    await user.click(
      screen.getByRole("button", {
        name: /GET http:\/\/api\.example\/v1\/users\/one/,
      }),
    );
    await waitFor(() =>
      expect(
        within(
          within(inspector).getByRole("region", { name: "请求" }),
        ).getByRole("tab", { name: "十六进制" }),
      ).toHaveAttribute("aria-selected", "true"),
    );
    const rawRequestPanel = within(inspector).getByRole("region", {
      name: "请求",
    });
    await waitFor(() =>
      expect(
        within(rawRequestPanel).getByRole("region", { name: "十六进制" }),
      ).toHaveTextContent("7b 22 6f 6b 22 3a 74 72 75 65 7d"),
    );
    expect(
      within(rawRequestPanel).getByRole("region", { name: "ASCII" }),
    ).toHaveTextContent('{"ok":true}');
    const hexDivider = within(rawRequestPanel).getByRole("separator", {
      name: "十六进制 / ASCII",
    });
    expect(hexDivider).toHaveAttribute("aria-valuenow", "50");
    hexDivider.focus();
    await user.keyboard("{Home}");
    expect(hexDivider).toHaveAttribute("aria-valuenow", "0");
    await user.keyboard("{End}");
    expect(hexDivider).toHaveAttribute("aria-valuenow", "100");
    expect(storeMocks.getTransactionBody).toHaveBeenCalledWith(
      "transaction-one",
      "request",
      expect.any(AbortSignal),
    );
  });

  /** 自动派生正文用于可读视图，十六进制仍保持抓获原文，避免协议解码覆盖取证证据。 */
  it("自动显示应用层解密正文并在十六进制保留原始字节", async () => {
    storeMocks.getTransactionBody.mockImplementation(
      async (
        transactionId: string,
        side: "request" | "response",
      ): Promise<EncodedBodyResponse> => ({
        revision: 1,
        meta: {
          transactionId,
          side,
          contentType: "application/x-www-form-urlencoded",
          encoding: "identity",
          storedBytes: 3,
          originalBytes: 3,
          truncated: false,
        },
        base64: window.btoa("raw"),
        decoded: {
          algorithm: "aes128EcbPkcs7Json",
          contentType: "application/json;charset=UTF-8",
          decodedBytes: 18,
          base64: window.btoa('{"keyword":"test"}'),
        },
      }),
    );
    const user = userEvent.setup();
    render(<ConnectionsWorkspace />);

    await expandTreePath(user, "http://api.example:8080", "v1", "users");
    await user.click(
      screen.getByRole("button", {
        name: /POST http:\/\/api\.example:8080\/v1\/users\/two JSON/,
      }),
    );
    const inspector = screen.getByRole("region", { name: "事务检查器" });
    await user.click(within(inspector).getByRole("tab", { name: "内容" }));
    const requestPanel = within(inspector).getByRole("region", { name: "请求" });
    await user.click(within(requestPanel).getByRole("tab", { name: "文本" }));

    expect(await within(requestPanel).findByText('{"keyword":"test"}')).toBeVisible();
    expect(
      within(requestPanel).queryByText(/已自动解密应用层正文/),
    ).not.toBeInTheDocument();

    await user.click(within(requestPanel).getByRole("tab", { name: "十六进制" }));
    expect(
      await within(requestPanel).findByRole("region", { name: "十六进制" }),
    ).toHaveTextContent("72 61 77");
    expect(
      within(requestPanel).queryByText(/已自动解密应用层正文/),
    ).not.toBeInTheDocument();
  });

  /** 高频实时页只保留尾部窗口；选中项滑出窗口时不得重建检查器或重新加载另一条事务。 */
  it("选中事务滑出实时尾页后保持详情稳定且不闪烁加载状态", async () => {
    const { rerender } = render(<ConnectionsWorkspace />);

    await waitFor(() =>
      expect(storeMocks.getTransactionDetail).toHaveBeenCalledWith(
        "transaction-one",
        expect.any(String),
        expect.any(AbortSignal),
      ),
    );
    storeMocks.getTransactionDetail.mockClear();
    const currentSnapshot = storeMocks.snapshot!;
    storeMocks.snapshot = {
      ...currentSnapshot,
      transactions: {
        ...currentSnapshot.transactions,
        offset: 1,
        items: currentSnapshot.transactions.items.slice(1),
      },
    };

    rerender(<ConnectionsWorkspace />);

    expect(screen.queryByText("正在加载事务详情…")).not.toBeInTheDocument();
    expect(storeMocks.getTransactionDetail).not.toHaveBeenCalled();
    expect(
      within(screen.getByRole("region", { name: "事务检查器" })).getAllByText(
        "http://api.example/v1/users/one",
      )[0],
    ).toBeVisible();
  });

  /** 高频服务快照只更新统计，不得让同一图片正文生成新的 Blob URL 并触发浏览器重新解码。 */
  it("实时快照刷新时复用同一图片预览资源", async () => {
    let objectUrlSequence = 0;
    vi.stubGlobal("URL", {
      createObjectURL: vi.fn(
        () => `blob:stable-image-${++objectUrlSequence}`,
      ),
      revokeObjectURL: vi.fn(),
    });
    const imageTransaction = createTransactionSummary({
      transactionId: "stable-image",
      sequence: 1,
      host: "image.example",
      path: "/cover.png",
      urlDisplay: "https://image.example/cover.png",
      contentType: "image/png",
    });
    const baseSnapshot = createServiceSnapshot();
    storeMocks.snapshot = createServiceSnapshot({
      recording: {
        ...baseSnapshot.recording,
        transactionCount: 1,
      },
      transactions: {
        ...baseSnapshot.transactions,
        total: 1,
        items: [imageTransaction],
      },
    });
    storeMocks.getTransactionDetail.mockResolvedValue({
      revision: 1,
      transaction: imageTransaction,
      requestHeaders: [],
      responseHeaders: [{ name: "content-type", value: "image/png" }],
      requestBody: null,
      requestPackets: [],
      responsePackets: [],
      responseBody: {
        transactionId: "stable-image",
        side: "response",
        contentType: "image/png",
        encoding: "identity",
        storedBytes: 8,
        originalBytes: 8,
        truncated: false,
      },
    });
    storeMocks.getTransactionBody.mockResolvedValue({
      revision: 1,
      meta: {
        transactionId: "stable-image",
        side: "response",
        contentType: "image/png",
        encoding: "identity",
        storedBytes: 8,
        originalBytes: 8,
        truncated: false,
      },
      base64: window.btoa("\u0089PNG\r\n\u001a\n"),
    });
    const user = userEvent.setup();
    const { rerender, unmount } = render(<ConnectionsWorkspace />);

    await user.click(screen.getByRole("tab", { name: "内容" }));
    const responsePanel = await screen.findByRole("region", { name: "响应" });
    await user.click(
      within(responsePanel).getByRole("tab", { name: "预览" }),
    );
    await waitFor(() => expect(URL.createObjectURL).toHaveBeenCalledTimes(1));
    expect(within(responsePanel).getByRole("img")).toHaveAttribute(
      "src",
      "blob:stable-image-1",
    );

    storeMocks.snapshot = {
      ...storeMocks.snapshot!,
      revision: storeMocks.snapshot!.revision + 1,
    };
    rerender(<ConnectionsWorkspace />);

    await waitFor(() =>
      expect(within(responsePanel).getByRole("img")).toHaveAttribute(
        "src",
        "blob:stable-image-1",
      ),
    );
    expect(URL.createObjectURL).toHaveBeenCalledTimes(1);
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:stable-image-1");
  });

  /** 回归旧 512 KiB UI 预览边界：只有当前正文被懒加载，但尾字节必须真实进入 DOM。 */
  it("用户打开正文后显示超过旧预览边界的完整内容", async () => {
    const user = userEvent.setup();
    const completeBody = `${"a".repeat(512 * 1024)}COMPLETE_TAIL`;
    const transaction = storeMocks.snapshot!.transactions.items.find(
      (candidate) => candidate.transactionId === "transaction-two",
    )!;
    storeMocks.getTransactionDetail.mockResolvedValue({
      revision: 1,
      transaction,
      requestHeaders: [{ name: "content-type", value: "text/plain" }],
      responseHeaders: [],
      requestBody: {
        transactionId: transaction.transactionId,
        side: "request",
        contentType: "text/plain",
        encoding: "identity",
        storedBytes: completeBody.length,
        originalBytes: completeBody.length,
        truncated: false,
      },
      responseBody: null,
      requestPackets: [],
      responsePackets: [],
    });
    storeMocks.getTransactionBody.mockResolvedValue({
      revision: 1,
      meta: {
        transactionId: transaction.transactionId,
        side: "request",
        contentType: "text/plain",
        encoding: "identity",
        storedBytes: completeBody.length,
        originalBytes: completeBody.length,
        truncated: false,
      },
      base64: window.btoa(completeBody),
    });

    render(<ConnectionsWorkspace />);
    await expandTreePath(user, "http://api.example:8080", "v1", "users");
    await user.click(
      screen.getByRole("button", {
        name: /POST http:\/\/api\.example:8080\/v1\/users\/two JSON/,
      }),
    );
    const inspector = screen.getByRole("region", { name: "事务检查器" });
    await user.click(within(inspector).getByRole("tab", { name: "内容" }));
    const requestPanel = within(inspector).getByRole("region", {
      name: "请求",
    });
    await user.click(within(requestPanel).getByRole("tab", { name: "文本" }));
    await waitFor(() =>
      expect(requestPanel.querySelector(".bodyPreview")).toHaveTextContent(
        /COMPLETE_TAIL$/,
      ),
    );
  });

  it("内容检查器的请求与响应面板可通过鼠标和键盘调整高度", async () => {
    let resizeCallback: ResizeObserverCallback | null = null;
    vi.stubGlobal("PointerEvent", MouseEvent);
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: ResizeObserverCallback) {
          resizeCallback = callback;
        }

        observe() {}

        unobserve() {}

        disconnect() {}
      },
    );
    const user = userEvent.setup();
    render(<ConnectionsWorkspace />);

    await expandTreePath(user, "http://api.example:8080", "v1", "users");
    await user.click(
      screen.getByRole("button", {
        name: /POST http:\/\/api\.example:8080\/v1\/users\/two JSON/,
      }),
    );
    const inspector = screen.getByRole("region", { name: "事务检查器" });
    await user.click(within(inspector).getByRole("tab", { name: "内容" }));
    const divider = await within(inspector).findByRole("separator", {
      name: "请求",
    });
    const contents = inspector.querySelector(".transactionContentsView");
    expect(contents).not.toBeNull();
    const contentsBounds = {
      x: 0,
      y: 100,
      width: 800,
      height: 600,
      top: 100,
      right: 800,
      bottom: 700,
      left: 0,
      toJSON: () => ({}),
    } as DOMRect;
    Object.defineProperty(contents, "clientHeight", {
      configurable: true,
      value: 600,
    });
    vi.spyOn(
      contents as HTMLDivElement,
      "getBoundingClientRect",
    ).mockReturnValue(contentsBounds);
    act(() => resizeCallback?.([], {} as ResizeObserver));

    expect(divider).toHaveAttribute("aria-orientation", "horizontal");
    expect(divider).toHaveAttribute("aria-valuemin", "0");
    expect(divider).toHaveAttribute("aria-valuemax", "100");
    expect(divider).toHaveAttribute("aria-valuenow", "50");

    divider.focus();
    await user.keyboard("{ArrowUp}");
    expect(divider).toHaveAttribute("aria-valuenow", "45");
    await user.keyboard("{Home}");
    expect(divider).toHaveAttribute("aria-valuenow", "0");
    await user.keyboard("{End}");
    expect(divider).toHaveAttribute("aria-valuenow", "100");

    fireEvent.pointerDown(divider, {
      button: 0,
      clientY: 0,
      pointerId: 1,
    });
    expect(divider).toHaveAttribute("aria-valuenow", "0");
    fireEvent.pointerMove(divider, { clientY: 900, pointerId: 1 });
    expect(divider).toHaveAttribute("aria-valuenow", "100");
    fireEvent.pointerUp(divider, { clientY: 900, pointerId: 1 });
  });

  it("结构视图以来源和路径为层级，并在资源叶节点省略传输方法", async () => {
    const user = userEvent.setup();
    render(<ConnectionsWorkspace />);

    expect(screen.getByText("http://api.example")).toBeVisible();
    expect(screen.getByText("http://api.example:8080")).toBeVisible();
    await expandTreePath(user, "http://api.example:8080", "v1", "users");
    const resourceRow = screen.getByRole("button", {
      name: /POST http:\/\/api\.example:8080\/v1\/users\/two JSON/,
    });
    expect(resourceRow).toHaveTextContent("two");
    expect(resourceRow).not.toHaveTextContent("POST");
    expect(screen.queryByText("根路径")).not.toBeInTheDocument();
    const sequenceTab = screen.getByRole("tab", { name: "序列" });
    expect(sequenceTab).toBeVisible();
    await user.click(sequenceTab);
    expect(sequenceTab).toHaveAttribute("aria-selected", "true");
    expect(
      screen.getByRole("button", {
        name: /POST http:\/\/api\.example:8080\/v1\/users\/two JSON/,
      }),
    ).toBeVisible();
  });

  it("摘要与图表页签只消费事务摘要并按已采集时间点绘制瀑布", async () => {
    const user = userEvent.setup();
    render(<ConnectionsWorkspace />);

    await expandTreePath(user, "http://api.example", "v1", "users");
    await user.click(
      screen.getByRole("button", {
        name: /GET http:\/\/api\.example\/v1\/users\/one/,
      }),
    );
    const inspector = screen.getByRole("region", {
      name: "事务检查器",
    });

    await user.click(within(inspector).getByRole("tab", { name: "摘要" }));
    expect(within(inspector).getByText("请求头")).toBeVisible();

    await user.click(within(inspector).getByRole("tab", { name: "图表" }));
    const waterfall = within(inspector).getByRole("list");
    expect(within(waterfall).getByText("TCP")).toBeVisible();
    expect(within(waterfall).getByText("HTTP")).toBeVisible();
    expect(within(waterfall).getByText("BODY")).toBeVisible();
    const httpRow = within(waterfall)
      .getByText("HTTP")
      .closest("[role='listitem']");
    expect(httpRow?.querySelector(".timingWaterfallBar")).toHaveStyle({
      left: "25%",
      width: "25%",
    });
  });

  it("检查器与消息页签支持方向键激活并关联当前面板", async () => {
    const user = userEvent.setup();
    render(<ConnectionsWorkspace />);

    const inspector = screen.getByRole("region", {
      name: "事务检查器",
    });
    const overviewTab = within(inspector).getByRole("tab", {
      name: "概览",
    });
    overviewTab.focus();
    await user.keyboard("{ArrowRight}");
    const contentsTab = within(inspector).getByRole("tab", {
      name: "内容",
    });
    expect(contentsTab).toHaveAttribute("aria-selected", "true");
    await waitFor(() =>
      expect(
        within(inspector).getAllByRole("tab", { name: "头" })[0],
      ).toBeVisible(),
    );

    const requestPanel = within(inspector).getByRole("region", {
      name: "请求",
    });
    const headersTab = within(requestPanel).getByRole("tab", { name: "头" });
    headersTab.focus();
    await user.keyboard("{ArrowRight}");
    expect(
      within(requestPanel).getByRole("tab", { name: "文本" }),
    ).toHaveAttribute("aria-selected", "true");
  });

  it("HTTP 与 HTTPS 结构树按来源、目录和资源组织，并以加减号控制展开", async () => {
    const user = userEvent.setup();
    const selectTransaction = vi.fn();
    const transactions = [
      createTransactionSummary({
        transactionId: "http-image",
        sequence: 1,
        host: "assets.example",
        path: "/images/loading.gif",
        urlDisplay: "http://assets.example/images/loading.gif",
      }),
      createTransactionSummary({
        transactionId: "http-style",
        sequence: 2,
        host: "assets.example",
        path: "/styles/site.css",
        urlDisplay: "http://assets.example/styles/site.css",
      }),
      createTransactionSummary({
        transactionId: "https-root",
        sequence: 3,
        protocol: "https",
        host: "secure.example",
        port: 443,
        path: "/favicon.ico",
        urlDisplay: "https://secure.example/favicon.ico",
      }),
    ];

    render(
      <TransactionNavigator
        onSelectTransaction={selectTransaction}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...createServiceSnapshot().transactions,
          items: transactions,
          total: transactions.length,
        }}
      />,
    );

    expect(screen.getByText("http://assets.example")).toBeVisible();
    expect(screen.getByText("https://secure.example")).toBeVisible();
    expect(screen.queryByText("images")).not.toBeInTheDocument();
    expect(screen.queryByText("loading.gif")).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "展开 http://assets.example" }),
    );
    expect(screen.getByText("images")).toBeVisible();
    expect(screen.queryByText("loading.gif")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "展开 images" }));
    const imageResource = screen.getByText("loading.gif");
    expect(imageResource).toBeVisible();
    expect(
      imageResource
        .closest("button")
        ?.querySelector(".transactionResourceIcon--image"),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "收缩 images" }));
    expect(
      screen.queryByRole("button", {
        name: new RegExp("GET http://assets\\.example/images/loading\\.gif"),
      }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "展开 images" }));
    await user.click(
      screen.getByRole("button", {
        name: new RegExp("GET http://assets\\.example/images/loading\\.gif"),
      }),
    );
    expect(selectTransaction).toHaveBeenCalledWith(
      "http-image",
      expect.objectContaining({ transactionId: "http-image" }),
    );
  });

  /**
   * 同一来源可以同时包含成功与失败事务；根 URL 只负责分组，状态必须落到实际事务叶节点。
   */
  it("结构树把失败状态标记在事务叶节点而不是来源 URL", async () => {
    const user = userEvent.setup();
    const origin = "https://capture.example:443";
    const transactions = [
      createTransactionSummary({
        transactionId: "tunnel-complete",
        sequence: 1,
        protocol: "tunnel",
        host: "capture.example",
        port: 443,
        path: "/",
        urlDisplay: "tls://capture.example:443",
        clientAddress: "127.0.0.1:50100",
        status: "complete",
      }),
      createTransactionSummary({
        transactionId: "tunnel-failed",
        sequence: 2,
        protocol: "tunnel",
        host: "capture.example",
        port: 443,
        path: "/",
        urlDisplay: origin,
        clientAddress: "127.0.0.1:50101",
        status: "failed",
      }),
    ];

    const { container } = render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...createServiceSnapshot().transactions,
          items: transactions,
          total: transactions.length,
        }}
      />,
    );

    const originHeader = screen.getByText(origin).closest(".streamTreeHeader");
    expect(originHeader).not.toBeNull();
    expect(originHeader?.querySelector(".transactionStatusDot")).toBeNull();

    await user.click(screen.getByRole("button", { name: `展开 ${origin}` }));
    const transactionLeaves = container.querySelectorAll(
      ".streamTransactionStatusItem",
    );
    expect(transactionLeaves).toHaveLength(2);
    expect(transactionLeaves[0]).toHaveTextContent("127.0.0.1:50100");
    expect(transactionLeaves[1]).toHaveTextContent("127.0.0.1:50101");
    expect(
      transactionLeaves[0]?.querySelector(".transactionStatusDot--success"),
    ).toBeInTheDocument();
    expect(
      transactionLeaves[1]?.querySelector(".transactionStatusDot--danger"),
    ).toBeInTheDocument();
  });

  it("事务右键菜单执行复制、聚焦与工具跳转，不展示空实现", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    const openToolSettings = vi.fn();
    vi.stubGlobal("navigator", {
      ...navigator,
      clipboard: { writeText },
    });
    const transaction = createTransactionSummary({
      transactionId: "context-transaction",
      host: "api.example",
      path: "/v1/result.json",
      urlDisplay: "http://api.example/v1/result.json",
      contentType: "application/json",
    });

    render(
      <TransactionNavigator
        onOpenToolSettings={openToolSettings}
        onSelectTransaction={vi.fn()}
        selectedHost={transaction.host}
        selectedTransactionId={null}
        transactionPage={{
          ...createServiceSnapshot().transactions,
          items: [transaction],
          total: 1,
        }}
      />,
    );

    fireEvent.contextMenu(screen.getByText("http://api.example"));
    await user.click(await screen.findByRole("menuitem", { name: "复制 URL" }));
    expect(writeText).toHaveBeenCalledWith(transaction.urlDisplay);

    fireEvent.contextMenu(screen.getByText("http://api.example"));
    await user.click(
      await screen.findByRole("menuitem", { name: "导出所选 HAR" }),
    );
    await waitFor(() =>
      expect(storeMocks.exportRecording).toHaveBeenCalledWith({
        format: "har",
        includeBodies: true,
        transactionIds: [transaction.transactionId],
      }),
    );

    fireEvent.contextMenu(screen.getByText("http://api.example"));
    await user.click(
      await screen.findByRole("menuitem", { name: "聚焦此主机" }),
    );
    expect(screen.getByRole("checkbox", { name: "聚焦" })).toBeChecked();

    fireEvent.contextMenu(screen.getByText("http://api.example"));
    await user.click(await screen.findByRole("menuitem", { name: "主机工具" }));
    expect(
      await screen.findByRole("menuitem", { name: "本地映射" }),
    ).toBeVisible();
    expect(openToolSettings).not.toHaveBeenCalled();
    expect(screen.queryByText("占位")).not.toBeInTheDocument();
  });

  it("从具体资源右键打开工具时保留完整路径和查询参数", async () => {
    const user = userEvent.setup();
    const openToolSettings = vi.fn();
    const transaction = createTransactionSummary({
      transactionId: "path-context-transaction",
      host: "api.example",
      port: 8443,
      path: "/v1/reports/result.json",
      query: "view=full",
      urlDisplay: "https://api.example:8443/v1/reports/result.json?view=full",
      contentType: "application/json",
    });

    render(
      <TransactionNavigator
        onOpenToolSettings={openToolSettings}
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...createServiceSnapshot().transactions,
          items: [transaction],
          total: 1,
        }}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: "展开 https://api.example:8443" }),
    );
    await user.click(screen.getByRole("button", { name: "展开 v1" }));
    await user.click(screen.getByRole("button", { name: "展开 reports" }));
    const resourceRow = screen.getByRole("button", {
      name: new RegExp(
        "GET https://api\\.example:8443/v1/reports/result\\.json\\?view=full",
      ),
    });

    fireEvent.contextMenu(resourceRow);
    await user.click(await screen.findByRole("menuitem", { name: "主机工具" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "本地映射" }));

    await waitFor(() =>
      expect(openToolSettings).toHaveBeenCalledWith("mapLocal", {
        transactionId: "path-context-transaction",
        contentType: "application/json",
        location: {
          protocol: "https",
          host: "api.example",
          port: "8443",
          path: "/v1/reports/result.json",
          query: "view=full",
        },
      }),
    );
  });

  it("HTTPS 主机右键可以预填并定位客户端证书导入", async () => {
    const user = userEvent.setup();
    const openSslSettings = vi.fn();
    const transaction = createTransactionSummary({
      transactionId: "client-certificate-context",
      host: "secure.example",
      port: 443,
      path: "/account",
      urlDisplay: "https://secure.example:443/account",
      contentType: "application/json",
    });
    render(
      <TransactionNavigator
        onOpenSslSettings={openSslSettings}
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...createServiceSnapshot().transactions,
          items: [transaction],
          total: 1,
        }}
      />,
    );

    fireEvent.contextMenu(screen.getByText("https://secure.example"));
    await user.click(await screen.findByRole("menuitem", { name: "主机工具" }));
    fireEvent.click(
      await screen.findByRole("menuitem", { name: "添加客户端证书" }),
    );

    await waitFor(() =>
      expect(openSslSettings).toHaveBeenCalledWith(
        expect.objectContaining({
          transactionId: "client-certificate-context",
          location: expect.objectContaining({
            host: "secure.example",
            port: "443",
          }),
        }),
        true,
      ),
    );
  });

  it("按图片、JSON、APK、HTML 与二进制类型显示对应文件图标", async () => {
    const user = userEvent.setup();
    const resources = [
      ["photo.png", "image/png", "image"],
      ["manifest.json", "application/json", "json"],
      ["client.apk", "application/vnd.android.package-archive", "apk"],
      ["index.html", "text/html", "html"],
      ["payload.bin", "application/octet-stream", "binary"],
    ] as const;
    const transactions = resources.map(([fileName, contentType], index) =>
      createTransactionSummary({
        transactionId: `resource-${index}`,
        sequence: index + 1,
        host: "files.example",
        path: `/${fileName}`,
        urlDisplay: `http://files.example/${fileName}`,
        contentType,
      }),
    );

    render(
      <TransactionNavigator
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedTransactionId={null}
        transactionPage={{
          ...createServiceSnapshot().transactions,
          items: transactions,
          total: transactions.length,
        }}
      />,
    );
    await user.click(
      screen.getByRole("button", { name: "展开 http://files.example" }),
    );

    for (const [fileName, , kind] of resources) {
      expect(
        screen
          .getByText(fileName)
          .closest("button")
          ?.querySelector(`.transactionResourceIcon--${kind}`),
      ).toBeInTheDocument();
    }
  });
  it("事务字节变化时高亮报文并在固定窗口后消散", () => {
    vi.useFakeTimers();
    const transaction = createTransactionSummary({
      transactionId: "traffic-transaction",
      host: "api.example",
      urlDisplay: "http://api.example/live",
      path: "/live",
    });
    const page = {
      ...createServiceSnapshot().transactions,
      total: 1,
      items: [transaction],
    };
    const { rerender } = render(
      <TransactionNavigator
        transactionPage={page}
        selectedTransactionId={null}
        selectedHost={null}
        onSelectTransaction={vi.fn()}
      />,
    );

    const transactionGroup = screen
      .getByText("http://api.example")
      .closest(".transactionHostGroup");
    expect(transactionGroup).not.toHaveAttribute("data-traffic-active");

    rerender(
      <TransactionNavigator
        transactionPage={{
          ...page,
          items: [
            {
              ...transaction,
              sizes: {
                ...transaction.sizes,
                responseBodyBytes: 128,
              },
            },
          ],
        }}
        selectedTransactionId={null}
        selectedHost={null}
        onSelectTransaction={vi.fn()}
      />,
    );
    expect(transactionGroup).toHaveAttribute("data-traffic-active", "true");

    act(() => {
      vi.advanceTimersByTime(1_001);
    });
    expect(transactionGroup).not.toHaveAttribute("data-traffic-active");
  });

  it("新事务进入滚动集合时高亮并在固定窗口后消散", () => {
    vi.useFakeTimers();
    const basePage = createServiceSnapshot().transactions;
    const firstTransaction = createTransactionSummary({
      transactionId: "existing-traffic",
      sequence: 1,
      host: "existing.example",
      urlDisplay: "http://existing.example/live",
      path: "/live",
    });
    const newTransaction = createTransactionSummary({
      transactionId: "new-traffic",
      sequence: 2,
      host: "new.example",
      urlDisplay: "http://new.example/live",
      path: "/live",
    });
    const { rerender } = render(
      <TransactionNavigator
        transactionPage={{ ...basePage, total: 1, items: [firstTransaction] }}
        selectedTransactionId={null}
        selectedHost={null}
        onSelectTransaction={vi.fn()}
      />,
    );

    rerender(
      <TransactionNavigator
        transactionPage={{
          ...basePage,
          total: 2,
          items: [firstTransaction, newTransaction],
        }}
        selectedTransactionId={null}
        selectedHost={null}
        onSelectTransaction={vi.fn()}
      />,
    );
    const newTransactionGroup = screen
      .getByText("http://new.example")
      .closest(".transactionHostGroup");
    expect(newTransactionGroup).toHaveAttribute("data-traffic-active", "true");

    act(() => {
      vi.advanceTimersByTime(1_001);
    });
    expect(newTransactionGroup).not.toHaveAttribute("data-traffic-active");
  });

  /**
   * 无截断提示时也必须保持“视图标签、筛选栏、事务主体”三级结构；
   * 筛选栏置于主体上方（Charles 习惯），主体仍占据唯一弹性行。
   */

});
