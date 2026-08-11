import { Button, DropdownMenu, IconButton, Theme } from "@radix-ui/themes";
import {
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { ListFilter, Settings as SettingsIcon } from "lucide-react";
import { useTranslation } from "react-i18next";
import { NavLink, useNavigate } from "react-router-dom";

import i18n from "../i18n";
import { showIndependentWindow } from "../platform/independentWindowContract";
import { showFloatingPanel } from "../platform/managedWindow";
import { useServiceStore } from "../state/serviceStore";
import { ToolbarIcon, type ToolbarIconName } from "./toolbarIcon";
import {
  defaultToolbarActionOrder,
  moveToolbarAction,
  moveToolbarActionToIndex,
  persistToolbarActionOrder,
  readToolbarActionOrder,
  type ToolbarActionId,
} from "./toolbarActionOrder";
import type { ListenerDialogId } from "./listenerSettingsDialog";
import type { ToolDialogId } from "./toolSettingsDialog";

const navigationItems: ReadonlyArray<{
  path: string;
  labelKey: string;
  inactiveIcon: ToolbarIconName;
  activeIcon: ToolbarIconName;
}> = [
  {
    path: "/overview",
    labelKey: "app.navigation.overview",
    inactiveIcon: "overviewInactive",
    activeIcon: "overviewActive",
  },
  {
    path: "/connections",
    labelKey: "app.navigation.connections",
    inactiveIcon: "connectionsInactive",
    activeIcon: "connectionsActive",
  },
];

const skipClearConfirmationStorageKey =
  "capture.recording.skipClearConfirmation";

/**
 * 读取清理确认偏好；偏好只属于当前浏览器界面，不进入代理服务配置或跨设备同步。
 *
 * 失败语义：存储不可访问时由浏览器直接报告异常，禁止静默改变破坏性操作的确认行为。
 */
function readSkipClearConfirmation(): boolean {
  return (
    window.localStorage.getItem(skipClearConfirmationStorageKey) === "true"
  );
}

/**
 * 请求显示悬浮面板；受管桌面窗口与浏览器调试窗口共享相同用户动作入口。
 */
async function openFloatingPanel(): Promise<void> {
  try {
    await showFloatingPanel();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(i18n.t("error.web.openFloatingPanel", { message }));
  }
}

/**
 * 在用户手势栈内同步请求设置独立窗口，避免 Chrome 把异步路由副作用判定为弹窗广告。
 * 原生或浏览器创建失败时记录完整错误；主窗口不跳转，也不伪造设置已经打开的状态。
 */
async function openSettingsWindow(section: "interface"): Promise<void> {
  try {
    await showIndependentWindow({ kind: "settings", section });
  } catch (error) {
    console.error("打开设置独立窗口失败", error);
  }
}

/**
 * 定义工具栏向上层请求打开临时 SSL 设置对话框的唯一回调。
 */
interface TopToolbarProps {
  onOpenSslSettings(): void;
  onOpenProtocolSettings(): void;
  onOpenToolSettings(tool: ToolDialogId): void;
  onOpenListenerSettings(listener: ListenerDialogId): void;
}

/**
 * 渲染主窗口工具栏；工具栏仅保留录制与工具快捷入口，语言设置和服务启停分别归属设置页与服务概览，避免跨层重复操作。
 */
export function TopToolbar({
  onOpenSslSettings,
  onOpenProtocolSettings,
  onOpenToolSettings,
  onOpenListenerSettings,
}: TopToolbarProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const {
    snapshot,
    refresh,
    toggleRecording,
    clearRecording,
    updateBreakpoints,
    updateThrottling,
    activeAction,
    controlConnection,
  } = useServiceStore();
  const [toolMenuOpen, setToolMenuOpen] = useState(false);
  const [toolbarReorderMode, setToolbarReorderMode] = useState(false);
  const [refreshPending, setRefreshPending] = useState(false);
  const [toolbarActionOrder, setToolbarActionOrder] = useState(
    readToolbarActionOrder,
  );
  const [draggedToolbarAction, setDraggedToolbarAction] =
    useState<ToolbarActionId | null>(null);
  const [toolbarDragOffsetX, setToolbarDragOffsetX] = useState(0);
  const [toolbarDropIndex, setToolbarDropIndex] = useState<number | null>(null);
  const [toolbarSettling, setToolbarSettling] = useState(false);
  const toolbarPointerDrag = useRef<{
    actionId: ToolbarActionId;
    actionOrder: ToolbarActionId[];
    dropIndex: number;
    pointerId: number;
    startX: number;
    startY: number;
  } | null>(null);
  const suppressToolbarClick = useRef(false);
  const recording = snapshot?.recording ?? null;
  const tools = snapshot?.tools ?? null;
  const suspendedBreakpointCount = tools?.suspendedBreakpointCount ?? 0;
  const recordingActive = recording?.state === "recording";
  const controlAvailable =
    controlConnection === "connected" && recording !== null;
  const recordingActionPending = activeAction === "recording";
  const recordingClearPending = activeAction === "recordingClear";
  const toolActionPending = activeAction === "tool";
  const clearActionDisabled =
    !controlAvailable ||
    recordingClearPending ||
    recording === null ||
    recording.transactionCount === 0;

  /**
   * 响应扫把按钮；已持久化免确认偏好时直接清理，否则在用户手势内创建独立确认窗口。
   *
   * 独立窗口负责等待权威快照归零和写入偏好，主窗口不会再被遮罩或焦点陷阱影响。
   * 失败语义：窗口创建或清理失败均保留现有事务，函数不伪造删除状态。
   */
  const requestClearRecording = () => {
    if (readSkipClearConfirmation()) {
      void clearRecording();
      return;
    }
    void showIndependentWindow({
      kind: "clearRecording",
      transactionCount: recording?.transactionCount ?? 0,
    }).catch((error: unknown) => {
      console.error("打开清空事务确认窗口失败", error);
    });
  };

  /**
   * 原子切换带宽限制开关，同时保留快照中的预设与范围配置。
   *
   * 运行上下文：顶栏只承担高频启停，详细参数由工具设置页维护。
   * 失败语义：工具快照缺失时不发起写入；请求失败由 Store 回填状态。
   */
  const toggleThrottling = () => {
    if (tools === null) {
      return;
    }

    const { presets: _presets, ...configuration } = tools.throttling;
    void updateThrottling({
      ...configuration,
      enabled: !configuration.enabled,
    });
  };

  /**
   * 原子切换断点开关，不修改既有规则、超时与等待配置。
   *
   * 运行上下文：快捷入口仅修改启停状态，配置编辑仍由独立设置入口负责。
   * 失败语义：工具快照缺失时不发起写入；请求失败由 Store 统一报告。
   */
  const toggleBreakpoints = () => {
    if (tools === null) {
      return;
    }

    void updateBreakpoints({
      ...tools.breakpoints,
      enabled: !tools.breakpoints.enabled,
    });
  };

  /**
   * 打开指定工具设置并释放下拉菜单焦点，避免路由或对话框切换后保留失效浮层。
   *
   * 参数：tool 为与控制面 ABI 对齐的工具标识。
   * 失败语义：打开行为由上层管理；本函数不伪造成功状态。
   */
  const openToolSettings = (tool: ToolDialogId) => {
    closeToolMenu();
    onOpenToolSettings(tool);
  };

  /**
   * 收起 Radix 菜单，路由跳转和对话框打开前统一释放触发器焦点。
   * 运行上下文：菜单本身的外部点击、Escape 和子菜单焦点恢复由组件库管理。
   */
  const closeToolMenu = () => {
    setToolMenuOpen(false);
  };

  /**
   * 从顶栏直接打开独立应用设置窗口，避免把全局设置混入抓包工具菜单。
   *
   * 运行上下文：监听、认证、容量与界面语言属于跨会话持久配置，由独立设置窗口承载。
   * 失败语义：窗口创建错误由统一窗口入口记录；本函数不会伪造设置已打开状态。
   */
  const openApplicationSettings = () => {
    void openSettingsWindow("interface");
  };

  /**
   * 从主工具栏打开独立进程选择器窗口；该入口与设置同级，不占用主导航的业务页面空间。
   * 窗口创建失败时保留当前页面和既有进程选择状态，只记录可诊断错误。
   */
  const openProcessManager = () => {
    void showIndependentWindow({ kind: "processManager" }).catch(
      (error: unknown) => console.error("打开进程选择器失败", error),
    );
  };

  /**
   * 打开 SSL 配置对话框并重置工具菜单导航，确保单次只展示一个模态配置入口。
   *
   * 运行上下文：SSL 设置属于服务职责域，但由独立对话框维护证书安装与代理配置。
   * 失败语义：SSL 对话框内的控制面错误不会改变菜单层级状态。
   */
  const openSslSettings = () => {
    closeToolMenu();
    onOpenSslSettings();
  };

  /** 打开协议工具 L3 设置；菜单关闭后再显示模态，避免焦点仍停留在已卸载的菜单项。 */
  const openProtocolSettings = () => {
    closeToolMenu();
    onOpenProtocolSettings();
  };

  /** 打开独立插件页面；先收起临时菜单，避免路由切换后保留失效的浮层焦点。 */
  const openPluginPage = () => {
    closeToolMenu();
    navigate("/plugins");
  };

  /**
   * 提交一次工具栏顺序变更并同步浏览器偏好，保证当前渲染和下次启动使用同一份完整排列。
   * 参数为已经校验的动作数组；Storage 写入失败时直接报告异常，不显示未持久化的伪成功顺序。
   */
  const commitToolbarActionOrder = (nextOrder: ToolbarActionId[]) => {
    persistToolbarActionOrder(nextOrder);
    setToolbarActionOrder(nextOrder);
  };

  /**
   * 记录鼠标或触控笔的排序起点并捕获指针，保证跨过按钮间隙时仍能收到结束事件。
   * 仅主指针左键进入排序候选态；未达到移动阈值时仍按普通按钮点击处理。
   */
  const startToolbarPointerDrag = (
    event: ReactPointerEvent<HTMLDivElement>,
    actionId: ToolbarActionId,
  ) => {
    // 默认模式必须把完整指针序列留给按钮；只有显式进入排序模式后才捕获指针。
    if (!toolbarReorderMode || !event.isPrimary || event.button !== 0) {
      return;
    }

    event.currentTarget.setPointerCapture(event.pointerId);
    const sourceIndex = toolbarActionOrder.indexOf(actionId);
    setToolbarSettling(false);
    setToolbarDragOffsetX(0);
    setToolbarDropIndex(sourceIndex);
    toolbarPointerDrag.current = {
      actionId,
      actionOrder: [...toolbarActionOrder],
      dropIndex: sourceIndex,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
    };
  };

  /**
   * 在指针移动超过四像素后让拖动实体磁吸到最近槽位，并驱动相邻控件实时让位。
   * 排序期间保持 DOM 顺序不变，避免频繁持久化和节点跳动；工具菜单会先关闭以释放浮层。
   */
  const updateToolbarPointerDrag = (
    event: ReactPointerEvent<HTMLDivElement>,
  ) => {
    const pointerDrag = toolbarPointerDrag.current;
    if (pointerDrag === null || pointerDrag.pointerId !== event.pointerId) {
      return;
    }

    if (
      Math.hypot(
        event.clientX - pointerDrag.startX,
        event.clientY - pointerDrag.startY,
      ) < 4
    ) {
      return;
    }

    event.preventDefault();
    const toolbarElement = event.currentTarget.parentElement;
    const actionElements = Array.from(
      toolbarElement?.querySelectorAll<HTMLElement>("[data-toolbar-action]") ??
        [],
    );
    // offsetLeft 不受让位 transform 影响；补上 offsetParent 的视口起点后即可和 clientX 稳定比较。
    const offsetParentLeft =
      actionElements[0]?.offsetParent?.getBoundingClientRect().left ?? 0;
    let nearestIndex = pointerDrag.dropIndex;
    let nearestDistance = Number.POSITIVE_INFINITY;
    const sourceIndex = pointerDrag.actionOrder.indexOf(pointerDrag.actionId);
    actionElements.forEach((actionElement, actionIndex) => {
      const actionCenter =
        offsetParentLeft +
        actionElement.offsetLeft +
        actionElement.offsetWidth / 2;
      const distance = Math.abs(event.clientX - actionCenter);
      const nearerToSourceOnBoundary =
        distance === nearestDistance &&
        Math.abs(actionIndex - sourceIndex) <
          Math.abs(nearestIndex - sourceIndex);
      if (distance < nearestDistance || nearerToSourceOnBoundary) {
        nearestDistance = distance;
        nearestIndex = actionIndex;
      }
    });
    pointerDrag.dropIndex = nearestIndex;
    const sourceElement = actionElements[sourceIndex];
    const targetElement = actionElements[nearestIndex];
    setDraggedToolbarAction(pointerDrag.actionId);
    setToolbarDragOffsetX(
      sourceElement === undefined || targetElement === undefined
        ? 0
        : targetElement.offsetLeft - sourceElement.offsetLeft,
    );
    setToolbarDropIndex(nearestIndex);
    if (pointerDrag.actionId === "tools") {
      closeToolMenu();
    }
  };

  /**
   * 在指针释放时把已经预览的目标槽位一次性提交，确保动画终点与持久化顺序完全一致。
   * 未达到拖动阈值时保持原顺序；真实拖动会抑制紧随其后的按钮点击。
   */
  const finishToolbarPointerDrag = (
    event: ReactPointerEvent<HTMLDivElement>,
  ) => {
    const pointerDrag = toolbarPointerDrag.current;
    if (pointerDrag === null || pointerDrag.pointerId !== event.pointerId) {
      return;
    }

    const moved =
      Math.hypot(
        event.clientX - pointerDrag.startX,
        event.clientY - pointerDrag.startY,
      ) >= 4;
    if (moved) {
      // 换序与清除让位 transform 必须在无过渡帧内同时发生，否则节点会从新槽位再多飞一个槽位。
      setToolbarSettling(true);
      commitToolbarActionOrder(
        moveToolbarActionToIndex(
          pointerDrag.actionOrder,
          pointerDrag.actionId,
          pointerDrag.dropIndex,
        ),
      );
      suppressToolbarClick.current = true;
      window.setTimeout(() => {
        suppressToolbarClick.current = false;
      }, 0);
      window.requestAnimationFrame(() => setToolbarSettling(false));
    }

    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    toolbarPointerDrag.current = null;
    setDraggedToolbarAction(null);
    setToolbarDragOffsetX(0);
    setToolbarDropIndex(null);
  };

  /**
   * 取消失去系统指针所有权的排序候选态，不根据最后坐标提交顺序。
   * 该路径覆盖窗口切换和系统手势抢占，确保下一次点击不会继承残留拖动状态。
   */
  const cancelToolbarPointerDrag = (
    event: ReactPointerEvent<HTMLDivElement>,
  ) => {
    if (toolbarPointerDrag.current?.pointerId !== event.pointerId) {
      return;
    }

    toolbarPointerDrag.current = null;
    suppressToolbarClick.current = false;
    setDraggedToolbarAction(null);
    setToolbarDragOffsetX(0);
    setToolbarDropIndex(null);
    setToolbarSettling(false);
  };

  /**
   * 处理工具栏动作上的 Alt+左右方向键，将当前动作按视觉方向移动一位并立即持久化。
   * 其他按键保持控件自身语义；到达边界时不循环，失败语义由偏好写入异常直接报告。
   */
  const moveToolbarActionByKeyboard = (
    event: ReactKeyboardEvent<HTMLDivElement>,
    actionId: ToolbarActionId,
  ) => {
    if (
      !toolbarReorderMode ||
      !event.altKey ||
      !["ArrowLeft", "ArrowRight"].includes(event.key)
    ) {
      return;
    }

    event.preventDefault();
    commitToolbarActionOrder(
      moveToolbarAction(
        toolbarActionOrder,
        actionId,
        event.key === "ArrowLeft" ? -1 : 1,
      ),
    );
  };

  /**
   * 恢复按日常抓包频率设计的默认排列，并关闭当前工具菜单以释放焦点。
   * 该动作只修改本机界面偏好，不触发录制、代理或工具状态写入。
   */
  const resetToolbarActionOrder = () => {
    commitToolbarActionOrder([...defaultToolbarActionOrder]);
    closeToolMenu();
  };

  /**
   * 切换工具栏排序模式；退出时同步释放残留拖动状态，确保下一次普通点击不会被旧手势抑制。
   * 该模式只改变本机界面编排能力，不触发任何服务端控制动作。
   */
  const toggleToolbarReorderMode = () => {
    toolbarPointerDrag.current = null;
    suppressToolbarClick.current = false;
    setDraggedToolbarAction(null);
    setToolbarDragOffsetX(0);
    setToolbarDropIndex(null);
    setToolbarSettling(false);
    setToolbarReorderMode((enabled) => !enabled);
  };

  /**
   * 刷新服务快照并为刷新控件切换专用忙碌图片；Promise 完成前拒绝重复刷新，
   * 请求失败仍由 Store 保留精确连接错误，本函数只负责解除可视忙碌状态。
   */
  const refreshSnapshot = () => {
    if (refreshPending) {
      return;
    }
    setRefreshPending(true);
    void refresh().finally(() => setRefreshPending(false));
  };

  /**
   * 使用 Radix DropdownMenu 承载工具分组、焦点循环和关闭语义。
   * 运行上下文：每个条目都调用现有路由、设置或服务控制入口；子菜单只负责分组，不创建空操作。
   */
  const toolMenu = (
    <DropdownMenu.Root
      modal={false}
      open={toolMenuOpen}
      onOpenChange={setToolMenuOpen}
    >
      <DropdownMenu.Trigger>
        <Button aria-label={t("app.menu.tools")} size="1" variant="ghost">
          <ToolbarIcon name={toolMenuOpen ? "toolsOpen" : "toolsClosed"} />
        </Button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Content align="start" sideOffset={8}>
        <DropdownMenu.Item onSelect={openSslSettings}>
          {t("ssl.open")}
        </DropdownMenu.Item>
        <DropdownMenu.Item onSelect={openProtocolSettings}>
          {t("protocolSettings.open")}
        </DropdownMenu.Item>
        <DropdownMenu.Item onSelect={openPluginPage}>
          {t("plugins.open")}
        </DropdownMenu.Item>
        <DropdownMenu.Item
          onSelect={() => {
            closeToolMenu();
            void openFloatingPanel();
          }}
        >
          {t("app.menu.window")}
        </DropdownMenu.Item>
        <DropdownMenu.Separator />
        <DropdownMenu.Item
          onSelect={() => {
            closeToolMenu();
            onOpenListenerSettings("reverseProxies");
          }}
        >
          {t("listeners.reverse.title")}
        </DropdownMenu.Item>
        <DropdownMenu.Item
          onSelect={() => {
            closeToolMenu();
            onOpenListenerSettings("portForwards");
          }}
        >
          {t("listeners.forward.title")}
        </DropdownMenu.Item>
        <DropdownMenu.Separator />
        <DropdownMenu.Item onSelect={() => openToolSettings("recordingRules")}>
          {t("tools.names.recordingRules")}
        </DropdownMenu.Item>
        <DropdownMenu.Item onSelect={() => openToolSettings("packetFilters")}>
          {t("tools.names.packetFilters")}
        </DropdownMenu.Item>
        <DropdownMenu.Sub>
          <DropdownMenu.SubTrigger>
            {t("tools.groups.interception")}
          </DropdownMenu.SubTrigger>
          <DropdownMenu.SubContent>
            {(["blockList", "noCaching", "blockCookies"] as const).map(
              (tool) => (
                <DropdownMenu.Item
                  key={tool}
                  onSelect={() => openToolSettings(tool)}
                >
                  {t(`tools.names.${tool}`)}
                </DropdownMenu.Item>
              ),
            )}
          </DropdownMenu.SubContent>
        </DropdownMenu.Sub>
        <DropdownMenu.Sub>
          <DropdownMenu.SubTrigger>
            {t("tools.groups.mapping")}
          </DropdownMenu.SubTrigger>
          <DropdownMenu.SubContent>
            {(["dnsSpoofing", "mapLocal", "mapRemote", "rewrite"] as const).map(
              (tool) => (
                <DropdownMenu.Item
                  key={tool}
                  onSelect={() => openToolSettings(tool)}
                >
                  {t(`tools.names.${tool}`)}
                </DropdownMenu.Item>
              ),
            )}
          </DropdownMenu.SubContent>
        </DropdownMenu.Sub>
        <DropdownMenu.Sub>
          <DropdownMenu.SubTrigger>
            {t("tools.groups.control")}
          </DropdownMenu.SubTrigger>
          <DropdownMenu.SubContent>
            {(["breakpoints", "throttling", "mirror", "autoSave"] as const).map(
              (tool) => (
                <DropdownMenu.Item
                  key={tool}
                  onSelect={() => openToolSettings(tool)}
                >
                  {t(`tools.names.${tool}`)}
                </DropdownMenu.Item>
              ),
            )}
            <DropdownMenu.Item onSelect={() => openToolSettings("export")}>
              {t("tools.export.action")}
            </DropdownMenu.Item>
          </DropdownMenu.SubContent>
        </DropdownMenu.Sub>
        <DropdownMenu.Separator />
        <DropdownMenu.Item onSelect={resetToolbarActionOrder}>
          {t("app.toolbar.resetOrder")}
        </DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Root>
  );

  const toolbarActionElements: Record<ToolbarActionId, ReactNode> = {
    tools: <div className="toolMenuHost">{toolMenu}</div>,
    throttling: (
      <IconButton
        size="2"
        variant="ghost"
        aria-label={t("tools.throttleToggle")}
        className={`toolQuickToggle${
          tools?.throttling.enabled ? " isEnabled" : ""
        }`}
        disabled={tools === null || toolActionPending}
        title={t("tools.throttleToggle")}
        type="button"
        onClick={toggleThrottling}
        onContextMenu={(event) => {
          event.preventDefault();
          openToolSettings("throttling");
        }}
      >
        <ToolbarIcon
          name={tools?.throttling.enabled ? "throttlingOn" : "throttlingOff"}
        />
      </IconButton>
    ),
    breakpoints: (
      <IconButton
        size="2"
        variant="ghost"
        aria-label={t("tools.breakpoints.toggle")}
        className={`toolQuickToggle breakpointToggle${
          tools?.breakpoints.enabled ? " isEnabled" : ""
        }`}
        disabled={tools === null || toolActionPending}
        title={t("tools.breakpoints.toggle")}
        type="button"
        onClick={toggleBreakpoints}
        onContextMenu={(event) => {
          event.preventDefault();
          openToolSettings("breakpoints");
        }}
      >
        <ToolbarIcon
          name={tools?.breakpoints.enabled ? "breakpointsOn" : "breakpointsOff"}
        />
        {suspendedBreakpointCount > 0 && (
          <span
            aria-label={t("tools.breakpoints.pending", {
              count: suspendedBreakpointCount,
            })}
            className="toolBadge"
          >
            {suspendedBreakpointCount}
          </span>
        )}
      </IconButton>
    ),
    recording: (
      <IconButton
        size="2"
        variant="ghost"
        className={`recordingToggle${recordingActive ? " isRecording" : ""}`}
        type="button"
        onClick={() => {
          if (!recordingActionPending) void toggleRecording();
        }}
        disabled={!controlAvailable}
        aria-busy={recordingActionPending}
        aria-disabled={!controlAvailable || recordingActionPending}
        title={t("recording.toggleLabel")}
        aria-label={t("recording.toggleLabel")}
      >
        <ToolbarIcon
          name={recordingActive ? "recordingActive" : "recordingIdle"}
        />
      </IconButton>
    ),
    refresh: (
      <IconButton
        size="2"
        variant="ghost"
        type="button"
        onClick={refreshSnapshot}
        disabled={refreshPending}
        title={t("app.toolbar.refreshSnapshot")}
        aria-label={t("app.toolbar.refreshSnapshot")}
      >
        <ToolbarIcon name={refreshPending ? "refreshBusy" : "refreshIdle"} />
      </IconButton>
    ),
    clear: (
      <IconButton
        size="2"
        variant="ghost"
        type="button"
        onClick={requestClearRecording}
        disabled={clearActionDisabled}
        title={t("recording.clear")}
        aria-label={t("recording.clear")}
      >
        <ToolbarIcon
          name={clearActionDisabled ? "clearDisabled" : "clearEnabled"}
        />
      </IconButton>
    ),
    settings: (
      <IconButton
        aria-label={t("page.settings.title")}
        className="toolbarSettingsButton"
        size="2"
        title={t("page.settings.title")}
        type="button"
        variant="ghost"
        onClick={openApplicationSettings}
      >
        <SettingsIcon aria-hidden="true" size={18} />
      </IconButton>
    ),
    processes: (
      <IconButton
        aria-label={t("app.navigation.processes")}
        size="2"
        title={t("app.navigation.processes")}
        type="button"
        variant="ghost"
        onClick={openProcessManager}
      >
        <ListFilter aria-hidden="true" size={18} />
      </IconButton>
    ),
  };
  const draggedToolbarActionIndex =
    draggedToolbarAction === null
      ? -1
      : toolbarActionOrder.indexOf(draggedToolbarAction);

  return (
    <Theme asChild>
      <header className="topToolbar">
        <div className="mainToolbarRow">
          <nav
            aria-label={t("app.navigation.label")}
            className="mainNavigation"
          >
            {navigationItems.map(
              ({ path, labelKey, inactiveIcon, activeIcon }) => (
                <NavLink
                  aria-label={t(labelKey)}
                  className={({ isActive }) =>
                    `toolbarNavItem${isActive ? " isActive" : ""}`
                  }
                  key={path}
                  title={t(labelKey)}
                  to={path}
                >
                  {({ isActive }) => (
                    <>
                      <ToolbarIcon
                        name={isActive ? activeIcon : inactiveIcon}
                      />
                      <span>{t(labelKey)}</span>
                    </>
                  )}
                </NavLink>
              ),
            )}
          </nav>
          <div
            aria-label={t("app.toolbar.actionGroupLabel")}
            className={`toolbarActions${toolbarSettling ? " isSettling" : ""}`}
            role="toolbar"
          >
            {toolbarActionOrder.map((actionId, actionIndex) => {
              const shiftedLeft =
                toolbarDropIndex !== null &&
                draggedToolbarActionIndex >= 0 &&
                draggedToolbarActionIndex < toolbarDropIndex &&
                actionIndex > draggedToolbarActionIndex &&
                actionIndex <= toolbarDropIndex;
              const shiftedRight =
                toolbarDropIndex !== null &&
                draggedToolbarActionIndex > toolbarDropIndex &&
                actionIndex >= toolbarDropIndex &&
                actionIndex < draggedToolbarActionIndex;

              return (
                <div
                  aria-description={
                    toolbarReorderMode
                      ? t("app.toolbar.reorderHint")
                      : undefined
                  }
                  className={`toolbarSortableItem${
                    draggedToolbarAction === actionId ? " isDragging" : ""
                  }${toolbarReorderMode ? " isReorderMode" : ""}${
                    shiftedLeft ? " isShiftedLeft" : ""
                  }${shiftedRight ? " isShiftedRight" : ""}`}
                  data-toolbar-action={actionId}
                  key={actionId}
                  style={
                    draggedToolbarAction === actionId
                      ? ({
                          "--toolbar-drag-offset-x": `${toolbarDragOffsetX}px`,
                        } as CSSProperties)
                      : undefined
                  }
                  onClickCapture={(event) => {
                    // 排序模式只允许编排位置，阻止松手时误触录制、清理或代理开关。
                    if (toolbarReorderMode || suppressToolbarClick.current) {
                      event.preventDefault();
                      event.stopPropagation();
                      suppressToolbarClick.current = false;
                    }
                  }}
                  onKeyDown={(event) =>
                    moveToolbarActionByKeyboard(event, actionId)
                  }
                  onPointerCancel={cancelToolbarPointerDrag}
                  onPointerDown={(event) =>
                    startToolbarPointerDrag(event, actionId)
                  }
                  onPointerMove={updateToolbarPointerDrag}
                  onPointerUp={finishToolbarPointerDrag}
                >
                  {toolbarActionElements[actionId]}
                </div>
              );
            })}
            <IconButton
              aria-label={t(
                toolbarReorderMode
                  ? "app.toolbar.finishReorder"
                  : "app.toolbar.startReorder",
              )}
              aria-pressed={toolbarReorderMode}
              className={`toolbarReorderToggle${
                toolbarReorderMode ? " isEnabled" : ""
              }`}
              size="2"
              title={t(
                toolbarReorderMode
                  ? "app.toolbar.finishReorder"
                  : "app.toolbar.startReorder",
              )}
              type="button"
              variant="ghost"
              onClick={toggleToolbarReorderMode}
            >
              <ToolbarIcon
                name={toolbarReorderMode ? "reorderOn" : "reorderOff"}
              />
            </IconButton>
          </div>
        </div>
      </header>
    </Theme>
  );
}
