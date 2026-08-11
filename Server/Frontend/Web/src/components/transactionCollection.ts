import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import type { TransactionPage, TransactionSummary } from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";

const transactionCollectionBatchSize = 1_000;
const historyRetryInitialDelayMilliseconds = 100;
const historyRetryMaximumDelayMilliseconds = 2_000;

interface TransactionOffsetRange {
  start: number;
  end: number;
}

interface IndexedTransactionSummary {
  fingerprint: string;
  summary: TransactionSummary;
}

interface TransactionCollectionState {
  appliedPageRevision: number;
  collectionToken: string;
  itemsTruncated: boolean;
  loadFailureCount: number;
  revision: number;
}

export interface CompleteTransactionCollection {
  items: TransactionSummary[];
  itemsTruncated: boolean;
  loadFailed: boolean;
  loading: boolean;
  total: number;
}

/**
 * 将摘要序列化为变更指纹；实时尾部会重复携带未变化摘要，指纹用于避免无意义替换和渲染。
 * 运行上下文：仅处理控制接口返回的协议摘要。参数是单条摘要，返回值只用于当前浏览器会话内比较。
 * 失败语义：协议模型若引入不可序列化字段，JSON 序列化错误会直接暴露契约破坏。
 */
function transactionSummaryFingerprint(summary: TransactionSummary): string {
  return JSON.stringify(summary);
}

/**
 * 返回页面实际覆盖的 offset 半开区间；响应可能受字节预算限制而短于 limit，因此只按实际条目计算终点。
 * 参数 page 是同一 collectionToken 的分页结果，返回区间用于校验历史补齐请求没有跳项。
 */
function transactionPageOffsetRange(page: TransactionPage): TransactionOffsetRange {
  return { start: page.offset, end: page.offset + page.items.length };
}

/**
 * 维护当前录制代际的完整事务索引；offset 直接映射数组槽位，写入和缺口推进均为摊销 O(1)。
 * 运行上下文：实例由单个事务导航器独占，后端分页只负责传输，界面不再暴露分页或窗口切换。
 * 失败语义：跨代页面被忽略；摘要总量只受后端录制策略约束，不在前端静默淘汰。
 */
export class TransactionCollectionIndex {
  private collectionTokenValue: string;
  private contiguousEndValue = 0;
  private retainedCountValue = 0;
  private slots: Array<IndexedTransactionSummary | undefined> = [];
  private snapshotCache: TransactionSummary[] | null = null;
  private snapshotTotal = -1;

  /** 创建指定分页代际的空索引；token 变化时必须 reset，禁止混合不同 offset 语义。 */
  constructor(collectionToken: string) {
    this.collectionTokenValue = collectionToken;
  }

  get collectionToken(): string {
    return this.collectionTokenValue;
  }

  get retainedCount(): number {
    return this.retainedCountValue;
  }

  /**
   * 切换分页代际并释放旧摘要；FIFO 头部淘汰或清空后，旧 offset 已不具备复用价值。
   * 参数 collectionToken 标识新代际。本操作不失败，调用后索引为空。
   */
  reset(collectionToken: string): void {
    this.collectionTokenValue = collectionToken;
    this.contiguousEndValue = 0;
    this.retainedCountValue = 0;
    this.slots = [];
    this.snapshotCache = null;
    this.snapshotTotal = -1;
  }

  /**
   * 按绝对 offset 增量写入分页摘要；实时尾部和历史页共享同一路径，因此不会重复或重新排序。
   * 参数 page 必须属于当前 token。返回值表示至少一个摘要新增或内容变化；跨代页面返回 false。
   */
  applyPage(page: TransactionPage): boolean {
    if (page.collectionToken !== this.collectionTokenValue) {
      return false;
    }
    let changed = false;
    page.items.forEach((summary, itemIndex) => {
      const offset = page.offset + itemIndex;
      const fingerprint = transactionSummaryFingerprint(summary);
      const current = this.slots[offset];
      if (current?.fingerprint === fingerprint) {
        return;
      }
      if (current === undefined) {
        this.retainedCountValue += 1;
      }
      this.slots[offset] = { fingerprint, summary };
      changed = true;
    });
    while (this.slots[this.contiguousEndValue] !== undefined) {
      this.contiguousEndValue += 1;
    }
    if (changed) {
      this.snapshotCache = null;
      this.snapshotTotal = -1;
    }
    return changed;
  }

