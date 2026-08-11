import {
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
  useEffect,
  useId,
  useRef,
} from "react";
import { createPortal } from "react-dom";

interface RuleEditorDialogProps {
  open: boolean;
  title: string;
  cancelLabel: string;
  confirmLabel: string;
  disabled?: boolean;
  confirmDisabled?: boolean;
  children: ReactNode;
  onCancel(): void;
  onConfirm(): void;
}

/**
 * 渲染规则设置窗口内的二级编辑对话框。
 * 运行上下文：父窗口只展示有序规则摘要，新增与编辑先在本组件维护的草稿中完成。
 * 参数：children 是当前规则的字段表单；confirmDisabled 表示领域校验尚未通过，disabled 表示提交期间冻结整个窗口。
 * 失败语义：原生或领域校验未通过时不会触发 onConfirm，取消只丢弃当前草稿。
 */
export function RuleEditorDialog({
  open,
  title,
  cancelLabel,
  confirmLabel,
  disabled = false,
  confirmDisabled = false,
  children,
  onCancel,
  onConfirm,
}: RuleEditorDialogProps) {
  const dialogRef = useRef<HTMLElement>(null);
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const titleId = useId();

  useEffect(() => {
    if (!open) {
      return undefined;
    }
    restoreFocusRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    const firstField = dialogRef.current?.querySelector<HTMLElement>(
      "input:not([type='hidden']):not(:disabled), select:not(:disabled), textarea:not(:disabled)",
    );
    (firstField ?? cancelButtonRef.current)?.focus();
    return () => {
      restoreFocusRef.current?.focus();
      restoreFocusRef.current = null;
    };
  }, [open]);

  if (!open) {
    return null;
  }

  /** Escape 只丢弃当前规则草稿，不关闭外层工具设置窗口。 */
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape" && !disabled) {
      event.preventDefault();
      event.stopPropagation();
      onCancel();
      return;
    }
    if (event.key !== "Tab") {
      return;
    }
    // 二级模态必须独占键盘焦点，否则 Tab 会落到被遮挡的父窗口并形成错误操作目标。
    const focusable = Array.from(
      dialogRef.current?.querySelectorAll<HTMLElement>(
        "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex='-1'])",
      ) ?? [],
    );
    if (focusable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  };

  /** 浏览器完成 required、范围与类型校验后，才把完整草稿交给父级提交。 */
  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    event.stopPropagation();
    if (!disabled) {
      onConfirm();
    }
  };

  // Portal 将二级表单移出外层设置表单，避免浏览器把嵌套 form 修复成错误提交目标。
  return createPortal(
    <div
      className="ruleEditorBackdrop"
      role="presentation"
      onKeyDown={handleKeyDown}
    >
      <section
        ref={dialogRef}
        aria-labelledby={titleId}
        aria-modal="true"
        className="ruleEditorDialog"
        role="dialog"
      >
        <form onSubmit={handleSubmit}>
          <header className="ruleEditorDialogHeader">
            <h3 id={titleId}>{title}</h3>
          </header>
          <div className="ruleEditorDialogBody">{children}</div>
          <footer className="ruleEditorDialogFooter">
            <button
              ref={cancelButtonRef}
              disabled={disabled}
              type="button"
              onClick={onCancel}
            >
              {cancelLabel}
            </button>
            <button disabled={disabled || confirmDisabled} type="submit">
              {confirmLabel}
            </button>
          </footer>
        </form>
      </section>
    </div>,
    document.body,
  );
}
