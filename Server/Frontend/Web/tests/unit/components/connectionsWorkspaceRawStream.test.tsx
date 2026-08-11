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

describe("事务工作台原始流", () => {
  it("无截断提示时事务主体仍占据独立弹性区域", () => {
    const page = createServiceSnapshot().transactions;
    const { container } = render(
      <TransactionNavigator
        transactionPage={page}
        selectedTransactionId={null}
        selectedHost={null}
        onSelectTransaction={vi.fn()}
      />,
    );

    const navigatorPane = container.querySelector(".transactionNavigatorPane");
    expect(navigatorPane?.children).toHaveLength(3);
    expect(navigatorPane?.children[1]).toHaveClass("transactionFilterBar");
    expect(navigatorPane?.children[2]).toHaveClass("transactionNavigatorBody");
    expect(
      navigatorPane?.querySelector(".transactionNavigatorBody"),
    ).toContainElement(screen.getByLabelText("事务结构", { selector: "div" }));
  });

  it("概览属性组使用加减号独立展开与收缩", async () => {
    const user = userEvent.setup();
    render(<ConnectionsWorkspace />);

    const inspector = screen.getByRole("region", { name: "事务检查器" });
    const expandConnection = await within(inspector).findByRole("button", {
      name: "展开 连接",
    });
    expect(expandConnection).toHaveAttribute("aria-expanded", "false");
    expect(within(inspector).queryByText("客户端地址")).not.toBeInTheDocument();

    await user.click(expandConnection);
    await waitFor(() => {
      const processView = within(inspector).getByTitle("C:\\Apps\\client.exe");
      expect(processView.querySelector("img")).toHaveAttribute(
        "src",
        "data:image/png;base64,aWNvbg==",
      );
    });
    expect(within(inspector).getByText("客户端地址")).toBeVisible();
    await user.click(
      within(inspector).getByRole("button", { name: "收缩 连接" }),
    );
    expect(within(inspector).queryByText("客户端地址")).not.toBeInTheDocument();
  });

  it.each(["socks", "tunnel"] as const)(
    "%s 原始流结构树按方向和单包发出精确选择",
    async (protocol) => {
      const user = userEvent.setup();
      const streamTransaction = createTransactionSummary({
        transactionId: `${protocol}-transaction-stream`,
        protocol,
        method: "CONNECT",
        host: "www.clash.com",
        port: 443,
        path: "/",
        urlDisplay: "https://www.clash.com:443",
        status: "failed",
      });
      const selectPacket = vi.fn();
      storeMocks.getTransactionDetail.mockImplementation(
        async (): Promise<TransactionDetail> => ({
          revision: 1,
          transaction: streamTransaction,
          requestHeaders: [],
          responseHeaders: [],
          requestBody: {
            transactionId: streamTransaction.transactionId,
            side: "request",
            contentType: "application/octet-stream",
            encoding: "binary",
            storedBytes: 7,
            originalBytes: 7,
            truncated: false,
          },
          responseBody: {
            transactionId: streamTransaction.transactionId,
            side: "response",
            contentType: "application/octet-stream",
            encoding: "binary",
            storedBytes: 5,
            originalBytes: 5,
            truncated: false,
          },
          requestPackets: [
            {
              sequence: 1,
              capturedAtMilliseconds: 1_700_000_000_010,
              storedOffsetBytes: 0,
              storedBytes: 7,
              originalBytes: 7,
              truncated: false,
              action: "forward",
              modifications: [],
            },
          ],
          responsePackets: [
            {
              sequence: 1,
              capturedAtMilliseconds: 1_700_000_000_020,
              storedOffsetBytes: 0,
              storedBytes: 5,
              originalBytes: 5,
              truncated: false,
              action: "replace",
              modifications: [
                {
                  offsetBytes: 1,
                  originalBytes: [0x01],
                  modifiedBytes: [0x00],
                },
              ],
            },
          ],
        }),
      );

      render(
        <TransactionNavigator
          onSelectPacket={selectPacket}
          onSelectTransaction={vi.fn()}
          selectedHost={null}
          selectedPacket={null}
          selectedTransactionId={null}
          transactionPage={{
            ...createServiceSnapshot().transactions,
            items: [streamTransaction],
            total: 1,
          }}
        />,
      );

      const streamRoot = await screen.findByRole("button", {
        name: "https://www.clash.com:443",
      });
      expect(streamRoot).toBeVisible();
      expect(streamRoot.querySelector(".transactionStatusDot")).toBeNull();
      const detailCallsBeforeExpand =
        storeMocks.getTransactionDetail.mock.calls.length;
      expect(
        screen.queryByRole("button", { name: "请求 7 B" }),
      ).not.toBeInTheDocument();
      expect(storeMocks.getTransactionDetail).toHaveBeenCalledTimes(
        detailCallsBeforeExpand,
      );
      await user.click(
        screen.getByRole("button", {
          name: "展开 https://www.clash.com:443",
        }),
      );
      expect(
        screen
          .getByRole("button", {
            name: "https://www.clash.com:443 127.0.0.1:50100 失败",
          })
          .querySelector(".transactionStatusDot--danger"),
      ).toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: "请求 7 B" }),
      ).not.toBeInTheDocument();
      expect(storeMocks.getTransactionDetail).toHaveBeenCalledTimes(
        detailCallsBeforeExpand,
      );
      await user.click(
        screen.getByRole("button", {
          name: "展开 https://www.clash.com:443 127.0.0.1:50100",
        }),
      );
      await user.click(screen.getByRole("button", { name: "展开 请求" }));
      await user.click(await screen.findByRole("button", { name: "请求 7 B" }));
      expect(selectPacket).toHaveBeenCalledWith({
        transactionId: `${protocol}-transaction-stream`,
        side: "request",
        sequence: 1,
      });
      await user.click(screen.getByRole("button", { name: "响应" }));
      expect(selectPacket).toHaveBeenLastCalledWith({
        transactionId: `${protocol}-transaction-stream`,
        side: "response",
        sequence: null,
      });
      await user.click(screen.getByRole("button", { name: "展开 响应" }));
      await user.click(
        await screen.findByRole("button", { name: "响应 替换 5 B" }),
      );
      expect(selectPacket).toHaveBeenLastCalledWith({
        transactionId: `${protocol}-transaction-stream`,
        side: "response",
        sequence: 1,
      });
    },
  );

  /** 连接根节点是可复制的定位信息，协议、主机和端口三部分都不得被展示层省略。 */
  it("原始流树完整显示 TCP、UDP 与 HTTPS 地址", async () => {
    const page = createServiceSnapshot().transactions;
    const transactions = [
      createTransactionSummary({
        transactionId: "stream-tcp-address",
        protocol: "socks",
        method: "CONNECT",
        host: "36.155.202.73",
        port: 10012,
        urlDisplay: "tcp://36.155.202.73:10012",
      }),
      createTransactionSummary({
        transactionId: "stream-udp-address",
        protocol: "socks",
        method: "UDP ASSOCIATE",
        host: "36.155.202.73",
        port: 10012,
        urlDisplay: "udp://36.155.202.73:10012",
      }),
      createTransactionSummary({
        transactionId: "stream-https-address",
        protocol: "socks",
        method: "CONNECT",
        host: "36.155.202.73",
        port: 10012,
        urlDisplay: "https://36.155.202.73:10012",
      }),
    ];

    render(
      <TransactionNavigator
        onSelectPacket={vi.fn()}
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedPacket={null}
        selectedTransactionId={null}
        transactionPage={{ ...page, items: transactions, total: 3 }}
      />,
    );

    for (const address of transactions.map(
      (transaction) => transaction.urlDisplay,
    )) {
      expect(screen.getByRole("button", { name: address })).toBeVisible();
    }
  });

  /**
   * 原始流方向的六个页签必须各自承担独立职责；尤其摘要不能重复概览，协议页也不能暴露 HTTP 响应校验器。
   */
  it("原始流方向逐页展示概览、逐包摘要、时间轴与同方向协议解码", async () => {
    const user = userEvent.setup();
    const streamTransaction = createTransactionSummary({
      transactionId: "transaction-stream-inspector",
      protocol: "socks",
      method: "CONNECT",
      host: "www.clash.com",
      port: 443,
      path: "",
      urlDisplay: "tcp://www.clash.com:443",
      sizes: {
        requestHeaderBytes: 0,
        requestBodyBytes: 0,
        responseHeaderBytes: 0,
        responseBodyBytes: 12,
      },
    });
    const streamDetail: TransactionDetail = {
      revision: 1,
      transaction: streamTransaction,
      requestHeaders: [],
      responseHeaders: [],
      requestBody: null,
      responseBody: {
        transactionId: streamTransaction.transactionId,
        side: "response",
        contentType: "application/octet-stream",
        encoding: "binary",
        storedBytes: 10,
        originalBytes: 12,
        truncated: true,
      },
      requestPackets: [],
      responsePackets: [
        {
          sequence: 1,
          capturedAtMilliseconds:
            streamTransaction.timings.startAtMilliseconds + 5,
          storedOffsetBytes: 0,
          storedBytes: 4,
          originalBytes: 4,
          truncated: false,
          action: "replace",
          modifications: [
            {
              offsetBytes: 1,
              originalBytes: [0x01],
              modifiedBytes: [0x00],
            },
          ],
        },
        {
          sequence: 2,
          capturedAtMilliseconds:
            streamTransaction.timings.startAtMilliseconds + 20,
          storedOffsetBytes: 4,
          storedBytes: 6,
          originalBytes: 8,
          truncated: true,
          action: "drop",
          modifications: [],
        },
      ],
    };
    const baseSnapshot = createServiceSnapshot();
    storeMocks.snapshot = createServiceSnapshot({
      recording: {
        ...baseSnapshot.recording,
        transactionCount: 1,
      },
      transactions: {
        ...baseSnapshot.transactions,
        total: 1,
        items: [streamTransaction],
      },
    });
    storeMocks.getTransactionDetail.mockReset().mockResolvedValue(streamDetail);
    storeMocks.getTransactionBody.mockResolvedValue({
      revision: 1,
      meta: {
        transactionId: streamTransaction.transactionId,
        side: "response",
        contentType: "application/octet-stream",
        encoding: "binary",
        storedBytes: 10,
        originalBytes: 12,
        truncated: true,
      },
      base64: window.btoa(
        String.fromCharCode(0xaa, 0x00, 0xbb, 0xcc, 1, 2, 3, 4, 5, 6),
      ),
    });

    render(<ConnectionsWorkspace />);
    await user.click(
      await screen.findByRole("button", {
        name: "展开 tcp://www.clash.com:443",
      }),
    );
    await user.click(
      screen.getByRole("button", {
        name: "展开 tcp://www.clash.com:443 127.0.0.1:50100",
      }),
    );
    await user.click(await screen.findByRole("button", { name: "响应" }));
    await user.click(screen.getByRole("button", { name: "展开 响应" }));
    expect(screen.getByRole("button", { name: "响应 丢弃 8 B" })).toBeVisible();
    expect(screen.getByText("响应 替换")).toHaveClass(
      "streamPacketAction",
      "isreplace",
    );
    const inspector = screen.getByRole("region", { name: "事务检查器" });

    expect(within(inspector).getByText("原始字节")).toBeVisible();
    expect(within(inspector).queryByText("相对时间")).not.toBeInTheDocument();

    await user.click(within(inspector).getByRole("tab", { name: "摘要" }));
    expect(
      within(inspector).getByRole("table", { name: "数据包摘要" }),
    ).toBeVisible();
    expect(within(inspector).getByText("相对时间")).toBeVisible();
    expect(within(inspector).getByText("6 B · 已截断")).toBeVisible();
    expect(within(inspector).queryByText("原始字节")).not.toBeInTheDocument();

    await user.click(within(inspector).getByRole("tab", { name: "图表" }));
    expect(within(inspector).getByText("响应时间轴")).toBeVisible();
    expect(within(inspector).getByText("+20 ms")).toBeVisible();
    expect(within(inspector).getByText("8 B · +20 ms")).toBeVisible();

    await user.click(within(inspector).getByRole("tab", { name: "协议解析" }));
    expect(
      await within(inspector).findByText(/Protobuf 解码未启用/),
    ).toBeVisible();
    expect(within(inspector).queryByText("验证响应")).not.toBeInTheDocument();
    expect(storeMocks.decodeProtobuf).toHaveBeenCalledWith(
      streamTransaction.transactionId,
      "response",
      expect.any(AbortSignal),
    );
    expect(storeMocks.getValidateConfiguration).not.toHaveBeenCalled();
    expect(storeMocks.getValidationReports).not.toHaveBeenCalled();

    await user.click(
      screen.getByRole("button", { name: "响应 替换 4 B" }),
    );
    expect(
      within(inspector).getByRole("tab", { name: "内容", selected: true }),
    ).toBeVisible();
    expect(
      await within(inspector).findByRole("region", { name: "十六进制" }),
    ).toBeVisible();
    expect(within(inspector).getByText("01/00")).toHaveClass(
      "hexDumpModifiedByte",
    );
    expect(
      within(inspector).queryByRole("region", { name: "WPE 修改差异" }),
    ).not.toBeInTheDocument();
    expect(
      within(inspector).queryByRole("region", { name: "数据包概览" }),
    ).not.toBeInTheDocument();

    await user.click(within(inspector).getByRole("tab", { name: "概览" }));
    expect(within(inspector).getByText("第 1 包")).toBeVisible();
    const packetOverview = within(inspector).getByRole("region", {
      name: "数据包概览",
    });
    expect(
      within(inspector).getByRole("tab", { name: "概览", selected: true }),
    ).toBeVisible();
    expect(within(packetOverview).getByText("捕获时间")).toBeVisible();
    expect(within(packetOverview).getByText("相对时间")).toBeVisible();
    expect(within(packetOverview).getByText("+5 ms")).toBeVisible();
    expect(within(packetOverview).getByText("原始大小")).toBeVisible();
    expect(within(packetOverview).getByText("已保存大小")).toBeVisible();
    expect(within(packetOverview).getByText("正文偏移")).toBeVisible();
    expect(within(packetOverview).getByText("完整性")).toBeVisible();
    expect(within(packetOverview).getByText("完整")).toBeVisible();
    expect(within(packetOverview).getByText("WPE 已修改（1 处）")).toBeVisible();
    expect(
      within(inspector).getByRole("tab", { name: "概览", selected: true }),
    ).toBeVisible();
    expect(
      within(inspector).queryByRole("region", { name: "十六进制" }),
    ).not.toBeInTheDocument();
  });
});
