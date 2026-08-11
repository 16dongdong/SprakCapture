import {
  Copy,
  Plus,
  RotateCw,
  Send,
  SlidersHorizontal,
  Trash2,
  X,
} from "lucide-react";
import {
  type ChangeEvent,
  type FormEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import type {
  AdvancedRepeatJob,
  ComposeRequest,
  ComposeRequestOverrides,
  HeaderField,
  TransactionDetail,
  TransactionSummary,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";
import { showIndependentWindow } from "../platform/independentWindowContract";
import { useModalFocus } from "./modalFocus";

export type RepeatDialogMode = "edit" | "advanced";

interface RequestDraft {
  method: string;
  url: string;
  headers: HeaderField[];
  bodyBase64: string;
  viaProxy: boolean;
}

interface TransactionRepeatActionsProps {
  transaction: Pick<TransactionSummary, "transactionId">;
  windowMode?: RepeatDialogMode;
  onWindowClose?(): void;
}

/** 将 Base64 正文解码为可编辑文本；解码失败时保留空文本并强制用户切换二进制模式。 */
function decodeBodyText(bodyBase64: string): string {
  try {
    const binary = window.atob(bodyBase64);
    const bytes = Uint8Array.from(binary, (character) =>
      character.charCodeAt(0),
    );
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return "";
  }
}

/** 将文本编辑器内容转换回 Base64；TextEncoder 保证多字节字符与控制协议使用相同 UTF-8 字节序列。 */
function encodeBodyText(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return window.btoa(binary);
}

/** 从事务详情和请求正文构造独立草稿；原始事务从不被编辑器写回。 */
function buildDraft(
  detail: TransactionDetail,
  bodyBase64: string,
): RequestDraft {
  return {
    method: detail.transaction.method,
    url: detail.transaction.urlDisplay,
    headers: detail.requestHeaders.map((header) => ({ ...header })),
    bodyBase64,
    viaProxy: true,
  };
}

/** 将完整草稿转换为覆盖字段；每个字段均显式传入，避免编辑结果意外继承已改变的事务视图。 */
function draftOverrides(draft: RequestDraft): ComposeRequestOverrides {
  return {
    method: draft.method,
    url: draft.url,
    headers: draft.headers,
    bodyBase64: draft.bodyBase64,
    viaProxy: draft.viaProxy,
  };
}

/** 渲染事务重复、编辑重复与高级重复入口，并管理两个可访问的局部对话框。 */
export function TransactionRepeatActions({
  transaction,
  windowMode,
  onWindowClose,
}: TransactionRepeatActionsProps) {
  const { t } = useTranslation();
  const {
    actionPending,
    activeAction,
    cancelAdvancedRepeat,
    getTransactionBody,
    getTransactionDetail,
    repeatTransaction,
    snapshot,
    startAdvancedRepeat,
  } = useServiceStore();
  const [mode, setMode] = useState<RepeatDialogMode | null>(windowMode ?? null);
  const [draft, setDraft] = useState<RequestDraft | null>(null);
  const [bodyText, setBodyText] = useState("");
  const [binaryBody, setBinaryBody] = useState(false);
  const [loadingDraft, setLoadingDraft] = useState(false);
  const [draftUnavailable, setDraftUnavailable] = useState(false);
  const [iterations, setIterations] = useState(10);
  const [concurrency, setConcurrency] = useState(1);
  const [intervalMilliseconds, setIntervalMilliseconds] = useState(0);
  const [recordEach, setRecordEach] = useState(true);
  const [stopOnError, setStopOnError] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [job, setJob] = useState<AdvancedRepeatJob | null>(null);
  const dialogRef = useRef<HTMLElement>(null);
  const modalFocus = useModalFocus({
    open: mode !== null,
    containerRef: dialogRef,
  });
  const busy = actionPending && activeAction === "repeat";
  // 启动响应负责让作业立即可见；后续状态以 SSE 推送的有界权威集合为准。
  const currentJob =
    job === null
      ? null
      : (snapshot?.advancedRepeats.find(
          (candidate) => candidate.jobId === job.jobId,
        ) ?? job);

  /** 关闭对话框并清理短生命周期草稿，防止后续事务复用上一条请求正文。 */
  const closeDialog = useCallback(() => {
    if (busy) {
      return;
    }
    if (onWindowClose !== undefined) {
      onWindowClose();
    } else {
      setMode(null);
    }
    setDraft(null);
    setJob(null);
    setDraftUnavailable(false);
    setConfirmed(false);
  }, [busy, onWindowClose]);

  /** 加载编辑或高级重复所需的完整请求；AbortController 确保切换选中事务时不会提交迟到结果。 */
  useEffect(() => {
    if (mode === null) {
      return undefined;
    }
    const abortController = new AbortController();
    setLoadingDraft(true);
    setDraftUnavailable(false);
    setDraft(null);
    void Promise.all([
      getTransactionDetail(transaction.transactionId, abortController.signal),
      getTransactionBody(
        transaction.transactionId,
        "request",
        abortController.signal,
      ),
    ])
      .then(([detail, body]) => {
        if (abortController.signal.aborted) {
          return;
        }
        setDraft(buildDraft(detail, body.base64));
        setBodyText(decodeBodyText(body.base64));
        setBinaryBody(false);
      })
      .catch(() => {
        if (!abortController.signal.aborted) {
          setDraftUnavailable(true);
        }
      })
      .finally(() => {
        if (!abortController.signal.aborted) {
          setLoadingDraft(false);
        }
      });
    return () => abortController.abort();
  }, [
    getTransactionBody,
    getTransactionDetail,
    mode,
    transaction.transactionId,
  ]);

  /** 一键重复不打开对话框；只派生新事务，原始选中项不受任何覆盖操作影响。 */
  const repeatNow = useCallback(() => {
    void repeatTransaction(transaction.transactionId);
  }, [repeatTransaction, transaction.transactionId]);

  /** 更新草稿字段，所有编辑均发生在独立对象中。 */
  const updateDraft = useCallback((update: Partial<RequestDraft>) => {
    setDraft((current) =>
      current === null ? current : { ...current, ...update },
    );
  }, []);

  /** 提交编辑后的重复请求；提交成功才关闭对话框，从而保留失败时的用户输入。 */
  const submitEdit = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      if (draft === null) {
        return;
      }
      const requestBody = binaryBody
        ? draft.bodyBase64
        : encodeBodyText(bodyText);
      void repeatTransaction(transaction.transactionId, {
        ...draftOverrides({ ...draft, bodyBase64: requestBody }),
      }).then((result) => {
        if (result !== null) {
          closeDialog();
        }
      });
    },
    [
      binaryBody,
      bodyText,
      closeDialog,
      draft,
      repeatTransaction,
      transaction.transactionId,
    ],
  );

  /** 启动已明确确认的高级重复任务；recordEach=false 将直接委托后端关闭事务写入。 */
  const submitAdvanced = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      if (draft === null || !confirmed) {
        return;
      }
      const bodyBase64 = binaryBody
        ? draft.bodyBase64
        : encodeBodyText(bodyText);
      const base: ComposeRequest = { ...draft, bodyBase64 };
      void startAdvancedRepeat({
        name: `${draft.method} ${draft.url}`.slice(0, 128),
        base,
        concurrency,
        totalIterations: iterations,
        intervalMilliseconds,
        recordEach,
        stopOnError,
        confirmed: true,
      }).then((result) => {
        if (result !== null) {
          setJob(result);
        }
      });
    },
    [
      binaryBody,
      bodyText,
      concurrency,
      confirmed,
      draft,
      intervalMilliseconds,
      iterations,
      recordEach,
      startAdvancedRepeat,
      stopOnError,
    ],
  );

  /** 请求取消当前作业；命令响应立即更新界面，随后由实时事件收敛所有已启动迭代的终态。 */
  const cancelJob = useCallback(() => {
    if (currentJob === null) {
      return;
    }
    void cancelAdvancedRepeat(currentJob.jobId).then((result) => {
      if (result !== null) {
        setJob(result);
      }
    });
  }, [cancelAdvancedRepeat, currentJob]);

  return (
    <>
      {windowMode === undefined && <div className="transactionRepeatActions">
        <button
          aria-label={t("repeat.repeat")}
          disabled={busy}
          title={t("repeat.repeat")}
          type="button"
          onClick={repeatNow}
        >
          <RotateCw aria-hidden="true" size={15} />
        </button>
        <button
          aria-label={t("repeat.editAndRepeat")}
          disabled={busy}
          title={t("repeat.editAndRepeat")}
          type="button"
          onClick={() =>
            void showIndependentWindow({
              kind: "repeat",
              transactionId: transaction.transactionId,
              mode: "edit",
            })
          }
        >
          <Copy aria-hidden="true" size={15} />
        </button>
        <button
          aria-label={t("repeat.advanced")}
          disabled={busy}
          title={t("repeat.advanced")}
          type="button"
          onClick={() =>
            void showIndependentWindow({
              kind: "repeat",
              transactionId: transaction.transactionId,
              mode: "advanced",
            })
          }
        >
          <SlidersHorizontal aria-hidden="true" size={15} />
        </button>
      </div>}
      {mode !== null && (
        <div
          className="dialogBackdrop"
          role="presentation"
          onMouseDown={closeDialog}
        >
          <section
            aria-modal="true"
            className="repeatDialog"
            ref={dialogRef}
            role="dialog"
            tabIndex={-1}
            onKeyDown={modalFocus.onKeyDown}
            onMouseDown={(event) => event.stopPropagation()}
          >
            <header>
              <div>
                <h2>
                  {t(
                    mode === "edit"
                      ? "repeat.editTitle"
                      : "repeat.advancedTitle",
                  )}
                </h2>
                <p>
                  {t(
                    mode === "edit" ? "repeat.editHint" : "repeat.advancedHint",
                  )}
                </p>
              </div>
              <button
                aria-label={t("repeat.close")}
                disabled={busy}
                type="button"
                onClick={closeDialog}
              >
                <X aria-hidden="true" size={18} />
              </button>
            </header>
            {loadingDraft && (
              <p className="repeatDialogState">{t("repeat.loading")}</p>
            )}
            {draftUnavailable && (
              <p className="repeatDialogState repeatDialogState--error">
                {t("repeat.bodyUnavailable")}
              </p>
            )}
            {draft !== null && currentJob === null && (
              <form onSubmit={mode === "edit" ? submitEdit : submitAdvanced}>
                <RequestEditor
                  binaryBody={binaryBody}
                  bodyText={bodyText}
                  disabled={busy}
                  draft={draft}
                  onBinaryBodyChange={setBinaryBody}
                  onBodyTextChange={setBodyText}
                  onDraftChange={updateDraft}
                />
                {mode === "advanced" && (
                  <AdvancedFields
                    concurrency={concurrency}
                    confirmed={confirmed}
                    disabled={busy}
                    intervalMilliseconds={intervalMilliseconds}
                    iterations={iterations}
                    recordEach={recordEach}
                    stopOnError={stopOnError}
                    onConcurrencyChange={setConcurrency}
                    onConfirmedChange={setConfirmed}
                    onIntervalChange={setIntervalMilliseconds}
                    onIterationsChange={setIterations}
                    onRecordEachChange={setRecordEach}
                    onStopOnErrorChange={setStopOnError}
                  />
                )}
                <footer>
                  <button disabled={busy} type="button" onClick={closeDialog}>
                    {t("repeat.cancel")}
                  </button>
                  <button
                    className="primaryButton"
                    disabled={busy || (mode === "advanced" && !confirmed)}
                    type="submit"
                  >
                    <Send aria-hidden="true" size={15} />
                    {busy
                      ? t("repeat.sending")
                      : t(mode === "edit" ? "repeat.send" : "repeat.start")}
                  </button>
                </footer>
              </form>
            )}
            {currentJob !== null && (
              <JobProgress job={currentJob} busy={busy} onCancel={cancelJob} />
            )}
          </section>
        </div>
      )}
    </>
  );
}

