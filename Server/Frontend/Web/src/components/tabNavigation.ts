import type { KeyboardEvent } from "react";

const tabNavigationKeys = new Set([
  "ArrowLeft",
  "ArrowRight",
  "Home",
  "End",
]);

/**
 * 为紧凑页签组提供一致的方向键导航；只在当前 tablist 内移动并激活可用页签。
 */
export function activateAdjacentTab(
  event: KeyboardEvent<HTMLElement>,
): void {
  if (!tabNavigationKeys.has(event.key)) {
    return;
  }
  const currentTab = (event.target as Element).closest<HTMLElement>(
    '[role="tab"]',
  );
  const tabs = Array.from(
    event.currentTarget.querySelectorAll<HTMLElement>(
      '[role="tab"]:not([disabled])',
    ),
  );
  const currentIndex =
    currentTab === null ? -1 : tabs.indexOf(currentTab);
  if (currentIndex < 0 || tabs.length === 0) {
    return;
  }

  event.preventDefault();
  const lastIndex = tabs.length - 1;
  const targetIndex =
    event.key === "Home"
      ? 0
      : event.key === "End"
        ? lastIndex
        : event.key === "ArrowRight"
          ? (currentIndex + 1) % tabs.length
          : (currentIndex - 1 + tabs.length) % tabs.length;
  tabs[targetIndex].focus();
  tabs[targetIndex].click();
}
