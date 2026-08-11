import type { TransactionDetail } from "../api/protocol";

const maximumCachedTransactionDetails = 128;

interface DetailCacheEntry {
  promise: Promise<TransactionDetail>;
  settled: boolean;
}

export interface TransactionDetailRepository {
  read(
    transactionId: string,
    revision: string,
    signal?: AbortSignal,
  ): Promise<TransactionDetail>;
}

/**
 * 为单个调用者绑定取消信号，但不取消共享中的底层请求。
 * 导航树收起时只停止自身消费，右侧检查器仍可复用同一在途结果；取消时以 AbortError 拒绝。
 */
function waitForCaller(
  sharedPromise: Promise<TransactionDetail>,
  signal?: AbortSignal,
): Promise<TransactionDetail> {
  if (signal === undefined) {
    return sharedPromise;
  }
  if (signal.aborted) {
    return Promise.reject(new DOMException("事务详情读取已取消。", "AbortError"));
  }
  return new Promise((resolve, reject) => {
    /** 在共享读取完成后移除本调用者的取消监听，避免长生命周期页面积累监听器。 */
    const removeAbortListener = () => signal.removeEventListener("abort", abort);
    /** 只拒绝当前订阅者；底层请求继续完成并写入 Provider 级缓存。 */
    const abort = () => {
      removeAbortListener();
      reject(new DOMException("事务详情读取已取消。", "AbortError"));
    };
    signal.addEventListener("abort", abort, { once: true });
    sharedPromise.then(
      (detail) => {
        removeAbortListener();
        resolve(detail);
      },
      (error: unknown) => {
        removeAbortListener();
        reject(error);
      },
    );
  });
}

/**
 * 创建 ServiceProvider 生命周期内共享的事务详情仓库。
 * 相同事务与摘要版本只发出一次 GET；失败结果立即移除，成功结果按最近使用顺序保留有限条目。
 */
export function createTransactionDetailRepository(
  loader: (transactionId: string) => Promise<TransactionDetail>,
): TransactionDetailRepository {
  const entries = new Map<string, DetailCacheEntry>();

  /**
   * 超出容量时只淘汰已完成条目；pending 请求必须保留，防止高并发中相同版本失去 singleflight。
   */
  const trimSettledEntries = () => {
    while (entries.size > maximumCachedTransactionDetails) {
      let settledKey: string | null = null;
      for (const [cacheKey, entry] of entries) {
        if (entry.settled) {
          settledKey = cacheKey;
          break;
        }
      }
      if (settledKey === null) {
        return;
      }
      entries.delete(settledKey);
    }
  };

  /** 读取指定事务版本；revision 是 SSE 摘要代际，不参与后端协议，仅用于严格失效旧缓存。 */
  const read = (
    transactionId: string,
    revision: string,
    signal?: AbortSignal,
  ): Promise<TransactionDetail> => {
    const cacheKey = `${transactionId}\u0000${revision}`;
    const cached = entries.get(cacheKey);
    if (cached !== undefined) {
      entries.delete(cacheKey);
      entries.set(cacheKey, cached);
      return waitForCaller(cached.promise, signal);
    }
    const entry: DetailCacheEntry = {
      promise: Promise.resolve().then(() => loader(transactionId)),
      settled: false,
    };
    entries.set(cacheKey, entry);
    void entry.promise.then(
      () => {
        entry.settled = true;
        trimSettledEntries();
      },
      () => {
        if (entries.get(cacheKey) === entry) {
          entries.delete(cacheKey);
        }
      },
    );
    trimSettledEntries();
    return waitForCaller(entry.promise, signal);
  };

  return { read };
}