/** 渲染结构化请求编辑器，头字段使用可增删的名称/值行，不暴露 JSON 配置入口。 */
function RequestEditor({
  binaryBody,
  bodyText,
  disabled,
  draft,
  onBinaryBodyChange,
  onBodyTextChange,
  onDraftChange,
}: {
  binaryBody: boolean;
  bodyText: string;
  disabled: boolean;
  draft: RequestDraft;
  onBinaryBodyChange(value: boolean): void;
  onBodyTextChange(value: string): void;
  onDraftChange(update: Partial<RequestDraft>): void;
}) {
  const { t } = useTranslation();
  const updateHeader = (
    index: number,
    key: keyof HeaderField,
    value: string,
  ) => {
    const headers = draft.headers.map((header, headerIndex) =>
      headerIndex === index ? { ...header, [key]: value } : header,
    );
    onDraftChange({ headers });
  };
  return (
    <div className="repeatEditorFields">
      <div className="repeatRequestGrid">
        <label>
          <span>{t("repeat.method")}</span>
          <input
            required
            disabled={disabled}
            value={draft.method}
            onChange={(event) =>
              onDraftChange({ method: event.target.value.toUpperCase() })
            }
          />
        </label>
        <label className="repeatUrlField">
          <span>{t("repeat.url")}</span>
          <input
            required
            disabled={disabled}
            type="url"
            value={draft.url}
            onChange={(event) => onDraftChange({ url: event.target.value })}
          />
        </label>
      </div>
      <label className="toolEnabledRow">
        <input
          checked={draft.viaProxy}
          disabled={disabled}
          type="checkbox"
          onChange={(event) =>
            onDraftChange({ viaProxy: event.target.checked })
          }
        />
        <span>{t("repeat.viaProxy")}</span>
      </label>
      <div className="repeatHeaders">
        <div className="repeatSectionHeading">
          <span>{t("repeat.headers")}</span>
          <button
            disabled={disabled}
            type="button"
            onClick={() =>
              onDraftChange({
                headers: [...draft.headers, { name: "", value: "" }],
              })
            }
          >
            <Plus aria-hidden="true" size={14} />
            {t("repeat.addHeader")}
          </button>
        </div>
        {draft.headers.map((header, index) => (
          <div className="repeatHeaderRow" key={`${index}:${header.name}`}>
            <input
              aria-label={t("repeat.headerName")}
              disabled={disabled}
              value={header.name}
              onChange={(event) =>
                updateHeader(index, "name", event.target.value)
              }
            />
            <input
              aria-label={t("repeat.headerValue")}
              disabled={disabled}
              value={header.value}
              onChange={(event) =>
                updateHeader(index, "value", event.target.value)
              }
            />
            <button
              aria-label={t("repeat.removeHeader")}
              disabled={disabled}
              type="button"
              onClick={() =>
                onDraftChange({
                  headers: draft.headers.filter(
                    (_, headerIndex) => headerIndex !== index,
                  ),
                })
              }
            >
              <Trash2 aria-hidden="true" size={14} />
            </button>
          </div>
        ))}
      </div>
      <label className="toolEnabledRow">
        <input
          checked={binaryBody}
          disabled={disabled}
          type="checkbox"
          onChange={(event) => onBinaryBodyChange(event.target.checked)}
        />
        <span>{t("repeat.binaryBody")}</span>
      </label>
      <label>
        <span>
          {binaryBody ? t("repeat.bodyBase64") : t("repeat.bodyText")}
        </span>
        <textarea
          disabled={disabled}
          rows={8}
          value={binaryBody ? draft.bodyBase64 : bodyText}
          onChange={(event: ChangeEvent<HTMLTextAreaElement>) =>
            binaryBody
              ? onDraftChange({ bodyBase64: event.target.value })
              : onBodyTextChange(event.target.value)
          }
        />
      </label>
    </div>
  );
}

