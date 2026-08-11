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

/** 鍒涘缓鍙敱娴嬭瘯绮剧‘瀹屾垚鐨?Promise锛屽鐜伴珮棰戜簨浠惰秴杩囨湰鍦拌鎯呰姹傞€熷害鐨勭湡瀹炴椂搴忋€?*/
function deferred<Value>() {
  let resolve!: (value: Value) => void;
  const promise = new Promise<Value>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

/** 鎸夋憳瑕佹瀯閫犳渶灏忓畬鏁磋鎯咃紱revision 鐢ㄤ簬鏂█鏈€缁堟覆鏌撶殑鏄渶鏂拌ˉ璇荤粨鏋溿€?*/
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

/** 娓叉煋瀹炴椂璇︽儏鐘舵€侊紱ready 鍐呭鍦ㄥ悗鍙拌ˉ璇绘湡闂村繀椤绘寔缁瓨鍦ㄣ€?*/
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

describe("瀹炴椂浜嬪姟璇︽儏璇诲彇", () => {
  it("楂橀鎽樿鍙樺寲鍚堝苟涓轰竴涓渶鏂拌ˉ璇讳笖鍒锋柊鏈熼棿涓嶉棯鍥?loading", async () => {
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