  /**
   * 返回从零开始的第一个待补齐范围；单次最多一千条，保证控制请求有界并且严格顺序推进。
   * 参数 total 是后端当前总量。返回 null 表示全部摘要已装入同一滚动区域。
   */
  firstMissingRange(total: number): TransactionOffsetRange | null {
    if (this.contiguousEndValue >= total) {
      return null;
    }
    let missingEnd = Math.min(
      total,
      this.contiguousEndValue + transactionCollectionBatchSize,
    );
    // 实时尾页通常先于历史到达；请求必须在首个已加载 offset 前停止，避免重复下载尾页并触发闪烁。
    for (
      let offset = this.contiguousEndValue + 1;
      offset < missingEnd;
      offset += 1
    ) {
      if (this.slots[offset] !== undefined) {
        missingEnd = offset;
        break;
      }
    }
    return {
      start: this.contiguousEndValue,
      end: missingEnd,
    };
  }

  /**
   * 返回按 offset 自然升序排列的全部已加载摘要；结果按修订缓存，父组件重复渲染不会重建数组。
   * 参数 total 限定当前后端可见范围，避免代际收缩时暴露越界槽位。
   */
  snapshot(total: number): TransactionSummary[] {
    if (this.snapshotCache !== null && this.snapshotTotal === total) {
      return this.snapshotCache;
    }
    const summaries: TransactionSummary[] = [];
    const visibleEnd = Math.min(total, this.slots.length);
    for (let offset = 0; offset < visibleEnd; offset += 1) {
      const indexedSummary = this.slots[offset];
      if (indexedSummary !== undefined) {
        summaries.push(indexedSummary.summary);
      }
    }
    this.snapshotCache = summaries;
    this.snapshotTotal = total;
    return summaries;
  }
}

/**
 * 自动补齐当前代际的全部摘要并持续合并实时尾部；界面只有一个滚动集合，不存在前后翻页状态。
 * 参数 transactionPage 是事件流最新页。返回值提供完整度、加载状态与当前已装入的全部摘要。
 * 失败语义：非 Abort 错误按 100ms 至 2s 指数退避；token 变化会取消旧请求并隔离晚到结果。
 */
