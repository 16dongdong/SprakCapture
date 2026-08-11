import {
  type KeyboardEvent,
  type RefObject,
  useCallback,
  useEffect,
  useRef,
} from "react";

/** 模态焦点管理器的输入；引用对象应在组件生命周期内保持稳定。 */
export interface ModalFocusOptions {
  /** 模态是否打开；焦点捕获与恢复仅由该状态边沿驱动。 */
  open: boolean;
  /** 包含全部可交互元素的模态根节点。 */
  containerRef: RefObject<HTMLElement | null>;
  /** 可选的首选初始焦点，例如关闭按钮。 */
  initialFocusRef?: RefObject<HTMLElement | null>;
}

/** 供模态根节点展开的键盘绑定。 */
export interface ModalFocusBindings {
  /** 在模态内部循环 Tab 与 Shift+Tab 焦点。 */
  onKeyDown(event: KeyboardEvent<HTMLElement>): void;
}

const focusableSelector = [
  "a[href]",
  "area[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "iframe",
  "object",
  "embed",
  "[contenteditable='true']",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

/**
 * 收集容器内当前可参与键盘导航的元素。
 * 过滤 hidden、inert 与 aria-hidden 子树，避免将焦点送入不可交互区域。
 */
function findFocusableElements(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(focusableSelector)).filter(
    (element) =>
      !element.hidden &&
      element.tabIndex >= 0 &&
      element.closest("[inert], [aria-hidden='true']") === null,
  );
}

/** 将焦点恢复到打开模态前的元素；元素已卸载时不强行跳转到无关节点。 */
function restoreFocus(element: HTMLElement | null): void {
  if (element !== null && element.isConnected) {
    element.focus();
  }
}

/**
 * 在模态挂载完成后建立初始焦点。
 * 优先使用调用方指定的元素，随后回退到首个可交互元素，最后聚焦容器自身。
 */
function focusInitialElement(
  container: HTMLElement | null,
  preferredElement: HTMLElement | null | undefined,
): void {
  if (container === null) {
    return;
  }
  if (
    preferredElement !== null &&
    preferredElement !== undefined &&
    container.contains(preferredElement) &&
    !preferredElement.matches(":disabled, [aria-hidden='true']")
  ) {
    preferredElement.focus();
    return;
  }
  const firstFocusableElement = findFocusableElements(container)[0];
  if (firstFocusableElement !== undefined) {
    firstFocusableElement.focus();
    return;
  }
  container.focus();
}

/**
 * 管理可访问模态的焦点边界。
 * Effect 仅依赖 open，因此服务快照、表单草稿等刷新不会重新捕获或恢复焦点。
 */
export function useModalFocus({
  open,
  containerRef,
  initialFocusRef,
}: ModalFocusOptions): ModalFocusBindings {
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open || typeof document === "undefined") {
      return undefined;
    }
    const activeElement = document.activeElement;
    restoreFocusRef.current =
      activeElement instanceof HTMLElement ? activeElement : null;
    const frameId = window.requestAnimationFrame(() => {
      focusInitialElement(containerRef.current, initialFocusRef?.current);
    });

    /** 关闭或卸载时取消尚未执行的初始聚焦，并恢复打开前的焦点。 */
    return () => {
      window.cancelAnimationFrame(frameId);
      restoreFocus(restoreFocusRef.current);
      restoreFocusRef.current = null;
    };
  }, [open]);

  /**
   * 将 Tab 导航限制在容器内。
   * 当前焦点不在可聚焦集合时，按方向分别跳转至首项或末项，保持键盘路径闭合。
   */
  const onKeyDown = useCallback(
    (event: KeyboardEvent<HTMLElement>) => {
      if (!open || event.key !== "Tab") {
        return;
      }
      const container = containerRef.current;
      if (container === null) {
        return;
      }
      const focusableElements = findFocusableElements(container);
      if (focusableElements.length === 0) {
        event.preventDefault();
        container.focus();
        return;
      }
      const activeElement = document.activeElement;
      const currentIndex = focusableElements.indexOf(
        activeElement instanceof HTMLElement ? activeElement : container,
      );
      const nextIndex = event.shiftKey
        ? currentIndex <= 0
          ? focusableElements.length - 1
          : currentIndex - 1
        : currentIndex === focusableElements.length - 1
          ? 0
          : currentIndex + 1;
      event.preventDefault();
      focusableElements[nextIndex]?.focus();
    },
    [containerRef, open],
  );

  return { onKeyDown };
}
