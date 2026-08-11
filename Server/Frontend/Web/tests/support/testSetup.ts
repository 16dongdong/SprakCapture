import "@testing-library/jest-dom/vitest";

import { cleanup } from "@testing-library/react";
import { afterEach, beforeEach } from "vitest";

import i18n, { localeStorageKey } from "@/i18n";

/**
 * 为 jsdom 补齐组件库依赖的尺寸观察接口。
 *
 * 运行上下文：jsdom 不实现布局观察，而 Radix 菜单滚动区会在挂载时注册观察器。
 * 参数：回调由浏览器运行时触发；测试夹具不计算布局，因此不保存或调用该回调。
 * 失败语义：只在接口缺失时安装，真实浏览器与未来 jsdom 实现保持原生行为。
 */
class TestResizeObserver {
  constructor(_callback: ResizeObserverCallback) {}

  /** 测试环境不计算元素布局，观察注册保持空操作。 */
  observe(_target: Element): void {}

  /** 测试环境不维护观察列表，注销保持空操作。 */
  unobserve(_target: Element): void {}

  /** 测试环境没有观察资源，断开连接保持幂等。 */
  disconnect(): void {}
}

if (typeof globalThis.ResizeObserver === "undefined") {
  Object.defineProperty(globalThis, "ResizeObserver", {
    configurable: true,
    value: TestResizeObserver,
    writable: true,
  });
}

const capturedPointers = new WeakMap<Element, Set<number>>();

/**
 * 为 jsdom 记录元素捕获的指针编号，使依赖 Pointer Capture 的拖动组件保持浏览器语义。
 * 参数为指针编号；测试环境不调度真实设备事件，失败时由 WeakMap 操作直接报告异常。
 */
function setTestPointerCapture(this: Element, pointerId: number): void {
  const pointerIds = capturedPointers.get(this) ?? new Set<number>();
  pointerIds.add(pointerId);
  capturedPointers.set(this, pointerIds);
}

/**
 * 查询 jsdom 元素是否持有指定指针，用于验证释放路径不会误处理其他指针。
 * 参数为指针编号；元素没有捕获记录时返回 false。
 */
function hasTestPointerCapture(this: Element, pointerId: number): boolean {
  return capturedPointers.get(this)?.has(pointerId) ?? false;
}

/**
 * 释放 jsdom 元素上的指定指针记录，模拟浏览器在 pointerup 后终止捕获。
 * 参数为指针编号；重复释放保持幂等，不影响其他元素或其他指针。
 */
function releaseTestPointerCapture(this: Element, pointerId: number): void {
  capturedPointers.get(this)?.delete(pointerId);
}

if (typeof Element.prototype.setPointerCapture === "undefined") {
  Element.prototype.setPointerCapture = setTestPointerCapture;
  Element.prototype.hasPointerCapture = hasTestPointerCapture;
  Element.prototype.releasePointerCapture = releaseTestPointerCapture;
}
beforeEach(async () => {
  // 既有组件断言以简体中文为稳定基线；单独的国际化测试再显式切换语言并负责恢复。
  window.localStorage.setItem(localeStorageKey, "zh-Hans");
  await i18n.changeLanguage("zh-Hans");
});

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});