/** 渲染高级重复的有界调度参数和显式确认，不将压测参数写入全局服务设置。 */
function AdvancedFields({
  concurrency,
  confirmed,
  disabled,
  intervalMilliseconds,
  iterations,
  recordEach,
  stopOnError,
  onConcurrencyChange,
  onConfirmedChange,
  onIntervalChange,
  onIterationsChange,
  onRecordEachChange,
  onStopOnErrorChange,
}: {
  concurrency: number;
  confirmed: boolean;
  disabled: boolean;
  intervalMilliseconds: number;
  iterations: number;
  recordEach: boolean;
  stopOnError: boolean;
  onConcurrencyChange(value: number): void;
  onConfirmedChange(value: boolean): void;
  onIntervalChange(value: number): void;
  onIterationsChange(value: number): void;
  onRecordEachChange(value: boolean): void;
  onStopOnErrorChange(value: boolean): void;
}) {
  const { t } = useTranslation();
  const readNumber =
    (setter: (value: number) => void, minimum: number, maximum: number) =>
    (event: ChangeEvent<HTMLInputElement>) => {
      const parsed = Number(event.target.value);
      setter(
        Number.isInteger(parsed)
          ? Math.min(maximum, Math.max(minimum, parsed))
          : minimum,
      );
    };
  return (
    <fieldset className="repeatAdvancedFields">
      <legend>{t("repeat.parameters")}</legend>
      <div className="repeatRequestGrid">
        <label>
          <span>{t("repeat.iterations")}</span>
          <input
            disabled={disabled}
            max={10_000}
            min={1}
            type="number"
            value={iterations}
            onChange={readNumber(onIterationsChange, 1, 10_000)}
          />
        </label>
        <label>
          <span>{t("repeat.concurrency")}</span>
          <input
            disabled={disabled}
            max={256}
            min={1}
            type="number"
            value={concurrency}
            onChange={readNumber(onConcurrencyChange, 1, 256)}
          />
        </label>
        <label>
          <span>{t("repeat.interval")}</span>
          <input
            disabled={disabled}
            max={60_000}
            min={0}
            type="number"
            value={intervalMilliseconds}
            onChange={readNumber(onIntervalChange, 0, 60_000)}
          />
        </label>
      </div>
      <label className="toolEnabledRow">
        <input
          checked={recordEach}
          disabled={disabled}
          type="checkbox"
          onChange={(event) => onRecordEachChange(event.target.checked)}
        />
        <span>{t("repeat.recordEach")}</span>
      </label>
      <label className="toolEnabledRow">
        <input
          checked={stopOnError}
          disabled={disabled}
          type="checkbox"
          onChange={(event) => onStopOnErrorChange(event.target.checked)}
        />
        <span>{t("repeat.stopOnError")}</span>
      </label>
      <label className="toolEnabledRow repeatConfirm">
        <input
          checked={confirmed}
          disabled={disabled}
          type="checkbox"
          onChange={(event) => onConfirmedChange(event.target.checked)}
        />
        <span>{t("repeat.confirm")}</span>
      </label>
    </fieldset>
  );
}

