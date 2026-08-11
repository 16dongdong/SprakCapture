import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { TransactionDetail } from "@/api/protocol";
import { ConnectionsWorkspace } from "@/components/connectionsWorkspace";
import {
  createServiceSnapshot,
  createTransactionSummary,
} from "#tests/testFixtures";

const storeMocks = vi.hoisted(() => ({
  decodeProtobuf: vi.fn(),
  exportRecording: vi.fn(),
  getProcesses: vi.fn(),
  getResponseMediaPreview: vi.fn(),
  getTransactionBody: vi.fn(),
  getTransactionDetail: vi.fn(),
  getValidateConfiguration: vi.fn(),
  getValidationReports: vi.fn(),
  listTransactions: vi.fn(),
  refresh: vi.fn(),
  repeatTransaction: vi.fn(),
  snapshot: null as ReturnType<typeof createServiceSnapshot> | null,
  validateResponse: vi.fn(),
}));

vi.mock("@/state/serviceStore", () => ({
  useServiceStore: () => ({
    decodeProtobuf: storeMocks.decodeProtobuf,
    exportRecording: storeMocks.exportRecording,
    getLiveTransactionDetail: storeMocks.getTransactionDetail,
    getProcesses: storeMocks.getProcesses,
    getResponseMediaPreview: storeMocks.getResponseMediaPreview,
    getTransactionBody: storeMocks.getTransactionBody,
    getTransactionDetail: storeMocks.getTransactionDetail,
    getValidateConfiguration: storeMocks.getValidateConfiguration,
    getValidationReports: storeMocks.getValidationReports,
    lastError: null,
    listTransactions: storeMocks.listTransactions,
    refresh: storeMocks.refresh,
    repeatTransaction: storeMocks.repeatTransaction,
    snapshot: storeMocks.snapshot,
    validateResponse: storeMocks.validateResponse,
  }),
}));

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
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("事务工作台实时媒体", () => {
  it("未完成媒体预览只跟随事务事件更新且不存在固定频率轮询", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const mediaTransaction = createTransactionSummary({
      transactionId: "live-media-preview",
      sequence: 1,
      host: "media.example",
      path: "/song.mp4",
      urlDisplay: "https://media.example/song.mp4",
      contentType: "audio/mp4",
      status: "pending",
      sizes: {
        requestHeaderBytes: 0,
        requestBodyBytes: 0,
        responseHeaderBytes: 120,
        responseBodyBytes: 100,
      },
    });
    const growingMediaTransaction = createTransactionSummary({
      ...mediaTransaction,
      sizes: { ...mediaTransaction.sizes, responseBodyBytes: 200 },
    });
    const baseSnapshot = createServiceSnapshot();
    storeMocks.snapshot = createServiceSnapshot({
      recording: { ...baseSnapshot.recording, transactionCount: 1 },
      transactions: {
        ...baseSnapshot.transactions,
        total: 1,
        items: [mediaTransaction],
      },
    });
    storeMocks.getTransactionDetail
      .mockReset()
      .mockResolvedValueOnce(
        createMediaDetail(mediaTransaction, 1, 100),
      )
      .mockResolvedValueOnce(
        createMediaDetail(growingMediaTransaction, 2, 200),
      );
    storeMocks.getResponseMediaPreview.mockReset().mockResolvedValue({
      transactionId: mediaTransaction.transactionId,
      status: "continuousPrefix",
      mimeType: "audio/mp4",
      capturedBytes: 100,
      totalBytes: 1_000,
      streamUrl: "/api/v1/transactions/live-media-preview/media",
    });
    storeMocks.getTransactionBody.mockReset();
    storeMocks.listTransactions
      .mockReset()
      .mockResolvedValue(storeMocks.snapshot.transactions);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const { rerender } = render(<ConnectionsWorkspace />);

    await user.click(screen.getByRole("tab", { name: "内容" }));
    const responsePanel = await screen.findByRole("region", { name: "响应" });
    await user.click(within(responsePanel).getByRole("tab", { name: "预览" }));
    await waitFor(() =>
      expect(storeMocks.getResponseMediaPreview).toHaveBeenCalledTimes(1),
    );

    await act(async () => vi.advanceTimersByTime(10_000));
    expect(storeMocks.getResponseMediaPreview).toHaveBeenCalledTimes(1);

    storeMocks.snapshot = {
      ...storeMocks.snapshot!,
      revision: storeMocks.snapshot!.revision + 1,
      transactions: {
        ...storeMocks.snapshot!.transactions,
        revision: storeMocks.snapshot!.transactions.revision + 1,
        items: [growingMediaTransaction],
      },
    };
    rerender(<ConnectionsWorkspace />);
    await waitFor(() =>
      expect(storeMocks.getResponseMediaPreview).toHaveBeenCalledTimes(2),
    );
  });
});

/** 构造指定正文长度的媒体详情；revision 与 SSE 摘要同步，用于验证事件驱动补读。 */
function createMediaDetail(
  transaction: ReturnType<typeof createTransactionSummary>,
  revision: number,
  storedBytes: number,
): TransactionDetail {
  return {
    revision,
    transaction,
    requestHeaders: [],
    responseHeaders: [{ name: "content-type", value: "audio/mp4" }],
    requestBody: null,
    responseBody: {
      transactionId: transaction.transactionId,
      side: "response",
      contentType: "audio/mp4",
      encoding: "identity",
      storedBytes,
      originalBytes: storedBytes,
      truncated: false,
    },
    requestPackets: [],
    responsePackets: [],
  };
}
