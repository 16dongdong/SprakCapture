import { describe, expect, it, vi } from "vitest";

import type { TransactionPage } from "@/api/protocol";
import { TransactionCollectionIndex } from "@/components/transactionCollection";
import {
  createServiceSnapshot,
  createTransactionSummary,
} from "#tests/testFixtures";

/**
 * 创建指定 offset 范围的稳定分页夹具；sequence 使用 offset+1，便于验证全量索引顺序和缺口边界。
 * 参数 token/offset/count/total 描述分页契约，返回值可直接写入集合索引。
 */
function createTransactionPage(
  token: string,
  offset: number,
  count: number,
  total: number,
): TransactionPage {
  const basePage = createServiceSnapshot().transactions;
  return {
    ...basePage,
    collectionToken: token,
    total,
    offset,
    limit: count,
    hasPrevious: offset > 0,
    hasMore: offset + count < total,
    nextOffset: offset + count < total ? offset + count : null,
    truncated: offset + count < total,
    items: Array.from({ length: count }, (_, itemIndex) => {
      const sequence = offset + itemIndex + 1;
      return createTransactionSummary({
        transactionId: `complete-${sequence}`,
        sequence,
        host: "volume.example",
        path: `/${sequence}`,
        urlDisplay: `http://volume.example/${sequence}`,
      });
    }),
  };
}

describe("事务连续集合索引", () => {
  it("先收到实时尾部后从零自动补齐全部缺口", () => {
    const index = new TransactionCollectionIndex("complete-history");
    index.applyPage(createTransactionPage("complete-history", 2_000, 500, 2_500));
    expect(index.firstMissingRange(2_500)).toEqual({ start: 0, end: 1_000 });

    index.applyPage(createTransactionPage("complete-history", 0, 1_000, 2_500));
    expect(index.firstMissingRange(2_500)).toEqual({ start: 1_000, end: 2_000 });
    index.applyPage(createTransactionPage("complete-history", 1_000, 1_000, 2_500));

    expect(index.firstMissingRange(2_500)).toBeNull();
    expect(index.retainedCount).toBe(2_500);
    expect(index.snapshot(2_500)[0]?.sequence).toBe(1);
    expect(index.snapshot(2_500).at(-1)?.sequence).toBe(2_500);
  });

  it("十万事务与一万次高频尾部更新不调用排序也不淘汰历史", () => {
    const total = 100_000;
    const index = new TransactionCollectionIndex("high-volume");
    const sortSpy = vi.spyOn(Array.prototype, "sort");
    try {
      for (let offset = 0; offset < total; offset += 1_000) {
        index.applyPage(
          createTransactionPage("high-volume", offset, 1_000, total),
        );
      }

      for (let updateIndex = 0; updateIndex < 10_000; updateIndex += 1) {
        const updatedTail = createTransactionPage(
          "high-volume",
          total - 1,
          1,
          total,
        );
        updatedTail.items[0] = {
          ...updatedTail.items[0],
          sizes: {
            ...updatedTail.items[0].sizes,
            responseBodyBytes: updateIndex,
          },
        };
        index.applyPage(updatedTail);
      }

      const snapshot = index.snapshot(total);
      expect(index.retainedCount).toBe(total);
      expect(snapshot).toHaveLength(total);
      expect(snapshot[0]?.sequence).toBe(1);
      expect(snapshot.at(-1)?.sequence).toBe(total);
      expect(new Set(snapshot.map((item) => item.transactionId))).toHaveLength(
        total,
      );
      expect(sortSpy).not.toHaveBeenCalled();
    } finally {
      sortSpy.mockRestore();
    }
  });

  it("切换录制代际后释放旧摘要并忽略晚到页面", () => {
    const index = new TransactionCollectionIndex("old-generation");
    index.applyPage(createTransactionPage("old-generation", 0, 10, 10));
    index.reset("new-generation");

    expect(index.applyPage(createTransactionPage("old-generation", 0, 10, 10))).toBe(
      false,
    );
    expect(index.retainedCount).toBe(0);
    expect(index.firstMissingRange(2)).toEqual({ start: 0, end: 2 });

    index.applyPage(createTransactionPage("new-generation", 0, 2, 2));
    expect(index.snapshot(2).map((item) => item.sequence)).toEqual([1, 2]);
  });
});
