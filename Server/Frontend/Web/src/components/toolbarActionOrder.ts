export const defaultToolbarActionOrder = [
  "recording",
  "clear",
  "refresh",
  "breakpoints",
  "throttling",
  "tools",
  "processes",
  "settings",
] as const;

export type ToolbarActionId = (typeof defaultToolbarActionOrder)[number];

const toolbarActionOrderStorageKey = "capture.toolbar.actionOrder";
const toolbarActionIds = new Set<string>(defaultToolbarActionOrder);

/**
 * 校验浏览器偏好中的工具栏动作标识，避免旧版本或手工修改的值进入渲染顺序。
 * 参数为待校验值；失败时返回 false，调用方必须忽略该值。
 */
export function isToolbarActionId(value: unknown): value is ToolbarActionId {
  return typeof value === "string" && toolbarActionIds.has(value);
}

/**
 * 读取当前浏览器保存的工具栏顺序；持久化内容必须是默认动作集合的完整排列。
 * 存储缺失时返回按使用频率设计的默认顺序；内容损坏时删除无效偏好后返回默认顺序。
 */
export function readToolbarActionOrder(): ToolbarActionId[] {
  const serializedOrder = window.localStorage.getItem(
    toolbarActionOrderStorageKey,
  );
  if (serializedOrder === null) {
    return [...defaultToolbarActionOrder];
  }

  try {
    const parsedOrder: unknown = JSON.parse(serializedOrder);
    if (
      Array.isArray(parsedOrder) &&
      parsedOrder.length === defaultToolbarActionOrder.length &&
      parsedOrder.every(isToolbarActionId) &&
      new Set(parsedOrder).size === defaultToolbarActionOrder.length
    ) {
      return parsedOrder;
    }
  } catch {
    // 工具栏偏好不是业务数据；损坏时只清除该键，防止整个工作区因本地 JSON 失效而停止渲染。
  }

  window.localStorage.removeItem(toolbarActionOrderStorageKey);
  return [...defaultToolbarActionOrder];
}

/**
 * 原子保存完整工具栏顺序，供刷新页面后的界面恢复使用。
 * 参数必须包含全部动作且无重复；浏览器拒绝写入时由 Storage API 将失败直接报告给调用方。
 */
export function persistToolbarActionOrder(
  actionOrder: readonly ToolbarActionId[],
): void {
  window.localStorage.setItem(
    toolbarActionOrderStorageKey,
    JSON.stringify(actionOrder),
  );
}

/**
 * 将一个工具栏动作移动到目标动作前后，保持动作集合完整且不产生重复入口。
 * sourceId 与 targetId 相同时返回原顺序；目标不存在时返回原顺序，防止拖放边界破坏偏好。
 */
export function reorderToolbarAction(
  actionOrder: readonly ToolbarActionId[],
  sourceId: ToolbarActionId,
  targetId: ToolbarActionId,
  placeAfterTarget: boolean,
): ToolbarActionId[] {
  if (sourceId === targetId) {
    return [...actionOrder];
  }

  const nextOrder = actionOrder.filter((actionId) => actionId !== sourceId);
  const targetIndex = nextOrder.indexOf(targetId);
  if (targetIndex < 0) {
    return [...actionOrder];
  }

  nextOrder.splice(targetIndex + (placeAfterTarget ? 1 : 0), 0, sourceId);
  return nextOrder;
}

/**
 * 按键盘方向移动单个工具栏动作，供 Alt+左右方向键实现与拖放等价的无障碍排序。
 * 到达边界时返回原顺序，不循环跳转，避免用户失去对当前位置的判断。
 */
export function moveToolbarAction(
  actionOrder: readonly ToolbarActionId[],
  actionId: ToolbarActionId,
  offset: -1 | 1,
): ToolbarActionId[] {
  const sourceIndex = actionOrder.indexOf(actionId);
  const targetIndex = sourceIndex + offset;
  if (sourceIndex < 0 || targetIndex < 0 || targetIndex >= actionOrder.length) {
    return [...actionOrder];
  }

  const nextOrder = [...actionOrder];
  [nextOrder[sourceIndex], nextOrder[targetIndex]] = [
    nextOrder[targetIndex],
    nextOrder[sourceIndex],
  ];
  return nextOrder;
}

/**
 * 将指定动作移动到确定的槽位索引，供指针拖动预览在松手时一次性提交最终排列。
 *
 * 运行上下文：拖动期间 DOM 顺序保持不变，相邻动作只通过位移动画让出目标槽位；松手后才调用本函数。
 * 参数：actionOrder 为完整动作序列，actionId 为被拖动作，targetIndex 为已经限制在序列范围内的槽位。
 * 失败语义：动作不存在或目标索引越界时直接返回原顺序副本，不破坏已持久化偏好。
 */
export function moveToolbarActionToIndex(
  actionOrder: readonly ToolbarActionId[],
  actionId: ToolbarActionId,
  targetIndex: number,
): ToolbarActionId[] {
  const sourceIndex = actionOrder.indexOf(actionId);
  if (
    sourceIndex < 0 ||
    targetIndex < 0 ||
    targetIndex >= actionOrder.length ||
    sourceIndex === targetIndex
  ) {
    return [...actionOrder];
  }

  const nextOrder = [...actionOrder];
  nextOrder.splice(sourceIndex, 1);
  nextOrder.splice(targetIndex, 0, actionId);
  return nextOrder;
}
