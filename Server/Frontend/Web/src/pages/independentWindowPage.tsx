import { type ReactNode, useEffect, useRef, useState } from "react";
import { useLocation, useParams } from "react-router-dom";
import { useTranslation } from "react-i18next";

import { ListenerSettingsDialog } from "../components/listenerSettingsDialog";
import { ProtocolSettingsDialog } from "../components/protocolSettingsDialog";
import { SslSettingsDialog } from "../components/sslSettingsDialog";
import { ConfirmDialog } from "../components/confirmDialog";
import {
  TransactionRepeatActions,
  type RepeatDialogMode,
} from "../components/transactionRepeatActions";
import {
  BreakpointHitDialog,
  ToolSettingsDialog,
} from "../components/toolSettingsDialog";
import {
  readListenerDialogId,
  readToolDialogId,
  readTransactionSeed,
  showIndependentWindow,
} from "../platform/independentWindowContract";
import { closeCurrentManagedWindow } from "../platform/managedWindow";
import { publishIndependentWindowResult } from "../platform/independentWindowEvents";
import { useServiceStore } from "../state/serviceStore";
import { SettingsPage } from "./settingsPage";
import { ProcessManagerPage } from "./processManagerPage";

/** 请求关闭当前受管窗口；按钮处理器不吞掉原生失败，开发控制台会保留完整异常。 */
function closeWindow(): void {
  void closeCurrentManagedWindow();
}

/**
 * 提供独立窗口的固定视口和正文网格；具体编辑器沿用原有业务布局，窗口切换时不会改变主窗口尺寸。
 */
function WindowSurface({ children }: { children: ReactNode }) {
  return (
    <div className="windowSurface independentWindowSurface">
      <div className="windowSurfaceContent independentWindowContent">
        {children}
      </div>
    </div>
  );
}

interface WindowFeedbackProps {
  title: string;
  message: string;
  role: "alert" | "status";
}

/**
 * 渲染独立窗口的稳定结果页；标题、正文和底栏与编辑型窗口共享三轨结构。
 * 参数：role 区分错误和成功状态的辅助技术播报语义；关闭动作始终销毁当前受管窗口。
 */
function WindowFeedback({ title, message, role }: WindowFeedbackProps) {
  return (
    <WindowSurface>
      <main className="independentWindowFeedback" role={role}>
        <header className="independentWindowFeedbackHeader">
          <h1>{title}</h1>
        </header>
        <div className="independentWindowFeedbackBody">
          <span aria-hidden="true" className={`feedbackStateDot is-${role}`} />
          <p>{message}</p>
        </div>
        <footer className="independentWindowFeedbackActions">
          <button type="button" onClick={closeWindow}>
            关闭
          </button>
        </footer>
      </main>
    </WindowSurface>
  );
}

/** 渲染无效独立窗口参数的明确失败状态，阻止未知参数进入配置写入组件。 */
function InvalidWindowRequest({ message }: { message: string }) {
  return <WindowFeedback message={message} role="alert" title="窗口参数无效" />;
}

/**
 * 承载断点队列，并在最后一个挂起事务处理完毕后关闭自身。
 * 打开设置会创建独立工具窗口，断点编辑器继续保留当前草稿和队列选择。
 */
function BreakpointHitWindow() {
  const { t } = useTranslation();
  const { snapshot, suspendedBreakpoints } = useServiceStore();
  const observedBreakpoint = useRef(false);

  useEffect(() => {
    if (suspendedBreakpoints.length > 0) {
      observedBreakpoint.current = true;
      return;
    }
    if (observedBreakpoint.current) {
      closeWindow();
    }
  }, [suspendedBreakpoints.length]);

  if (snapshot === null) {
    return (
      <WindowFeedback
        message={t("app.control.connecting")}
        role="status"
        title={t("tools.breakpoints.hitTitle")}
      />
    );
  }
  if (suspendedBreakpoints.length === 0) {
    return (
      <WindowFeedback
        message={t("tools.breakpoints.emptyMessage")}
        role="status"
        title={t("tools.breakpoints.emptyTitle")}
      />
    );
  }

  return (
    <WindowSurface>
      <BreakpointHitDialog
        onOpenToolSettings={() =>
          void showIndependentWindow({ kind: "tool", tool: "breakpoints" })
        }
      />
    </WindowSurface>
  );
}

