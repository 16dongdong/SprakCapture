import { ChevronDown, ChevronUp, Plus, Trash2 } from "lucide-react";

interface RuleListProps<Item> {
  title: string;
  emptyHint: string;
  items: readonly Item[];
  selectedIndex: number;
  disabled: boolean;
  addLabel: string;
  removeLabel: string;
  moveUpLabel: string;
  moveDownLabel: string;
  itemLabel(index: number, item: Item): string;
  onSelect(index: number): void;
  onAdd(): void;
  onRemove(index: number): void;
  onMove(fromIndex: number, toIndex: number): void;
}

/**
 * 移动规则数组中的一个条目并返回新数组。
 * 规则顺序直接决定匹配优先级；越界移动返回原顺序的副本，调用方无需额外构造防御分支。
 */
export function moveListItem<Item>(
  items: readonly Item[],
  fromIndex: number,
  toIndex: number,
): Item[] {
  if (
    fromIndex < 0 ||
    fromIndex >= items.length ||
    toIndex < 0 ||
    toIndex >= items.length ||
    fromIndex === toIndex
  ) {
    return [...items];
  }
  const nextItems = [...items];
  const [item] = nextItems.splice(fromIndex, 1);
  nextItems.splice(toIndex, 0, item as Item);
  return nextItems;
}

/**
 * 渲染规则、作用域和规则集共用的列表编辑器。
 * 使用原生列表与普通按钮表达选择和顺序操作，避免不完整 listbox 语义影响键盘及辅助技术导航。
 */
export function RuleList<Item>({
  title,
  emptyHint,
  items,
  selectedIndex,
  disabled,
  addLabel,
  removeLabel,
  moveUpLabel,
  moveDownLabel,
  itemLabel,
  onSelect,
  onAdd,
  onRemove,
  onMove,
}: RuleListProps<Item>) {
  return (
    <aside aria-label={title} className="toolRuleListPane">
      <header className="toolRuleListHeader">
        <strong>{title}</strong>
        <button disabled={disabled} type="button" onClick={onAdd}>
          <Plus aria-hidden="true" size={14} />
          {addLabel}
        </button>
      </header>
      {items.length === 0 ? (
        <p className="toolRuleListEmpty">{emptyHint}</p>
      ) : (
        <ul className="toolRuleList">
          {items.map((item, index) => (
            <li className="toolRuleListItem" key={`${index}-${itemLabel(index, item)}`}>
              <button
                aria-pressed={selectedIndex === index}
                className={selectedIndex === index ? "isSelected" : ""}
                disabled={disabled}
                type="button"
                onClick={() => onSelect(index)}
              >
                <span>{itemLabel(index, item)}</span>
              </button>
              <div className="toolRuleOrderControls">
                <button
                  aria-label={`${moveUpLabel} ${index + 1}`}
                  className="iconButton toolRuleMoveButton"
                  disabled={disabled || index === 0}
                  type="button"
                  onClick={() => onMove(index, index - 1)}
                >
                  <ChevronUp aria-hidden="true" size={14} />
                </button>
                <button
                  aria-label={`${moveDownLabel} ${index + 1}`}
                  className="iconButton toolRuleMoveButton"
                  disabled={disabled || index === items.length - 1}
                  type="button"
                  onClick={() => onMove(index, index + 1)}
                >
                  <ChevronDown aria-hidden="true" size={14} />
                </button>
                <button
                  aria-label={`${removeLabel} ${index + 1}`}
                  className="iconButton toolRuleDeleteButton"
                  disabled={disabled}
                  type="button"
                  onClick={() => onRemove(index)}
                >
                  <Trash2 aria-hidden="true" size={14} />
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}
