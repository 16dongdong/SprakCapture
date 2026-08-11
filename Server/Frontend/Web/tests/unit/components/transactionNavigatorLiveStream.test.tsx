import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { TransactionDetail } from "@/api/protocol";
import { TransactionNavigator } from "@/components/transactionNavigator";
import {
  createServiceSnapshot,
  createTransactionSummary,
} from "#tests/testFixtures";

const getLiveTransactionDetail = vi.hoisted(() => vi.fn());

vi.mock("@/state/serviceStore", () => ({
  useServiceStore: () => ({ getLiveTransactionDetail }),
}));

describe("原始流事件刷新", () => {
  it("SOCKS 活动事务实时显示新增包并在结束后补齐方向计数", async () => {
    const user = userEvent.setup();
    const pendingTransaction = createTransactionSummary({
      transactionId: "transaction-stream-refresh",
      protocol: "socks",
      method: "CONNECT",
      host: "www.baidu.com",
      port: 443,
      path: "",
      urlDisplay: "https://www.baidu.com:443",
      status: "pending",
      statusCode: null,
      sizes: {
        requestHeaderBytes: 0,
        requestBodyBytes: 0,
        responseHeaderBytes: 0,
        responseBodyBytes: 1,
      },
    });
    const growingTransaction = createTransactionSummary({
      ...pendingTransaction,
      sizes: { ...pendingTransaction.sizes, responseBodyBytes: 2 },
    });
    const completedTransaction = createTransactionSummary({
      ...pendingTransaction,
      status: "complete",
      statusCode: 0,
      sizes: { ...pendingTransaction.sizes, responseBodyBytes: 4 },
    });
    getLiveTransactionDetail
      .mockReset()
      .mockResolvedValueOnce(createStreamDetail(pendingTransaction, 1, 1))
      .mockResolvedValueOnce(createStreamDetail(growingTransaction, 2, 2))
      .mockResolvedValueOnce(createStreamDetail(completedTransaction, 3, 4));

    const page = createServiceSnapshot().transactions;
    const { rerender } = render(
      <TransactionNavigator
        onSelectPacket={vi.fn()}
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedPacket={null}
        selectedTransactionId={null}
        transactionPage={{ ...page, items: [pendingTransaction], total: 1 }}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: "展开 https://www.baidu.com:443" }),
    );
    await user.click(
      screen.getByRole("button", {
        name: "展开 https://www.baidu.com:443 127.0.0.1:50100",
      }),
    );
    const activeResponseButton = await screen.findByRole("button", {
      name: "响应",
    });
    expect(
      within(activeResponseButton.parentElement!).getByText("1"),
    ).toBeVisible();

    rerender(
      <TransactionNavigator
        onSelectPacket={vi.fn()}
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedPacket={null}
        selectedTransactionId={null}
        transactionPage={{
          ...page,
          revision: page.revision + 1,
          items: [growingTransaction],
          total: 1,
        }}
      />,
    );
    await expectResponsePacketCount("2");

    rerender(
      <TransactionNavigator
        onSelectPacket={vi.fn()}
        onSelectTransaction={vi.fn()}
        selectedHost={null}
        selectedPacket={null}
        selectedTransactionId={null}
        transactionPage={{
          ...page,
          revision: page.revision + 2,
          items: [completedTransaction],
          total: 1,
        }}
      />,
    );
    await expectResponsePacketCount("4");
    await user.click(screen.getByRole("button", { name: "展开 响应" }));
    expect(screen.getAllByRole("button", { name: "响应 1 B" })).toHaveLength(4);
    expect(getLiveTransactionDetail).toHaveBeenCalledTimes(3);
  });
});

/** 构造活动原始流详情；packetCount 同时驱动正文长度和包索引，避免测试夹具自相矛盾。 */
function createStreamDetail(
  transaction: ReturnType<typeof createTransactionSummary>,
  revision: number,
  packetCount: number,
): TransactionDetail {
  return {
    revision,
    transaction,
    requestHeaders: [],
    responseHeaders: [],
    requestBody: null,
    responseBody: {
      transactionId: transaction.transactionId,
      side: "response",
      contentType: "application/octet-stream",
      encoding: "binary",
      storedBytes: packetCount,
      originalBytes: packetCount,
      truncated: false,
    },
    requestPackets: [],
    responsePackets: Array.from({ length: packetCount }, (_, index) => ({
      sequence: index + 1,
      capturedAtMilliseconds: 1_700_000_000_010 + index,
      storedOffsetBytes: index,
      storedBytes: 1,
      originalBytes: 1,
      truncated: false,
      action: "forward",
      modifications: [],
    })),
  };
}

/** 等待响应方向的实时包计数更新；超时表示 SSE 摘要没有驱动详情补读。 */
async function expectResponsePacketCount(expectedCount: string): Promise<void> {
  await waitFor(() => {
    expect(
      within(screen.getByRole("button", { name: "响应" }).parentElement!).getByText(
        expectedCount,
      ),
    ).toBeVisible();
  });
}