/** 执行在线响应校验；确认后显示真实报告摘要，用户关闭前可明确看到成功或失败结果。 */
function OnlineValidationWindow({ transactionId }: { transactionId: string }) {
  const { t } = useTranslation();
  const { validateResponse } = useServiceStore();
  const [pending, setPending] = useState(false);
  const [issueCount, setIssueCount] = useState<number | null>(null);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);

  /** 确认后调用在线校验端点；失败保留当前窗口，成功发布刷新事件并展示问题数。 */
  const confirmValidation = async () => {
    setPending(true);
    setErrorMessage(null);
    try {
      const report = await validateResponse(transactionId, {
        validatorId: "w3cHtmlOnline",
        onlineUploadConfirmed: true,
      });
      setIssueCount(report.issues.length);
      publishIndependentWindowResult({
        kind: "onlineValidation",
        transactionId,
      });
    } catch (error) {
      setErrorMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setPending(false);
    }
  };

  if (issueCount !== null) {
    return (
      <WindowFeedback
        message={
          issueCount === 0
            ? t("viewer.protocol.validate.noIssues")
            : `${issueCount} 个问题`
        }
        role="status"
        title={t("viewer.protocol.online.title")}
      />
    );
  }
  return (
    <WindowSurface>
      <ConfirmDialog
        busy={pending}
        cancelLabel={t("viewer.protocol.online.cancel")}
        confirmLabel={t("viewer.protocol.online.confirm")}
        message={errorMessage ?? t("viewer.protocol.online.message")}
        open
        title={t("viewer.protocol.online.title")}
        onCancel={closeWindow}
        onConfirm={() => void confirmValidation()}
      />
    </WindowSurface>
  );
}

/** 执行插件卸载确认；只有后端确认删除后才广播刷新并关闭窗口。 */
function PluginUninstallWindow({
  pluginId,
  pluginName,
}: {
  pluginId: string;
  pluginName: string;
}) {
  const { t } = useTranslation();
  const { actionPending, lastError, uninstallPlugin } = useServiceStore();

  /** 调用真实卸载命令；活动连接冲突或宿主拒绝时保持窗口供用户查看错误并重试。 */
  const confirmUninstall = async () => {
    const succeeded = await uninstallPlugin(pluginId);
    if (!succeeded) {
      return;
    }
    publishIndependentWindowResult({ kind: "pluginUninstall", pluginId });
    closeWindow();
  };

  return (
    <WindowSurface>
      <ConfirmDialog
        busy={actionPending}
        cancelLabel={t("tools.cancel")}
        confirmLabel={t("plugins.uninstallConfirm")}
        message={lastError ?? t("plugins.uninstallDescription")}
        open
        title={t("plugins.uninstallTitle", { name: pluginName })}
        onCancel={closeWindow}
        onConfirm={() => void confirmUninstall()}
      />
    </WindowSurface>
  );
}

const skipClearConfirmationStorageKey =
  "capture.recording.skipClearConfirmation";

/** 执行录制清空确认，并在用户明确勾选后持久化下次免确认偏好。 */
function ClearRecordingWindow({
  transactionCount,
}: {
  transactionCount: number;
}) {
  const { t } = useTranslation();
  const { activeAction, clearRecording, lastError, snapshot } =
    useServiceStore();
  const [skipFutureConfirmation, setSkipFutureConfirmation] = useState(false);
  const [submitted, setSubmitted] = useState(false);
  const busy = activeAction === "recordingClear";

  /** 只在权威快照确认事务数归零后关闭；失败时保留错误和重试入口。 */
  useEffect(() => {
    if (!submitted || busy) {
      return;
    }
    if (snapshot?.recording.transactionCount === 0) {
      publishIndependentWindowResult({ kind: "clearRecording" });
      closeWindow();
      return;
    }
    if (lastError !== null) {
      setSubmitted(false);
    }
  }, [busy, lastError, snapshot?.recording.transactionCount, submitted]);

  /** 原子清空事务；偏好写入与清空处于同一确认动作，取消不会改变现有设置。 */
  const confirmClear = async () => {
    if (skipFutureConfirmation) {
      window.localStorage.setItem(skipClearConfirmationStorageKey, "true");
    }
    setSubmitted(true);
    await clearRecording();
  };

  return (
    <WindowSurface>
      <ConfirmDialog
        busy={busy || submitted}
        cancelLabel={t("recording.clearCancel")}
        confirmLabel={
          busy ? t("recording.clearing") : t("recording.clearConfirm")
        }
        message={
          lastError ?? t("recording.clearMessage", { count: transactionCount })
        }
        open
        option={{
          checked: skipFutureConfirmation,
          label: t("recording.clearDoNotAskAgain"),
          onCheckedChange: setSkipFutureConfirmation,
        }}
        title={t("recording.clearTitle")}
        onCancel={closeWindow}
        onConfirm={() => void confirmClear()}
      />
    </WindowSurface>
  );
}