export function useCompleteTransactionCollection(
  transactionPage: TransactionPage,
): CompleteTransactionCollection {
  const { listTransactions } = useServiceStore();
  const indexReference = useRef<TransactionCollectionIndex | null>(null);
  if (indexReference.current === null) {
    indexReference.current = new TransactionCollectionIndex(
      transactionPage.collectionToken,
    );
    indexReference.current.applyPage(transactionPage);
  }
  const index = indexReference.current;
  const [collectionState, setCollectionState] = useState<TransactionCollectionState>({
    appliedPageRevision: transactionPage.revision,
    collectionToken: transactionPage.collectionToken,
    itemsTruncated: transactionPage.itemsTruncated,
    loadFailureCount: 0,
    revision: 0,
  });

  useLayoutEffect(() => {
    if (collectionState.collectionToken !== transactionPage.collectionToken) {
      index.reset(transactionPage.collectionToken);
      index.applyPage(transactionPage);
      setCollectionState((current) => ({
        appliedPageRevision: transactionPage.revision,
        collectionToken: transactionPage.collectionToken,
        itemsTruncated: transactionPage.itemsTruncated,
        loadFailureCount: 0,
        revision: current.revision + 1,
      }));
      return;
    }

    // 索引属于可变高流量缓存，禁止在 React render 阶段写入。开发环境严格模式会重复 render，
    // 若首轮 render 已写入、提交轮又判定“无变化”，界面就会长期停在 0 条而只能靠刷新重建。
    const currentPageChanged = index.applyPage(transactionPage);
    const truncatedChanged =
      transactionPage.itemsTruncated && !collectionState.itemsTruncated;
    const pageRevisionChanged =
      collectionState.appliedPageRevision !== transactionPage.revision;
    if (currentPageChanged || truncatedChanged || pageRevisionChanged) {
      // SSE 事务帧就是实时提交边界；每帧立刻推进 React，不做定时合并或轮询刷新。
      // 索引只重建受影响槽位，界面刷新不会重复下载正文，也不会牺牲事务到达时序。
      setCollectionState((current) =>
        current.collectionToken === transactionPage.collectionToken &&
        (currentPageChanged ||
          current.appliedPageRevision !== transactionPage.revision ||
          (transactionPage.itemsTruncated && !current.itemsTruncated))
          ? {
              ...current,
              appliedPageRevision: transactionPage.revision,
              itemsTruncated:
                current.itemsTruncated || transactionPage.itemsTruncated,
              revision: current.revision + 1,
            }
          : current,
      );
    }
  }, [
    collectionState.appliedPageRevision,
    collectionState.collectionToken,
    collectionState.itemsTruncated,
    index,
    transactionPage.collectionToken,
    transactionPage.items,
    transactionPage.itemsTruncated,
    transactionPage.offset,
    transactionPage.revision,
  ]);

  const collectionMatches =
    collectionState.collectionToken === transactionPage.collectionToken &&
    index.collectionToken === transactionPage.collectionToken;
  const missingRange = collectionMatches
    ? index.firstMissingRange(transactionPage.total)
    : null;
  const missingRangeStart = missingRange?.start ?? null;
  const missingRangeEnd = missingRange?.end ?? null;

  useEffect(() => {
    if (missingRange === null || !collectionMatches) {
      return undefined;
    }
    // layout effect 会先把同一 SSE 帧写入索引；被该实时页覆盖的范围不应再发历史 GET。
    // 在副作用真正执行时复核缺口，可同时避免严格模式重复副作用与尾页跳跃造成的重复请求。
    const confirmedMissingRange = index.firstMissingRange(transactionPage.total);
    if (
      confirmedMissingRange === null ||
      confirmedMissingRange.start !== missingRange.start ||
      confirmedMissingRange.end !== missingRange.end
    ) {
      return undefined;
    }
    const abortController = new AbortController();
    const requestedRange = missingRange;
    const requestedToken = transactionPage.collectionToken;
    const retryExponent = Math.min(
      Math.max(collectionState.loadFailureCount - 1, 0),
      30,
    );
    const retryDelayMilliseconds =
      collectionState.loadFailureCount === 0
        ? 0
        : Math.min(
            historyRetryInitialDelayMilliseconds * 2 ** retryExponent,
            historyRetryMaximumDelayMilliseconds,
          );

    /**
     * 请求并提交当前唯一缺口；响应短页只推进实际返回区间，下一轮继续读取，禁止跳过任何摘要。
     * 请求失败会增加重试次数，取消或换代不会写入当前集合。
     */
    const loadMissingPage = (): void => {
      void listTransactions(
        {
          offset: requestedRange.start,
          limit: requestedRange.end - requestedRange.start,
          collectionToken: requestedToken,
        },
        abortController.signal,
      )
        .then((page) => {
          const loadedRange = transactionPageOffsetRange(page);
          if (
            page.collectionToken !== requestedToken ||
            loadedRange.start !== requestedRange.start ||
            loadedRange.end <= loadedRange.start ||
            loadedRange.end > requestedRange.end
          ) {
            throw new Error("transactionCollectionChanged");
          }
          if (abortController.signal.aborted) {
            return;
          }
          const changed = index.applyPage(page);
          setCollectionState((current) =>
            current.collectionToken === requestedToken
              ? {
                  ...current,
                  itemsTruncated: current.itemsTruncated || page.itemsTruncated,
                  loadFailureCount: 0,
                  revision: current.revision + (changed ? 1 : 0),
                }
              : current,
          );
        })
        .catch((error: unknown) => {
          if (
            abortController.signal.aborted ||
            (error instanceof Error && error.name === "AbortError")
          ) {
            return;
          }
          setCollectionState((current) =>
            current.collectionToken === requestedToken
              ? {
                  ...current,
                  loadFailureCount: current.loadFailureCount + 1,
                }
              : current,
          );
        });
    };

    const retryTimer =
      retryDelayMilliseconds === 0
        ? null
        : window.setTimeout(loadMissingPage, retryDelayMilliseconds);
    if (retryTimer === null) {
      loadMissingPage();
    }
    return () => {
      if (retryTimer !== null) {
        window.clearTimeout(retryTimer);
      }
      abortController.abort();
    };
  }, [
    collectionMatches,
    collectionState.loadFailureCount,
    index,
    listTransactions,
    missingRangeEnd,
    missingRangeStart,
    transactionPage.collectionToken,
  ]);

  const items = useMemo(
    () =>
      collectionMatches
        ? index.snapshot(transactionPage.total)
        : transactionPage.items,
    [
      collectionMatches,
      collectionState.revision,
      index,
      transactionPage.collectionToken,
      transactionPage.items,
      transactionPage.total,
    ],
  );

  return {
    items,
    itemsTruncated:
      transactionPage.itemsTruncated ||
      (collectionMatches && collectionState.itemsTruncated),
    loadFailed:
      collectionMatches &&
      missingRange !== null &&
      collectionState.loadFailureCount > 0,
    loading:
      collectionMatches &&
      missingRange !== null &&
      collectionState.loadFailureCount === 0,
    total: transactionPage.total,
  };
}
