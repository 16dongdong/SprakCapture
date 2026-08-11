interface TreeToggleProps {
  expanded: boolean;
  label: string;
  onToggle(): void;
}

/**
 * 渲染树节点唯一的展开控件；按钮只改变子节点可见性，节点文本仍负责选择会话、方向或单包。
 *
 * 运行上下文：HTTP 路径树和原始流树复用同一语义，保证键盘与屏幕阅读器的展开描述一致。
 * 参数：expanded 为当前状态；label 用于生成无障碍名称；onToggle 回写所属节点状态。
 * 失败语义：不处理业务选择，也不触发正文读取。
 */
export function TreeToggle({
  expanded,
  label,
  onToggle,
}: TreeToggleProps) {
  return (
    <button
      aria-expanded={expanded}
      aria-label={`${expanded ? "收缩" : "展开"} ${label}`}
      className="treeToggle"
      onClick={onToggle}
      type="button"
    >
      {expanded ? "−" : "+"}
    </button>
  );
}
