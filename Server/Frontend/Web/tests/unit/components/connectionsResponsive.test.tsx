import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ConnectionsWorkspace } from "@/components/connectionsWorkspace";

vi.mock("@/state/serviceStore", async () => {
  const {
    createServiceSnapshot,
    createTransactionSummary,
  } = await import("#tests/testFixtures");
  const transaction = createTransactionSummary();
  const baseSnapshot = createServiceSnapshot();
  const getTransactionDetail = vi.fn(async () => ({
    revision: 1,
    transaction,
    requestHeaders: [],
    responseHeaders: [],
    requestBody: null,
    responseBody: null,
  }));
  const storeValue = {
    snapshot: createServiceSnapshot({
      recording: {
        ...baseSnapshot.recording,
        transactionCount: 1,
      },
      transactions: {
        ...baseSnapshot.transactions,
        total: 1,
        items: [transaction],
      },
    }),
    lastError: null,
    refresh: vi.fn(),
    getTransactionDetail,
    getLiveTransactionDetail: getTransactionDetail,
    getTransactionBody: vi.fn(),
  };
  return {
    useServiceStore: () => storeValue,
  };
});

/**
 * 创建带稳定坐标的指针事件；JSDOM 未提供完整 PointerEvent 坐标实现。
 */
function createPointerEvent(
  type: string,
  clientX: number,
  clientY: number,
): Event {
  const event = new Event(type, { bubbles: true });
  Object.defineProperties(event, {
    clientX: { value: clientX },
    clientY: { value: clientY },
  });
  return event;
}

beforeEach(() => {
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({
      matches: true,
      media: "(max-width: 960px)",
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })),
  );
  vi.stubGlobal(
    "ResizeObserver",
    vi.fn(() => ({
      observe: vi.fn(),
      unobserve: vi.fn(),
      disconnect: vi.fn(),
    })),
  );
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(600);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("窄屏上下分栏", () => {
  it("使用纵向坐标调整导航高度并暴露水平分隔条语义", () => {
    render(<ConnectionsWorkspace />);
    expect(window.matchMedia).toHaveBeenCalledWith("(max-width: 960px)");
    const separator = screen.getByRole("separator", {
      name: "调整事务导航高度",
    });
    const workspace = separator.closest("main");

    expect(separator).toHaveAttribute("aria-orientation", "horizontal");
    expect(separator).toHaveAttribute("aria-valuenow", "280");
    expect(workspace).toHaveStyle({ "--navigator-size": "280px" });

    fireEvent(separator, createPointerEvent("pointerdown", 500, 100));
    fireEvent(window, createPointerEvent("pointermove", 900, 164));
    fireEvent(window, createPointerEvent("pointerup", 900, 164));

    expect(separator).toHaveAttribute("aria-valuenow", "344");
    expect(workspace).toHaveStyle({ "--navigator-size": "344px" });
  });
});
