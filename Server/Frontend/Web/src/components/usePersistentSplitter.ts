import {
  type PointerEvent as ReactPointerEvent,
  type RefObject,
  useCallback,
  useEffect,
  useState,
} from "react";

export type SplitterOrientation = "vertical" | "horizontal";

interface SplitterLimits {
  defaultSize: number;
  minimumSize: number;
  maximumSize: number;
  minimumDetailSize: number;
}

export interface SplitterOptions {
  storageKey: string;
  stackedMediaQuery: string;
  desktop: SplitterLimits;
  stacked: SplitterLimits;
}

interface SplitterResult {
  navigatorSize: number;
  orientation: SplitterOrientation;
  minimumSize: number;
  maximumSize: number;
  beginResize(event: ReactPointerEvent<HTMLDivElement>): void;
  resetSize(): void;
  resizeBy(delta: number): void;
}

const splitterThickness = 5;

/**
 * 读取当前分栏轴的尺寸约束；垂直分隔条控制宽度，水平分隔条控制高度。
 */
function resolveLimits(
  options: SplitterOptions,
  orientation: SplitterOrientation,
): SplitterLimits {
  return orientation === "vertical" ? options.desktop : options.stacked;
}

/**
 * 读取容器在当前分栏轴上的可用尺寸，保证详情区始终保留最低空间。
 */
function resolveContainerSize(
  container: HTMLElement,
  orientation: SplitterOrientation,
): number {
  return orientation === "vertical"
    ? container.clientWidth
    : container.clientHeight;
}

/**
 * 将导航区尺寸约束在容器可用范围；极小窗口优先避免整体溢出。
 */
function clampNavigatorSize(
  requestedSize: number,
  containerSize: number,
  limits: SplitterLimits,
): number {
  const availableMaximum = Math.max(
    0,
    Math.min(
      limits.maximumSize,
      containerSize - limits.minimumDetailSize - splitterThickness,
    ),
  );
  const availableMinimum = Math.min(limits.minimumSize, availableMaximum);
  return Math.max(
    availableMinimum,
    Math.min(requestedSize, availableMaximum),
  );
}

/**
 * 为宽度与高度使用独立持久化键，避免切换响应式布局时复用错误尺寸。
 */
function resolveStorageKey(
  options: SplitterOptions,
  orientation: SplitterOrientation,
): string {
  const axisName = orientation === "vertical" ? "width" : "height";
  return `${options.storageKey}:${axisName}`;
}

/**
 * 读取持久化尺寸；缺失或损坏时使用当前布局的设计尺寸。
 */
function readStoredSize(
  options: SplitterOptions,
  orientation: SplitterOrientation,
): number {
  const limits = resolveLimits(options, orientation);
  const storedSize = Number(
    window.localStorage.getItem(resolveStorageKey(options, orientation)),
  );
  return Number.isFinite(storedSize) && storedSize > 0
    ? storedSize
    : limits.defaultSize;
}

/**
 * 管理桌面左右与窄屏上下两种可拖动分栏；每个方向独立约束并持久化。
 */
