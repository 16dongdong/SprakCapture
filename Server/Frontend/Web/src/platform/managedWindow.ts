import { isTauri } from "@tauri-apps/api/core";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { Window as TauriWindow } from "@tauri-apps/api/window";
import i18n from "../i18n";

/** 描述受管窗口显示、隐藏、恢复与关闭所需的最小原生能力，使互斥编排可脱离 Tauri 运行时测试。 */
export interface ManagedApplicationWindow {
  unminimize(): Promise<void>;
  show(): Promise<void>;
  hide(): Promise<void>;
  setFocus(): Promise<void>;
  close(): Promise<void>;
}

/** 描述动态业务窗口的稳定尺寸和路由；label 必须使用桌面能力清单允许的前缀。 */
export interface ManagedRouteWindow {
  label: `app-window-${string}`;
  path: string;
  title: string;
  width: number;
  height: number;
  minWidth: number;
  minHeight: number;
}

/** 隔离桌面窗口 API 与浏览器弹窗 API，避免业务组件读取 Tauri 私有状态。 */
export interface ManagedWindowPlatform {
  isDesktop(): boolean;
  findManagedWindow(
    windowLabel: string,
  ): Promise<ManagedApplicationWindow | null>;
  createManagedWindow(
    windowTarget: ManagedRouteWindow,
  ): Promise<ManagedApplicationWindow>;
  currentManagedWindow(): ManagedApplicationWindow;
  openBrowserWindow(
    windowPath: string,
    windowName: string,
    windowFeatures?: string,
  ): boolean;
  closeBrowserWindow(): void;
}

interface WindowTarget {
  label: string;
  browserPath: string;
  browserName: string;
  browserFeatures?: string;
}

const mainWindowTarget: WindowTarget = {
  label: "main",
  browserPath: "/connections",
  browserName: "mainWindow",
};

const floatingWindowTarget: WindowTarget = {
  label: "floating",
  browserPath: "/floating",
  browserName: "floatingPanel",
  browserFeatures: "popup=yes,width=340,height=250,resizable=yes",
};

/** 返回目标窗口的互斥窗口；主工作区与悬浮面板只能有一个处于可见状态。 */
function exclusiveWindowLabel(windowTarget: WindowTarget): string {
  return windowTarget.label === mainWindowTarget.label
    ? floatingWindowTarget.label
    : mainWindowTarget.label;
}

/**
 * 生成独立 Webview 的原生窗口配置；系统装饰负责 Windows 11 圆角和阴影，非透明底色避免 WebView2 首帧闪白。
 * 失败语义：该纯函数不访问运行时；非法尺寸由 Tauri 创建事件返回明确错误，不在前端伪造修正值。
 */
export function createManagedWindowOptions(
  windowTarget: ManagedRouteWindow,
): ConstructorParameters<typeof WebviewWindow>[1] {
  return {
    url: windowTarget.path,
    title: windowTarget.title,
    width: windowTarget.width,
    height: windowTarget.height,
    minWidth: windowTarget.minWidth,
    minHeight: windowTarget.minHeight,
    center: true,
    preventOverflow: { width: 24, height: 24 },
    focus: true,
    focusable: true,
    resizable: true,
    decorations: true,
    transparent: false,
    shadow: true,
    titleBarStyle: "transparent",
    hiddenTitle: true,
    acceptFirstMouse: true,
    backgroundColor: [238, 240, 243, 255],
    visible: false,
  };
}

/**
 * 等待动态 WebviewWindow 完成原生创建，确保后续聚焦不会与窗口初始化竞态。
 * 失败语义：Tauri 返回的创建错误原样转换为 Error，调用方不得伪造窗口已打开状态。
 */
async function createDesktopWindow(
  windowTarget: ManagedRouteWindow,
): Promise<ManagedApplicationWindow> {
  const webviewWindow = new WebviewWindow(
    windowTarget.label,
    createManagedWindowOptions(windowTarget),
  );
  await new Promise<void>((resolve, reject) => {
    void webviewWindow.once("tauri://created", () => resolve());
    void webviewWindow.once<string>("tauri://error", (event) => {
      reject(new Error(event.payload));
    });
  });
  return webviewWindow;
}

const defaultPlatform: ManagedWindowPlatform = {
  isDesktop: isTauri,
  findManagedWindow: (windowLabel) => TauriWindow.getByLabel(windowLabel),
  createManagedWindow: createDesktopWindow,
  currentManagedWindow: () => TauriWindow.getCurrent(),
  openBrowserWindow: (windowPath, windowName, windowFeatures) => {
    return window.open(windowPath, windowName, windowFeatures) !== null;
  },
  closeBrowserWindow: () => window.close(),
};

// React 严格模式和快速重复点击可能并发请求同一 label；共享创建 Promise 可避免 Tauri 返回重复 label 错误。
const pendingWindowCreations = new Map<
  string,
  Promise<ManagedApplicationWindow>
>();
let windowTransitionChain: Promise<void> = Promise.resolve();