/** 显示高级重复进度、延迟统计与取消按钮；只有队列或运行状态允许再次发送取消。 */
function JobProgress({
  job,
  busy,
  onCancel,
}: {
  job: AdvancedRepeatJob;
  busy: boolean;
  onCancel(): void;
}) {
  const { t } = useTranslation();
  const percentage =
    job.plan.totalIterations === 0
      ? 0
      : Math.min(
          100,
          Math.round(
            (job.completedIterations / job.plan.totalIterations) * 100,
          ),
        );
  const running = job.state === "queued" || job.state === "running";
  return (
    <div className="repeatJobProgress">
      <strong>{t(`repeat.states.${job.state}`)}</strong>
      <progress
        max={job.plan.totalIterations}
        value={job.completedIterations}
      />
      <span>
        {t("repeat.progress", {
          completed: job.completedIterations,
          total: job.plan.totalIterations,
          percentage,
        })}
      </span>
      <dl>
        <div>
          <dt>{t("repeat.success")}</dt>
          <dd>{job.successCount}</dd>
        </div>
        <div>
          <dt>{t("repeat.failure")}</dt>
          <dd>{job.failureCount}</dd>
        </div>
        <div>
          <dt>{t("repeat.p95")}</dt>
          <dd>{job.latencyMilliseconds.p95} ms</dd>
        </div>
      </dl>
      {job.lastError !== null && (
        <p className="repeatDialogState repeatDialogState--error">
          {t("repeat.lastError", { code: job.lastError })}
        </p>
      )}
      <footer>
        <button disabled={busy || !running} type="button" onClick={onCancel}>
          {t("repeat.cancelJob")}
        </button>
      </footer>
    </div>
  );
}
