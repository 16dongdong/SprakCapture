import {
  type KeyboardEvent,
  useEffect,
  useId,
  useRef,
} from "react";


interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message: string;
  cancelLabel: string;
  confirmLabel: string;
  busy?: boolean;
  option?: {
    checked: boolean;
    label: string;
    onCheckedChange(checked: boolean): void;
  };
  onCancel(): void;
  onConfirm(): void;
}

/**
 * 渲染可复用确认对话框；打开后默认聚焦取消按钮，避免破坏性操作被回车意外确认。
 * 参数：option 为与本次确认绑定的可选布尔偏好；取消时不会由对话框自行持久化该值。
 * 失败语义：组件不吞掉回调异常，业务动作和持久化结果由调用方统一负责。
 */
export function ConfirmDialog({
  open,
  title,
  message,
  cancelLabel,
  confirmLabel,
  busy = false,
  option,
  onCancel,
  onConfirm,
}: ConfirmDialogProps) {
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  const confirmButtonRef = useRef<HTMLButtonElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const titleId = useId();
  const messageId = useId();

  useEffect(() => {
    if (!open) {
      return undefined;
    }
    restoreFocusRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    cancelButtonRef.current?.focus();
    return () => {
      restoreFocusRef.current?.focus();
      restoreFocusRef.current = null;
    };
  }, [open]);

  if (!open) {
    return null;
  }

  /**
   * 支持 Escape 关闭；忙碌期间保持对话框稳定，等待唯一清空请求结束。
   */
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape" && !busy) {
      event.preventDefault();
      onCancel();
      return;
    }
    if (event.key === "Tab" && busy) {
      event.preventDefault();
      return;
    }
    if (event.key !== "Tab") {
      return;
    }
    if (
      event.shiftKey &&
      document.activeElement === cancelButtonRef.current
    ) {
      event.preventDefault();
      confirmButtonRef.current?.focus();
      return;
    }
    if (
      !event.shiftKey &&
      document.activeElement === confirmButtonRef.current
    ) {
      event.preventDefault();
      cancelButtonRef.current?.focus();
    }
  };

  return (
    <div
      className="dialogBackdrop"
      role="presentation"
      onKeyDown={handleKeyDown}
    >
      <section
        aria-busy={busy}
        aria-describedby={messageId}
        aria-labelledby={titleId}
        aria-modal="true"
        className="confirmDialog"
        data-state={busy ? "busy" : "idle"}
        role="dialog"
      >
        <header className="confirmDialogHeader">
          <h2 id={titleId}>{title}</h2>
          <span aria-hidden="true" className="dialogActivityIndicator" />
        </header>
        <div className="confirmDialogBody">
          <p id={messageId}>{message}</p>
          {option !== undefined && (
            <label className="confirmDialogOption">
              <input
                checked={option.checked}
                disabled={busy}
                type="checkbox"
                onChange={(event) =>
                  option.onCheckedChange(event.target.checked)
                }
              />
              <span>{option.label}</span>
            </label>
          )}
        </div>
        <footer className="confirmDialogActions">
          <button
            disabled={busy}
            ref={cancelButtonRef}
            type="button"
            onClick={onCancel}
          >
            {cancelLabel}
          </button>
          <button
            className="dangerButton"
            disabled={busy}
            ref={confirmButtonRef}
            type="button"
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </footer>
      </section>
    </div>
  );
}