/** 串行化主窗口与悬浮面板的切换，避免并发点击在两个异步隐藏/显示序列之间留下双窗口可见状态。 */
function enqueueWindowTransition(
  transition: () => Promise<void>,
): Promise<void> {
  const nextTransition = windowTransitionChain.then(transition, transition);
  windowTransitionChain = nextTransition.catch(() => undefined);
  return nextTransition;
}

/**
 * 按目标声明恢复预创建窗口；Tauri 缺失预创建窗口属于安装配置错误，浏览器则复用命名窗口。
 * 桌面端先隐藏互斥窗口，再显示并聚焦目标，保证主工作区和悬浮面板不会同时可见。
 */
async function showWindowTarget(
  windowTarget: WindowTarget,
  platform: ManagedWindowPlatform,
): Promise<void> {
  if (!platform.isDesktop()) {
    platform.openBrowserWindow(
      windowTarget.browserPath,
      windowTarget.browserName,
      windowTarget.browserFeatures,
    );
    return;
  }

  const managedWindow = await platform.findManagedWindow(windowTarget.label);
  if (managedWindow === null) {
    throw new Error(
      i18n.t("error.web.managedWindowMissing", { label: windowTarget.label }),
    );
  }
  const exclusiveWindow = await platform.findManagedWindow(
    exclusiveWindowLabel(windowTarget),
  );
  if (exclusiveWindow !== null) {
    await exclusiveWindow.hide();
  }
  await focusManagedWindow(managedWindow);
}

/** 恢复并聚焦已创建窗口；固定调用顺序兼容 Windows 最小化窗口的恢复约束。 */
async function focusManagedWindow(
  managedWindow: ManagedApplicationWindow,
): Promise<void> {
  await managedWindow.unminimize();
  await managedWindow.show();
  await managedWindow.setFocus();
}

/** 恢复主窗口；浏览器调试态复用连接会话命名窗口。 */
export function showMainWindow(
  platform: ManagedWindowPlatform = defaultPlatform,
): Promise<void> {
  return enqueueWindowTransition(() =>
    showWindowTarget(mainWindowTarget, platform),
  );
}

/** 恢复悬浮面板；浏览器调试态复用固定尺寸的命名弹窗。 */
export function showFloatingPanel(
  platform: ManagedWindowPlatform = defaultPlatform,
): Promise<void> {
  return enqueueWindowTransition(() =>
    showWindowTarget(floatingWindowTarget, platform),
  );
}

/**
 * 创建或复用一个独立业务窗口；路由与尺寸来自统一 contract，避免各组件复制窗口参数。
 * 浏览器调试通过 label 对应的命名窗口复用，桌面端只在 label 尚不存在时创建 WebviewWindow。
 */
export async function showManagedRouteWindow(
  windowTarget: ManagedRouteWindow,
  platform: ManagedWindowPlatform = defaultPlatform,
): Promise<void> {
  if (!platform.isDesktop()) {
    platform.openBrowserWindow(
      windowTarget.path,
      windowTarget.label,
      `popup=yes,width=${windowTarget.width},height=${windowTarget.height},resizable=yes`,
    );
    return;
  }
  const existingWindow = await platform.findManagedWindow(windowTarget.label);
  let managedWindow = existingWindow;
  if (managedWindow === null) {
    const pendingCreation =
      pendingWindowCreations.get(windowTarget.label) ??
      platform.createManagedWindow(windowTarget);
    pendingWindowCreations.set(windowTarget.label, pendingCreation);
    try {
      managedWindow = await pendingCreation;
    } finally {
      pendingWindowCreations.delete(windowTarget.label);
    }
  }
  await focusManagedWindow(managedWindow);
}

/**
 * 关闭当前独立窗口；浏览器和桌面实现保持同一按钮语义。
 * 失败语义：原生窗口拒绝关闭时 Promise 失败，业务草稿保持在当前页面而不会被伪装成已取消。
 */
const windowExitAnimationMilliseconds = 140;

/**
 * 在原生窗口销毁前播放只使用合成属性的退出动画。
 * 测试平台和不含独立窗口根节点的页面跳过等待，避免把生命周期延迟扩散到普通调用方。
 */
async function playWindowExitAnimation(): Promise<void> {
  const windowSurface = document.querySelector(".independentWindowSurface");
  if (windowSurface === null) {
    return;
  }
  document.documentElement.classList.add("isWindowClosing");
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
    return;
  }
  await new Promise<void>((resolve) => {
    window.setTimeout(resolve, windowExitAnimationMilliseconds);
  });
}

export async function closeCurrentManagedWindow(
  platform: ManagedWindowPlatform = defaultPlatform,
): Promise<void> {
  const animated = platform === defaultPlatform;
  if (animated) {
    await playWindowExitAnimation();
  }
  try {
    if (!platform.isDesktop()) {
      platform.closeBrowserWindow();
      return;
    }
    await platform.currentManagedWindow().close();
  } catch (error) {
    // 原生窗口拒绝关闭时恢复交互状态，确保草稿仍可操作并允许用户再次尝试。
    if (animated) {
      document.documentElement.classList.remove("isWindowClosing");
    }
    throw error;
  }
}
