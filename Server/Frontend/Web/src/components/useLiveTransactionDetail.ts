import { useCallback, useEffect, useRef, useState } from "react";

import type { TransactionDetail, TransactionSummary } from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";

export type LiveTransactionDetailState =
  | { kind: "empty" }
  | { kind: "loading" }
  | { kind: "ready"; detail: TransactionDetail }
  | { kind: "error" };

interface DetailTarget {
  enabled: boolean;
  generation: number;
  transactionId: string | null;
}

/**
 * 为事务摘要生成内容代际键；字段来自 SSE 权威摘要，任何会影响详情、正文或媒体聚合的变化都会推进该键。
 * 运行上下文：只对当前选中或已经展开的少量事务调用，完整序列化可避免遗漏后续协议字段。
 */
export function transactionDetailRevision(
  transaction: TransactionSummary,
): string {
  return JSON.stringify(transaction);
}

/**
 * 事件驱动地读取事务详情，并把高频摘要变化合并为至多一个在途请求和一个最新补读。
 *
 * 运行上下文：导航树和右侧检查器共享该 hook。`revision` 来自 SSE 摘要，`retryVersion`
 * 只由显式重试按钮推进。刷新期间保留最近一次 ready 内容，因此活动流不会闪回加载占位。
 * 失败语义：首次请求失败返回 error；已有可用详情时刷新失败继续保留旧详情，等待下一事件或显式重试。
 */
export function useLiveTransactionDetail(options: {
  enabled: boolean;
  retryVersion?: number;
  revision: string;
  transactionId: string | null;
}): LiveTransactionDetailState {
  const { enabled, retryVersion = 0, revision, transactionId } = options;
  const { getLiveTransactionDetail } = useServiceStore();
  const requestedContentRevision = `${revision}\u0000${retryVersion}`;
  const [state, setState] = useState<LiveTransactionDetailState>({
    kind: transactionId === null || !enabled ? "empty" : "loading",
  });
  const targetRef = useRef<DetailTarget>({
    enabled: false,
    generation: 0,
    transactionId: null,
  });
  const desiredRevisionRef = useRef(requestedContentRevision);
  const loadedRevisionRef = useRef<string | null>(null);
  const activeRequestRef = useRef<AbortController | null>(null);

  /**
   * 启动当前代际唯一详情读取；请求期间的新 revision 只更新 desired，完成后直接补读最新版本。
   * 网络失败不会清除已经展示的详情，避免短暂控制面抖动造成右侧内容闪白。
   */
  const requestLatestDetail = useCallback(function requestLatestDetail(): void {
    const target = targetRef.current;
    if (
      !target.enabled ||
      target.transactionId === null ||
      activeRequestRef.current !== null ||
      loadedRevisionRef.current === desiredRevisionRef.current
    ) {
      return;
    }
    const requestGeneration = target.generation;
    const requestedRevision = desiredRevisionRef.current;
    const abortController = new AbortController();
    activeRequestRef.current = abortController;
    void getLiveTransactionDetail(
      target.transactionId,
      requestedRevision,
      abortController.signal,
    )
      .then((detail) => {
        if (
          !abortController.signal.aborted &&
          targetRef.current.generation === requestGeneration
        ) {
          loadedRevisionRef.current = requestedRevision;
          setState({ kind: "ready", detail });
        }
      })
      .catch((error: unknown) => {
        if (
          !abortController.signal.aborted &&
          targetRef.current.generation === requestGeneration
        ) {
          loadedRevisionRef.current = requestedRevision;
          setState((current) =>
            current.kind === "ready" ? current : { kind: "error" },
          );
        }
      })
      .finally(() => {
        if (activeRequestRef.current === abortController) {
          activeRequestRef.current = null;
        }
        if (
          !abortController.signal.aborted &&
          targetRef.current.generation === requestGeneration &&
          loadedRevisionRef.current !== desiredRevisionRef.current
        ) {
          requestLatestDetail();
        }
      });
  }, [getLiveTransactionDetail]);

  useEffect(() => {
    activeRequestRef.current?.abort();
    activeRequestRef.current = null;
    const generation = targetRef.current.generation + 1;
    targetRef.current = { enabled, generation, transactionId };
    desiredRevisionRef.current = requestedContentRevision;
    loadedRevisionRef.current = null;
    if (!enabled || transactionId === null) {
      setState({ kind: "empty" });
      return undefined;
    }
    setState({ kind: "loading" });
    requestLatestDetail();
    return () => {
      if (targetRef.current.generation === generation) {
        activeRequestRef.current?.abort();
        activeRequestRef.current = null;
      }
    };
  }, [enabled, requestLatestDetail, retryVersion, transactionId]);

  useEffect(() => {
    desiredRevisionRef.current = requestedContentRevision;
    requestLatestDetail();
  }, [requestLatestDetail, requestedContentRevision]);

  return state;
}
