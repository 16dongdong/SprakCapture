import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Window as TauriWindow } from "@tauri-apps/api/window";
import { type KeyboardEvent, useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

const mainWindowCloseRequestedEvent = "desktop://main-window-close-requested";

/** 隔离 Tauri 事件和命令，使关闭询问可以在浏览器测试中验证完整交互。 */
export interface MainWindowClosePlatform {
  isMainDesktopWindow(): boolean;
  listenForCloseRequest(listener: () => void): Promise<UnlistenFn>;
  hasPendingCloseRequest(): Promise<boolean>;
  cancelCloseRequest(): Promise<void>;
  resolveCloseRequest(enterTray: boolean, remember: boolean): Promise<void>;
}

const defaultPlatform: MainWindowClosePlatform = {
  isMainDesktopWindow: () => isTauri() && TauriWindow.getCurrent().label === "main",
  listenForCloseRequest: async (listener) => listen(mainWindowCloseRequestedEvent, listener),
  hasPendingCloseRequest: () => invoke<boolean>("pendingMainWindowClosePrompt"),
  cancelCloseRequest: () => invoke("cancelMainWindowClose"),
  resolveCloseRequest: (enterTray, remember) =>
    invoke("resolveMainWindowClose", { enterTray, remember }),
};

/**
 * 渲染主窗口首次关闭询问，只呈现“进入托盘”这一项是非判断，避免把简单决定扩展成设置表单。
 * 命令失败时保留弹框和记住状态，用户可根据原始诊断直接重试。
 */
export function MainWindowCloseDialog({
  platform = defaultPlatform,
}: {
  platform?: MainWindowClosePlatform;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [remember, setRemember] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const yesButtonRef = useRef<HTMLButtonElement>(null);
  const titleId = useId();
  const questionId = useId();

  /** 为每次原生关闭请求建立干净草稿；重复事件不会增加第二层弹框。 */
  const showPrompt = useCallback(() => {
    setRemember(false);
    setErrorMessage(null);
    setOpen(true);
  }, []);

  useEffect(() => {
    if (!platform.isMainDesktopWindow()) {
      return undefined;
    }
    let disposed = false;
    let removeListener: UnlistenFn | undefined;

    /** 先注册事件再查询原生状态，覆盖 React 挂载前后发生关闭请求的竞态。 */
    const connectClosePrompt = async () => {
      try {
        const registeredListener = await platform.listenForCloseRequest(showPrompt);
        if (disposed) {
          registeredListener();
          return;
        }
        removeListener = registeredListener;
        if (await platform.hasPendingCloseRequest()) {
          showPrompt();
        }
      } catch (error) {
        if (!disposed) {
          setErrorMessage(formatCloseError(error));
        }
      }
    };

    void connectClosePrompt();
    return () => {
      disposed = true;
      removeListener?.();
    };
  }, [platform, showPrompt]);

  useEffect(() => {
    if (open) {
      yesButtonRef.current?.focus();
    }
  }, [open]);

  /** Escape 只取消当前关闭请求并保留主窗口，不会替用户选择“否”。 */
  const cancelPrompt = async () => {
    if (submitting) {
      return;
    }
    setSubmitting(true);
    setErrorMessage(null);
    try {
      await platform.cancelCloseRequest();
      setOpen(false);
    } catch (error) {
      setErrorMessage(formatCloseError(error));
    } finally {
      setSubmitting(false);
    }
  };

  /** 提交是非结果；是进入托盘，否停止服务并退出整个桌面进程。 */
  const resolvePrompt = async (enterTray: boolean) => {
    if (submitting) {
      return;
    }
    setSubmitting(true);
    setErrorMessage(null);
    try {
      await platform.resolveCloseRequest(enterTray, remember);
      setOpen(false);
    } catch (error) {
      setErrorMessage(formatCloseError(error));
    } finally {
      setSubmitting(false);
    }
  };

  if (!open) {
    return null;
  }

  /** 键盘取消与原生弹框一致；处理期间锁定输入，避免同时提交两种结果。 */
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape" && !submitting) {
      event.preventDefault();
      void cancelPrompt();
    }
  };

  return (
    <div className="mainCloseBackdrop" role="presentation" onKeyDown={handleKeyDown}>
      <section
        aria-busy={submitting}
        aria-describedby={questionId}
        aria-labelledby={titleId}
        aria-modal="true"
        className="mainCloseDialog"
        role="dialog"
      >
        <h2 id={titleId}>{t("desktopClose.title")}</h2>
        <p id={questionId}>{t("desktopClose.question")}</p>

        <label className="mainCloseRemember">
          <input
            checked={remember}
            disabled={submitting}
            type="checkbox"
            onChange={(event) => setRemember(event.target.checked)}
          />
          <span>{t("desktopClose.remember")}</span>
        </label>

        {errorMessage !== null && (
          <p className="mainCloseError" role="alert">
            {errorMessage}
          </p>
        )}

        <footer className="mainCloseActions">
          <button
            className="mainCloseYes"
            disabled={submitting}
            ref={yesButtonRef}
            type="button"
            onClick={() => void resolvePrompt(true)}
          >
            {submitting ? t("desktopClose.processing") : t("desktopClose.yes")}
          </button>
          <button
            disabled={submitting}
            type="button"
            onClick={() => void resolvePrompt(false)}
          >
            {t("desktopClose.no")}
          </button>
        </footer>
      </section>
    </div>
  );
}

/** 将桌面、文件系统或窗口错误规范为可读文本，未知值不伪造成成功原因。 */
function formatCloseError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
