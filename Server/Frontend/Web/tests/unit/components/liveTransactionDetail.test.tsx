import { act, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { TransactionDetail, TransactionSummary } from "@/api/protocol";
import {
  transactionDetailRevision,
  useLiveTransactionDetail,
} from "@/components/useLiveTransactionDetail";
import { createTransactionSummary } from "#tests/testFixtures";

const getLiveTransactionDetail = vi.fn();

vi.mock("@/state/serviceStore", () => ({
  useServiceStore: () => ({ getLiveTransactionDetail }),
}));

/**
 * 创建由测试显式完成的 Promise，用于复现高频事件快于详情请求的时序。
 *
 * 函数无输入，返回 Promise 与唯一完成回调；测试若未调用回调，请求会保持挂起而不是伪造结果。
 */
function deferred<Value>() {
  let resolve!: (value: Value) => void;
  const promise = new Promise<Value>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

/**
 * 根据事务摘要和修订号构造最小完整详情，供实时详情钩子的时序断言使用。
 *
 * `transaction` 是当前摘要，`revision` 标识补读版本；夹具没有失败分支，缺失字段由类型检查直接拒绝。
 */
function createDetail(
  transaction: TransactionSummary,
  revision: number,
): TransactionDetail {
  return {
    revision,
    transaction,
    requestHeaders: [],
    responseHeaders: [],
    requestBody: null,
    responseBody: null,
    requestPackets: [],
    responsePackets: [],
  };
}

/**
 * 渲染指定事务的实时详情状态，验证后台补读期间既有 ready 内容不会退回 loading。
 *
 * `transaction` 决定查询标识和修订；钩子错误会按自身状态直接渲染，测试不会吞掉异常状态。
 */
function DetailProbe({ transaction }: { transaction: TransactionSummary }) {
  const state = useLiveTransactionDetail({
    enabled: true,
    revision: transactionDetailRevision(transaction),
    transactionId: transaction.transactionId,
  });
  return (
    <output>
      {state.kind === "ready" ? `ready:${state.detail.revision}` : state.kind}
    </output>
  );
}

describe("实时事务详情读取", () => {
  it("合并高频摘要变化并只补读最新版本且刷新期间不闪回 loading", async () => {
    const firstRequest = deferred<TransactionDetail>();
    const latestRequest = deferred<TransactionDetail>();
    getLiveTransactionDetail
      .mockReset()
      .mockReturnValueOnce(firstRequest.promise)
      .mockReturnValueOnce(latestRequest.promise);
    const initial = createTransactionSummary({
      transactionId: "live-detail",
      sizes: {
        requestHeaderBytes: 0,
        requestBodyBytes: 0,
        responseHeaderBytes: 0,
        responseBodyBytes: 1,
      },
    });
    const { rerender } = render(<DetailProbe transaction={initial} />);
    expect(screen.getByText("loading")).toBeInTheDocument();
    expect(getLiveTransactionDetail).toHaveBeenCalledTimes(1);

    for (const responseBodyBytes of [2, 3, 4]) {
      rerender(
        <DetailProbe
          transaction={createTransactionSummary({
            ...initial,
            sizes: { ...initial.sizes, responseBodyBytes },
          })}
        />,
      );
    }
    expect(getLiveTransactionDetail).toHaveBeenCalledTimes(1);

    await act(async () => firstRequest.resolve(createDetail(initial, 1)));
    expect(screen.getByText("ready:1")).toBeInTheDocument();
    await waitFor(() => expect(getLiveTransactionDetail).toHaveBeenCalledTimes(2));
    expect(screen.queryByText("loading")).not.toBeInTheDocument();

    await act(async () =>
      latestRequest.resolve(
        createDetail(
          createTransactionSummary({
            ...initial,
            sizes: { ...initial.sizes, responseBodyBytes: 4 },
          }),
          4,
        ),
      ),
    );
    expect(await screen.findByText("ready:4")).toBeInTheDocument();
    expect(getLiveTransactionDetail).toHaveBeenCalledTimes(2);
  });
});

