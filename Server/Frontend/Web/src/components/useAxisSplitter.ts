import {
  type KeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

export interface AxisSplitterOptions {
  axis: "horizontal" | "vertical";
  dividerSize: number;
  initialRatio?: number;
}

export interface AxisSplitterState {
  ratio: number;
  resizing: boolean;
  beginResize(event: ReactPointerEvent<HTMLDivElement>): void;
  continueResize(event: ReactPointerEvent<HTMLDivElement>): void;
  finishResize(event: ReactPointerEvent<HTMLDivElement>): void;
  handleKeyDown(event: KeyboardEvent<HTMLDivElement>): void;
}

const keyboardStep = 0.05;
const keyboardPageStep = 0.1;

/** 把分栏比例限制在完整的 0..1 轨道内；边界值表示一侧完全收起而分割条仍保持可操作。 */
function clampRatio(ratio: number): number {
  return Math.min(1, Math.max(0, ratio));
}

/**
 * 管理水平或垂直分割条的指针捕获和键盘状态，使正文分栏能够连续拖到任一边界。
 *
 * 运行上下文：调用方把 ref 绑定到固定尺寸容器，并将返回 ratio 写入三轨 CSS Grid。
 * 参数：containerRef 指向分栏容器；options 指定分割轴、可见分割条尺寸和初始比例。
 * 失败语义：容器未挂载、尺寸无效或非主鼠标按键时保持原比例，不制造跳变或全局监听器。
 */
export function useAxisSplitter(
  containerRef: RefObject<HTMLDivElement>,
  options: AxisSplitterOptions,
): AxisSplitterState {
  const activePointerIdRef = useRef<number | null>(null);
  const [ratio, setRatio] = useState(() =>
    clampRatio(options.initialRatio ?? 0.5),
  );
  const [resizing, setResizing] = useState(false);

  /** 按容器内指针坐标换算第一面板比例；半个分割条偏移保证 0 和 1 精确对应两端。 */
  const updateFromPointer = useCallback(
    (clientX: number, clientY: number) => {
      const container = containerRef.current;
      if (container === null) {
        return;
      }
      const bounds = container.getBoundingClientRect();
      const horizontal = options.axis === "horizontal";
      const size = horizontal ? bounds.height : bounds.width;
      const coordinate = horizontal ? clientY - bounds.top : clientX - bounds.left;
      const availableSize = size - options.dividerSize;
      if (!Number.isFinite(coordinate) || availableSize <= 0) {
        return;
      }
      const nextRatio = clampRatio(
        (coordinate - options.dividerSize / 2) / availableSize,
      );
      setRatio((currentRatio) =>
        Math.abs(currentRatio - nextRatio) < 0.001 ? currentRatio : nextRatio,
      );
    },
    [containerRef, options.axis, options.dividerSize],
  );

  /** 捕获主指针并从按下位置立即更新，避免分割条开始拖动时先产生一次视觉跳跃。 */
  const beginResize = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (event.pointerType === "mouse" && event.button !== 0) {
        return;
      }
      event.preventDefault();
      activePointerIdRef.current = event.pointerId;
      event.currentTarget.setPointerCapture?.(event.pointerId);
      setResizing(true);
      updateFromPointer(event.clientX, event.clientY);
    },
    [updateFromPointer],
  );

  /** 只响应当前捕获的指针；多点触控和无关鼠标移动不得改写分栏比例。 */
  const continueResize = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      if (activePointerIdRef.current !== event.pointerId) {
        return;
      }
      updateFromPointer(event.clientX, event.clientY);
    },
    [updateFromPointer],
  );

  /** 释放分割条持有的指针并结束拖动反馈；取消事件与正常抬起使用相同收尾语义。 */
  const finishResize = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    if (activePointerIdRef.current !== event.pointerId) {
      return;
    }
    if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
      event.currentTarget.releasePointerCapture?.(event.pointerId);
    }
    activePointerIdRef.current = null;
    setResizing(false);
  }, []);

  useEffect(() => {
    if (!resizing) {
      return undefined;
    }
    /**
     * 在窗口级继续当前拖拽；浏览器或 WebView 重排 Grid 后可能提前丢失元素级指针捕获，窗口监听保证拖动不中断。
     */
    const continueWindowResize = (event: globalThis.PointerEvent) => {
      if (activePointerIdRef.current === event.pointerId) {
        updateFromPointer(event.clientX, event.clientY);
      }
    };
    /** 指针在元素外释放时结束状态，避免光标和禁止选择反馈永久滞留。 */
    const finishWindowResize = (event: globalThis.PointerEvent) => {
      if (activePointerIdRef.current !== event.pointerId) {
        return;
      }
      activePointerIdRef.current = null;
      setResizing(false);
    };
    window.addEventListener("pointermove", continueWindowResize);
    window.addEventListener("pointerup", finishWindowResize);
    window.addEventListener("pointercancel", finishWindowResize);
    return () => {
      window.removeEventListener("pointermove", continueWindowResize);
      window.removeEventListener("pointerup", finishWindowResize);
      window.removeEventListener("pointercancel", finishWindowResize);
    };
  }, [resizing, updateFromPointer]);

  /** 处理方向键、Page 键和 Home/End；键位移动方向与分割条在屏幕上的实际方向一致。 */
  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      const decreaseKey = options.axis === "horizontal" ? "ArrowUp" : "ArrowLeft";
      const increaseKey = options.axis === "horizontal" ? "ArrowDown" : "ArrowRight";
      let nextRatio: number | null = null;
      if (event.key === decreaseKey) {
        nextRatio = ratio - keyboardStep;
      } else if (event.key === increaseKey) {
        nextRatio = ratio + keyboardStep;
      } else if (event.key === "PageUp") {
        nextRatio = ratio - keyboardPageStep;
      } else if (event.key === "PageDown") {
        nextRatio = ratio + keyboardPageStep;
      } else if (event.key === "Home") {
        nextRatio = 0;
      } else if (event.key === "End") {
        nextRatio = 1;
      }
      if (nextRatio === null) {
        return;
      }
      event.preventDefault();
      setRatio(clampRatio(nextRatio));
    },
    [options.axis, ratio],
  );

  return {
    ratio,
    resizing,
    beginResize,
    continueResize,
    finishResize,
    handleKeyDown,
  };
}