export function usePersistentSplitter(
  containerRef: RefObject<HTMLElement | null>,
  options: SplitterOptions,
): SplitterResult {
  const [orientation, setOrientation] = useState<SplitterOrientation>(() =>
    window.matchMedia(options.stackedMediaQuery).matches
      ? "horizontal"
      : "vertical",
  );
  const [navigatorSizes, setNavigatorSizes] = useState(() => ({
    vertical: readStoredSize(options, "vertical"),
    horizontal: readStoredSize(options, "horizontal"),
  }));
  const limits = resolveLimits(options, orientation);
  const navigatorSize = navigatorSizes[orientation];

  useEffect(() => {
    const mediaQuery = window.matchMedia(options.stackedMediaQuery);

    /**
     * 同步 CSS 断点对应的分栏轴，避免视觉方向与拖动计算方向分离。
     */
    const updateOrientation = () => {
      setOrientation(mediaQuery.matches ? "horizontal" : "vertical");
    };

    mediaQuery.addEventListener("change", updateOrientation);
    updateOrientation();
    return () => mediaQuery.removeEventListener("change", updateOrientation);
  }, [options.stackedMediaQuery]);

  useEffect(() => {
    const container = containerRef.current;
    if (container === null) {
      return;
    }

    /**
     * 容器变化时只收紧当前轴的越界尺寸，不覆盖另一响应式布局的记录。
     */
    const updateBounds = () => {
      const containerSize = resolveContainerSize(container, orientation);
      setNavigatorSizes((currentSizes) => ({
        ...currentSizes,
        [orientation]: clampNavigatorSize(
          currentSizes[orientation],
          containerSize,
          limits,
        ),
      }));
    };

    const observer = new ResizeObserver(updateBounds);
    observer.observe(container);
    updateBounds();
    return () => observer.disconnect();
  }, [containerRef, limits, orientation]);

  /**
   * 开始当前轴的指针拖动；全局监听保证指针离开分隔条后仍可连续调整。
   */
  const beginResize = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const container = containerRef.current;
      if (container === null) {
        return;
      }

      event.preventDefault();
      const startCoordinate =
        orientation === "vertical" ? event.clientX : event.clientY;
      const startSize = navigatorSize;
      const containerSize = resolveContainerSize(container, orientation);
      let finalSize = startSize;

      /**
       * 根据当前轴位移更新尺寸；窄屏模式只读取纵向坐标。
       */
      const moveSplitter = (pointerEvent: PointerEvent) => {
        const pointerCoordinate =
          orientation === "vertical"
            ? pointerEvent.clientX
            : pointerEvent.clientY;
        finalSize = clampNavigatorSize(
          startSize + pointerCoordinate - startCoordinate,
          containerSize,
          limits,
        );
        setNavigatorSizes((currentSizes) => ({
          ...currentSizes,
          [orientation]: finalSize,
        }));
      };

      /**
       * 结束拖动并持久化最终尺寸；移除全局监听避免重复处理。
       */
      const finishResize = () => {
        window.removeEventListener("pointermove", moveSplitter);
        window.removeEventListener("pointerup", finishResize);
        window.localStorage.setItem(
          resolveStorageKey(options, orientation),
          String(finalSize),
        );
      };

      window.addEventListener("pointermove", moveSplitter);
      window.addEventListener("pointerup", finishResize, { once: true });
    },
    [containerRef, limits, navigatorSize, options, orientation],
  );

  /**
   * 使用键盘按固定步长调整当前轴，并立即持久化供下次窗口打开恢复。
   */
  const resizeBy = useCallback(
    (delta: number) => {
      const container = containerRef.current;
      if (container === null) {
        return;
      }
      const nextSize = clampNavigatorSize(
        navigatorSize + delta,
        resolveContainerSize(container, orientation),
        limits,
      );
      window.localStorage.setItem(
        resolveStorageKey(options, orientation),
        String(nextSize),
      );
      setNavigatorSizes((currentSizes) => ({
        ...currentSizes,
        [orientation]: nextSize,
      }));
    },
    [containerRef, limits, navigatorSize, options, orientation],
  );

  /**
   * 双击分隔条恢复当前布局的设计尺寸并立即持久化。
   */
  const resetSize = useCallback(() => {
    const container = containerRef.current;
    if (container === null) {
      return;
    }
    const nextSize = clampNavigatorSize(
      limits.defaultSize,
      resolveContainerSize(container, orientation),
      limits,
    );
    window.localStorage.setItem(
      resolveStorageKey(options, orientation),
      String(nextSize),
    );
    setNavigatorSizes((currentSizes) => ({
      ...currentSizes,
      [orientation]: nextSize,
    }));
  }, [containerRef, limits, options, orientation]);

  return {
    navigatorSize,
    orientation,
    minimumSize: limits.minimumSize,
    maximumSize: limits.maximumSize,
    beginResize,
    resetSize,
    resizeBy,
  };
}