/**
 * 渲染设置独立窗口；区域切换保留在同一窗口路由中，返回按钮关闭窗口而不改写主窗口地址。
 */
export function IndependentSettingsWindowPage() {
  return (
    <WindowSurface>
      <div className="independentWindowShell independentWindowShell--settings">
        <SettingsPage routeBase="/window/settings" onClose={closeWindow} />
      </div>
    </WindowSurface>
  );
}

/**
 * 按路由 kind 挂载现有完整业务编辑器；查询参数只恢复字段化种子，不复制正文或服务快照。
 */
export function IndependentDialogWindowPage() {
  const { dialogKind } = useParams<{ dialogKind?: string }>();
  const location = useLocation();
  const parameters = new URLSearchParams(location.search);
  const seed = readTransactionSeed(parameters);

  if (dialogKind === "ssl") {
    return (
      <WindowSurface>
        <SslSettingsDialog
          focusClientCertificate={
            parameters.get("focus") === "clientCertificate"
          }
          initialLocation={seed?.location ?? null}
          open
          onClose={closeWindow}
        />
      </WindowSurface>
    );
  }
  if (dialogKind === "processes") {
    return (
      <WindowSurface>
        <ProcessManagerPage />
      </WindowSurface>
    );
  }
  if (dialogKind === "protocol") {
    return (
      <WindowSurface>
        <ProtocolSettingsDialog open onClose={closeWindow} />
      </WindowSurface>
    );
  }
  if (dialogKind === "listener") {
    const listener = readListenerDialogId(parameters.get("listener"));
    return listener === null ? (
      <InvalidWindowRequest message="缺少有效的监听器类型。" />
    ) : (
      <WindowSurface>
        <ListenerSettingsDialog open={listener} onClose={closeWindow} />
      </WindowSurface>
    );
  }
  if (dialogKind === "tool") {
    const tool = readToolDialogId(parameters.get("tool"));
    return tool === null ? (
      <InvalidWindowRequest message="缺少有效的工具类型。" />
    ) : (
      <WindowSurface>
        <ToolSettingsDialog
          initialSeed={seed}
          open={tool}
          onClose={closeWindow}
        />
      </WindowSurface>
    );
  }
  if (dialogKind === "breakpoint-hit") {
    return <BreakpointHitWindow />;
  }
  if (dialogKind === "online-validation") {
    const transactionId = parameters.get("transactionId");
    const validatorId = parameters.get("validatorId");
    return transactionId === null || validatorId !== "w3cHtmlOnline" ? (
      <InvalidWindowRequest message="缺少有效的在线校验事务或校验器。" />
    ) : (
      <OnlineValidationWindow transactionId={transactionId} />
    );
  }
  if (dialogKind === "plugin-uninstall") {
    const pluginId = parameters.get("pluginId");
    const pluginName = parameters.get("pluginName");
    return pluginId === null || pluginName === null ? (
      <InvalidWindowRequest message="缺少有效的插件卸载参数。" />
    ) : (
      <PluginUninstallWindow pluginId={pluginId} pluginName={pluginName} />
    );
  }
  if (dialogKind === "repeat") {
    const transactionId = parameters.get("transactionId");
    const modeValue = parameters.get("mode");
    const mode: RepeatDialogMode | null =
      modeValue === "edit" || modeValue === "advanced" ? modeValue : null;
    return transactionId === null || mode === null ? (
      <InvalidWindowRequest message="缺少有效的重复事务或编辑模式。" />
    ) : (
      <WindowSurface>
        <TransactionRepeatActions
          transaction={{ transactionId }}
          windowMode={mode}
          onWindowClose={closeWindow}
        />
      </WindowSurface>
    );
  }
  if (dialogKind === "clear-recording") {
    const transactionCount = Number(parameters.get("transactionCount"));
    return !Number.isSafeInteger(transactionCount) || transactionCount < 0 ? (
      <InvalidWindowRequest message="缺少有效的事务数量。" />
    ) : (
      <ClearRecordingWindow transactionCount={transactionCount} />
    );
  }
  return <InvalidWindowRequest message="请求的独立窗口类型不存在。" />;
}
