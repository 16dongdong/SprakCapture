import { describe, expect, it, vi } from "vitest";

import type { TransactionDetail } from "@/api/protocol";
import { createTransactionDetailRepository } from "@/state/transactionDetailRepository";

const detail = { transaction: { transactionId: "transaction-1" } } as TransactionDetail;

describe("transactionDetailRepository", () => {
  it("同一事务版本共享在途请求和缓存结果", async () => {
    let resolveDetail: ((value: TransactionDetail) => void) | undefined;
    const loader = vi.fn(
      () =>
        new Promise<TransactionDetail>((resolve) => {
          resolveDetail = resolve;
        }),
    );
    const repository = createTransactionDetailRepository(loader);

    const first = repository.read("transaction-1", "revision-1");
    const second = repository.read("transaction-1", "revision-1");
    await Promise.resolve();
    expect(loader).toHaveBeenCalledTimes(1);
    resolveDetail?.(detail);

    await expect(first).resolves.toBe(detail);
    await expect(second).resolves.toBe(detail);
    await expect(repository.read("transaction-1", "revision-1")).resolves.toBe(
      detail,
    );
    expect(loader).toHaveBeenCalledTimes(1);
  });

  it("版本推进后重新读取，单个订阅者取消不终止共享读取", async () => {
    const loader = vi.fn(async () => detail);
    const repository = createTransactionDetailRepository(loader);
    await repository.read("transaction-1", "revision-1");

    const abortController = new AbortController();
    const cancelled = repository.read(
      "transaction-1",
      "revision-2",
      abortController.signal,
    );
    const active = repository.read("transaction-1", "revision-2");
    abortController.abort();

    await expect(cancelled).rejects.toMatchObject({ name: "AbortError" });
    await expect(active).resolves.toBe(detail);
    expect(loader).toHaveBeenCalledTimes(2);
  });

  it("容量压力不淘汰在途条目且相同版本始终单飞", async () => {
    const resolvers = new Map<string, (value: TransactionDetail) => void>();
    const loader = vi.fn(
      (transactionId: string) =>
        new Promise<TransactionDetail>((resolve) => {
          resolvers.set(transactionId, resolve);
        }),
    );
    const repository = createTransactionDetailRepository(loader);
    const pendingReads = Array.from({ length: 129 }, (_, index) =>
      repository.read(`transaction-${index}`, "revision-1"),
    );
    const duplicateFirst = repository.read("transaction-0", "revision-1");
    await Promise.resolve();

    expect(loader).toHaveBeenCalledTimes(129);
    for (const [transactionId, resolve] of resolvers) {
      resolve({
        transaction: { transactionId },
      } as TransactionDetail);
    }
    await expect(Promise.all([...pendingReads, duplicateFirst])).resolves.toHaveLength(
      130,
    );
  });
});
