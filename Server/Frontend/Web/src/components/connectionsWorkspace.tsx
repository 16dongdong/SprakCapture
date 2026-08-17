import {
  type CSSProperties,
  type KeyboardEvent,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import type { TransactionSummary } from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";
import { useReportUiDataView } from "../state/uiContextReporter";
import {
  type InspectorView,
  TransactionInspector,
} from "./transactionInspector";
import { TransactionNavigator } from "./transactionNavigator";
import type { StreamPacketSelection } from "./streamPacketSelection";
import type { ToolDialogId } from "./toolSettingsDialog";
import type { TransactionToolSeed } from "./transactionToolSeed";
import { usePersistentSplitter } from "./usePersistentSplitter";

/*
 * 事务导航的默认宽度、分隔条和检查器最低可读宽度合计为 945px。
 * 在此阈值前切换为上下分栏，避免右侧属性表在仍使用左右布局时被压缩或遮挡。
 */
const transactionWorkspaceStackedBreakpoint = 960;

const splitterOptions = {
  storageKey: "transactionsNavigatorSize",
  stackedMediaQuery: `(max-width: ${transactionWorkspaceStackedBreakpoint}px)`,
  desktop: {
    defaultSize: 520,
    minimumSize: 360,
    maximumSize: 920,
    minimumDetailSize: 420,
  },
  stacked: {
    defaultSize: 280,
    minimumSize: 180,
    maximumSize: 520,
    minimumDetailSize: 180,
  },
};

const keyboardResizeStep = 16;

interface ConnectionsWorkspaceProps {
  onOpenSslSettings?(seed: TransactionToolSeed, focusClientCertificate?: boolean): void;
  onOpenToolSettings?(tool: ToolDialogId, seed: TransactionToolSeed): void;
}

/**
 * 渲染响应式事务工作台；导航只读取 snapshot.transactions.items，宽屏左右分栏、窄屏上下分栏。
 *
 * 运行上下文：连接会话路由挂载后持续消费服务快照，并把事务右键菜单接到主窗口现有工具编辑器。
 * 参数：onOpenSslSettings 与 onOpenToolSettings 分别打开已有 SSL 和规则配置界面。
 * 失败语义：快照不可用时显示明确重试入口，工具回调缺失时右键菜单不会渲染对应项目。
 */
export function ConnectionsWorkspace({
  onOpenSslSettings,
  onOpenToolSettings,
}: ConnectionsWorkspaceProps) {
  const { t } = useTranslation();
  const { snapshot, lastError, refresh } = useServiceStore();
  const [selectedTransactionId, setSelectedTransactionId] = useState<
    string | null
  >(null);
  const [selectedPacket, setSelectedPacket] =
    useState<StreamPacketSelection | null>(null);
  const [inspectorView, setInspectorView] =
    useState<InspectorView>("overview");
  const [selectedTransactionSnapshot, setSelectedTransactionSnapshot] =
    useState<TransactionSummary | null>(null);
  const selectedRecordingSessionIdRef = useRef<string | null>(null);
  const workspaceRef = useRef<HTMLDivElement>(null);
  const {
    navigatorSize,
    orientation,
    minimumSize,
    maximumSize,
    beginResize,
    resetSize,
    resizeBy,
  } = usePersistentSplitter(workspaceRef, splitterOptions);
  const transactions = snapshot?.transactions.items ?? [];
  const transactionTotal = snapshot?.transactions.total ?? 0;
  const recordingSessionId = snapshot?.recording.recordingSessionId ?? null;
  const transactionCollectionToken =
    snapshot?.transactions.collectionToken ?? null;
  const selectedCollectionTokenRef = useRef<string | null>(null);

  useEffect(() => {
    if (
      selectedRecordingSessionIdRef.current !== recordingSessionId ||
      selectedCollectionTokenRef.current !== transactionCollectionToken
    ) {
      // 清空事务会推进 collectionToken，但会继续复用同一录制会话。选择状态必须同时按
      // 集合代际失效，否则右侧会保留已删除事务并制造“清空后没有新录制”的错误观感。
      selectedRecordingSessionIdRef.current = recordingSessionId;
      selectedCollectionTokenRef.current = transactionCollectionToken;
      const initialTransaction = transactions[0] ?? null;
      setSelectedTransactionId(initialTransaction?.transactionId ?? null);
      setSelectedTransactionSnapshot(initialTransaction);
      setSelectedPacket(null);
      return;
    }
    const currentTransaction = transactions.find(
      (transaction) => transaction.transactionId === selectedTransactionId,
    );
    if (currentTransaction !== undefined) {
      setSelectedTransactionSnapshot(currentTransaction);
      return;
    }
    if (selectedTransactionId !== null && transactionTotal > 0) {
      // 实时页是固定窗口；选中事务滑出尾页不代表后端已删除它。保留选择快照可阻止
      // 高频追加把检查器持续切换到新的尾页首项，历史详情和媒体元素因此保持稳定。
      return;
    }
    const initialTransaction = transactions[0] ?? null;
    setSelectedTransactionId(initialTransaction?.transactionId ?? null);
    setSelectedTransactionSnapshot(initialTransaction);
    setSelectedPacket(null);
  }, [
    recordingSessionId,
    transactionCollectionToken,
    selectedTransactionId,
    transactionTotal,
    transactions,
  ]);

  const liveSelectedTransaction = transactions.find(
    (transaction) => transaction.transactionId === selectedTransactionId,
  );
  const selectedTransaction =
    liveSelectedTransaction ??
    (selectedTransactionSnapshot?.transactionId === selectedTransactionId
      ? selectedTransactionSnapshot
      : null);
  const reportedSelection = useMemo(() => {
    if (selectedPacket !== null && selectedPacket.sequence !== null) {
      return {
        kind: "streamPacket" as const,
        ids: [selectedPacket.transactionId],
        side: selectedPacket.side,
        sequence: selectedPacket.sequence,
      };
    }
    return selectedTransactionId === null
      ? null
      : {
          kind: "transaction" as const,
          ids: [selectedTransactionId],
          side: null,
          sequence: null,
        };
  }, [selectedPacket, selectedTransactionId]);
  useReportUiDataView(inspectorView, reportedSelection);

  /**
   * 选择方向或流片段时同时同步所属事务；包被滚动录制淘汰后的方向回退也复用此唯一状态入口。
   */
  const selectPacket = (selection: StreamPacketSelection) => {
    setSelectedTransactionId(selection.transactionId);
    setSelectedPacket(selection);
  };

  /**
   * 选择事务根节点或普通事务时清除片段选择，避免右侧继续展示上一次连接的片段。
   */
  const selectTransaction = (
    transactionId: string,
    transaction: TransactionSummary,
  ) => {
    setSelectedTransactionId(transactionId);
    setSelectedTransactionSnapshot(transaction);
    setSelectedPacket(null);
  };

  /**
   * 通过方向键调整当前分栏轴；键盘语义始终与 aria-orientation 和响应式布局一致。
   */
  const resizeWithKeyboard = (event: KeyboardEvent<HTMLDivElement>) => {
    const decreaseKey =
      orientation === "vertical" ? "ArrowLeft" : "ArrowUp";
    const increaseKey =
      orientation === "vertical" ? "ArrowRight" : "ArrowDown";
    if (event.key !== decreaseKey && event.key !== increaseKey) {
      return;
    }
    event.preventDefault();
    resizeBy(
      event.key === decreaseKey ? -keyboardResizeStep : keyboardResizeStep,
    );
  };

  if (snapshot === null) {
    return (
      <main
        className="connectionsWorkspace connectionsWorkspace--unavailable"
        aria-label={t("transactions.workspace.regionLabel")}
      >
        <div className="emptyState">
          <strong>
            {lastError === null
              ? t("viewer.detailLoading")
              : t("viewer.detailFailed")}
          </strong>
          {lastError !== null && (
            <>
              <span>{lastError}</span>
              <button type="button" onClick={() => void refresh()}>
                {t("viewer.retry")}
              </button>
            </>
          )}
        </div>
      </main>
    );
  }

  return (
    <main
      aria-label={t("transactions.workspace.regionLabel")}
      className="connectionsWorkspace"
      ref={workspaceRef}
      style={{ "--navigator-size": `${navigatorSize}px` } as CSSProperties}
    >
      <TransactionNavigator
        transactionPage={snapshot.transactions}
        selectedTransactionId={selectedTransactionId}
        selectedPacket={selectedPacket}
        selectedHost={selectedTransaction?.host ?? null}
        onSelectTransaction={selectTransaction}
        onSelectPacket={selectPacket}
        onOpenSslSettings={onOpenSslSettings}
        onOpenToolSettings={onOpenToolSettings}
      />
      <div
        className="workspaceSplitter"
        role="separator"
        aria-label={t(
          orientation === "vertical"
            ? "transactions.workspace.splitterNavigationWidth"
            : "transactions.workspace.splitterNavigationHeight",
        )}
        aria-orientation={orientation}
        aria-valuemin={minimumSize}
        aria-valuemax={maximumSize}
        aria-valuenow={navigatorSize}
        tabIndex={0}
        onDoubleClick={resetSize}
        onKeyDown={resizeWithKeyboard}
        onPointerDown={beginResize}
      />
      <TransactionInspector
        onViewChange={setInspectorView}
        onPacketUnavailable={selectPacket}
        selectedPacket={selectedPacket}
        transaction={selectedTransaction}
      />
    </main>
  );
}
