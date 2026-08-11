import { useEffect, useState } from "react";

interface IntegerFieldProps {
  label: string;
  value: number | null;
  min: number;
  max?: number;
  disabled: boolean;
  className?: string;
  wrap?: boolean;
  allowEmpty?: boolean;
  onChange(value: number): void;
  onEmpty?(): void;
}

/**
 * 渲染支持临时空值的受控整数输入。
 * 数字配置在用户清空并重新输入时保留本地文本草稿，只在值满足边界后回写父级，避免旧数值与新输入拼接。
 */
export function IntegerField({
  label,
  value,
  min,
  max,
  disabled,
  className,
  wrap = true,
  allowEmpty = false,
  onChange,
  onEmpty,
}: IntegerFieldProps) {
  const [draftValue, setDraftValue] = useState(() => (value === null ? "" : String(value)));

  useEffect(() => {
    setDraftValue(value === null ? "" : String(value));
  }, [value]);

  /** 将满足整数和边界约束的草稿写回父级；可空字段在清空时显式回写 null，其余字段等待用户完成输入。 */
  const updateDraftValue = (nextValue: string) => {
    setDraftValue(nextValue);
    if (nextValue === "") {
      if (allowEmpty) {
        onEmpty?.();
      }
      return;
    }
    const parsed = Number(nextValue);
    if (
      Number.isInteger(parsed) &&
      parsed >= min &&
      (max === undefined || parsed <= max)
    ) {
      onChange(parsed);
    }
  };

  const input = (
    <input
      aria-label={label}
      disabled={disabled}
      inputMode="numeric"
      max={max}
      min={min}
      required={!allowEmpty}
      step={1}
      type="number"
      value={draftValue}
      onChange={(event) => updateDraftValue(event.target.value)}
    />
  );

  if (!wrap) {
    return input;
  }

  return (
    <label className={className}>
      <span>{label}</span>
      {input}
    </label>
  );
}
