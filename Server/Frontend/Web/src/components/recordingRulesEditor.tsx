import { ArrowDown, ArrowUp, Pencil, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  RecordingRule,
  RecordingRuleAction,
  RecordingRuleConfiguration,
  RecordingRuleKind,
  RecordingRuleSet,
} from "../api/protocol";

interface RecordingRulesEditorProps {
  configuration: RecordingRuleConfiguration;
  disabled: boolean;
  onChange(configuration: RecordingRuleConfiguration): void;
}

interface RuleDialogState {
  setId: string;
  rule: RecordingRule;
  originalRuleId: string | null;
}

const ruleKinds: readonly RecordingRuleKind[] = [
  "domain",
  "domainSuffix",
  "domainKeyword",
  "destinationIpCidr",
  "clientIpCidr",
  "port",
  "processName",
  "protocol",
  "method",
  "match",
];
const ruleActions: readonly RecordingRuleAction[] = [
  "record",
  "doNotRecord",
  "reject",
];

/** 创建只用于持久化规则主键的随机标识；浏览器不把该值作为匹配条件。 */
function createRuleIdentifier(prefix: string): string {
  return `${prefix}-${crypto.randomUUID()}`;
}

/** 返回一个可在添加窗口中直接编辑的新规则；默认记录动作避免意外阻断流量。 */
function createRule(): RecordingRule {
  return {
    id: createRuleIdentifier("rule"),
    enabled: true,
    kind: "domain",
    value: "",
    action: "record",
  };
}

/** 把数组元素移动一个位置；越界时保持原顺序，确保规则优先级可预测。 */
function moveItem<T>(items: readonly T[], index: number, offset: -1 | 1): T[] {
  const targetIndex = index + offset;
  if (targetIndex < 0 || targetIndex >= items.length) {
    return [...items];
  }
  const result = [...items];
  [result[index], result[targetIndex]] = [result[targetIndex], result[index]];
  return result;
}

/** 渲染可视化录制规则集；列表顺序就是运行时首条命中优先级。 */
export function RecordingRulesEditor({
  configuration,
  disabled,
  onChange,
}: RecordingRulesEditorProps) {
  const { t } = useTranslation();
  const [dialog, setDialog] = useState<RuleDialogState | null>(null);

  /** 以不可变更新替换指定规则集，避免污染来自服务快照的权威对象。 */
  const updateSet = (
    setId: string,
    update: (set: RecordingRuleSet) => RecordingRuleSet,
  ) => {
    onChange({
      ...configuration,
      ruleSets: configuration.ruleSets.map((set) =>
        set.id === setId ? update(set) : set,
      ),
    });
  };

  /** 添加独立规则集；空集合不会匹配流量，用户可随后通过窗口添加规则。 */
  const addSet = () => {
    const sequence = configuration.ruleSets.length + 1;
    onChange({
      ...configuration,
      ruleSets: [
        ...configuration.ruleSets,
        {
          id: createRuleIdentifier("set"),
          name: t("tools.recordingRules.defaultSetName", { sequence }),
          enabled: true,
          rules: [],
        },
      ],
    });
  };

  /** 保存添加或编辑窗口中的规则；规则 ID 保持稳定以支持持久化更新。 */
  const saveRule = () => {
    if (dialog === null) {
      return;
    }
    updateSet(dialog.setId, (set) => ({
      ...set,
      rules:
        dialog.originalRuleId === null
          ? [...set.rules, dialog.rule]
          : set.rules.map((rule) =>
              rule.id === dialog.originalRuleId ? dialog.rule : rule,
            ),
    }));
    setDialog(null);
  };

  return (
    <div className="recordingRulesEditor">
      <div className="recordingRulesSummary">
        <label>
          <span>{t("tools.recordingRules.defaultAction")}</span>
          <select
            disabled={disabled}
            value={configuration.defaultAction}
            onChange={(event) =>
              onChange({
                ...configuration,
                defaultAction: event.target.value as RecordingRuleAction,
              })
            }
          >
            {ruleActions.map((action) => (
              <option key={action} value={action}>
                {t(`tools.recordingRules.actions.${action}`)}
              </option>
            ))}
          </select>
        </label>
        <p>{t("tools.recordingRules.orderHint")}</p>
        <button disabled={disabled} type="button" onClick={addSet}>
          <Plus aria-hidden="true" size={16} />
          {t("tools.recordingRules.addSet")}
        </button>
      </div>

      <div className="recordingRuleSetList">
        {configuration.ruleSets.length === 0 ? (
          <div className="recordingRulesEmpty">
            {t("tools.recordingRules.empty")}
          </div>
        ) : (
          configuration.ruleSets.map((set, setIndex) => (
            <section className="recordingRuleSet" key={set.id}>
              <header>
                <label className="recordingRuleSetEnabled">
                  <input
                    checked={set.enabled}
                    disabled={disabled}
                    type="checkbox"
                    onChange={(event) =>
                      updateSet(set.id, (current) => ({
                        ...current,
                        enabled: event.target.checked,
                      }))
                    }
                  />
                  <input
                    aria-label={t("tools.recordingRules.setName")}
                    disabled={disabled}
                    maxLength={128}
                    required
                    value={set.name}
                    onChange={(event) =>
                      updateSet(set.id, (current) => ({
                        ...current,
                        name: event.target.value,
                      }))
                    }
                  />
                </label>
                <div className="recordingRuleActions">
                  <button
                    aria-label={t("tools.form.moveUp")}
                    disabled={disabled || setIndex === 0}
                    type="button"
                    onClick={() =>
                      onChange({
                        ...configuration,
                        ruleSets: moveItem(
                          configuration.ruleSets,
                          setIndex,
                          -1,
                        ),
                      })
                    }
                  >
                    <ArrowUp size={15} />
                  </button>
                  <button
                    aria-label={t("tools.form.moveDown")}
                    disabled={
                      disabled || setIndex === configuration.ruleSets.length - 1
                    }
                    type="button"
                    onClick={() =>
                      onChange({
                        ...configuration,
                        ruleSets: moveItem(configuration.ruleSets, setIndex, 1),
                      })
                    }
                  >
                    <ArrowDown size={15} />
                  </button>
                  <button
                    aria-label={t("tools.recordingRules.removeSet")}
                    disabled={disabled}
                    type="button"
                    onClick={() =>
                      onChange({
                        ...configuration,
                        ruleSets: configuration.ruleSets.filter(
                          (candidate) => candidate.id !== set.id,
                        ),
                      })
                    }
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              </header>

              <div className="recordingRuleRows">
                {set.rules.length === 0 ? (
                  <p>{t("tools.form.noRules")}</p>
                ) : (
                  set.rules.map((rule, ruleIndex) => (
                    <div className="recordingRuleRow" key={rule.id}>
                      <input
                        aria-label={t("tools.form.ruleEnabled")}
                        checked={rule.enabled}
                        disabled={disabled}
                        type="checkbox"
                        onChange={(event) =>
                          updateSet(set.id, (current) => ({
                            ...current,
                            rules: current.rules.map((candidate) =>
                              candidate.id === rule.id
                                ? {
                                    ...candidate,
                                    enabled: event.target.checked,
                                  }
                                : candidate,
                            ),
                          }))
                        }
                      />
                      <span className="recordingRuleKind">
                        {t(`tools.recordingRules.kinds.${rule.kind}`)}
                      </span>
                      <code>{rule.kind === "match" ? "*" : rule.value}</code>
                      <span
                        className={`recordingRuleAction recordingRuleAction--${rule.action}`}
                      >
                        {t(`tools.recordingRules.actions.${rule.action}`)}
                      </span>
                      <div className="recordingRuleActions">
                        <button
                          aria-label={t("tools.form.moveUp")}
                          disabled={disabled || ruleIndex === 0}
                          type="button"
                          onClick={() =>
                            updateSet(set.id, (current) => ({
                              ...current,
                              rules: moveItem(current.rules, ruleIndex, -1),
                            }))
                          }
                        >
                          <ArrowUp size={14} />
                        </button>
                        <button
                          aria-label={t("tools.form.moveDown")}
                          disabled={
                            disabled || ruleIndex === set.rules.length - 1
                          }
                          type="button"
                          onClick={() =>
                            updateSet(set.id, (current) => ({
                              ...current,
                              rules: moveItem(current.rules, ruleIndex, 1),
                            }))
                          }
                        >
                          <ArrowDown size={14} />
                        </button>
                        <button
                          aria-label={t("tools.form.editRule")}
                          disabled={disabled}
                          type="button"
                          onClick={() =>
                            setDialog({
                              setId: set.id,
                              rule: structuredClone(rule),
                              originalRuleId: rule.id,
                            })
                          }
                        >
                          <Pencil size={14} />
                        </button>
                        <button
                          aria-label={t("tools.form.removeRule")}
                          disabled={disabled}
                          type="button"
                          onClick={() =>
                            updateSet(set.id, (current) => ({
                              ...current,
                              rules: current.rules.filter(
                                (candidate) => candidate.id !== rule.id,
                              ),
                            }))
                          }
                        >
                          <Trash2 size={14} />
                        </button>
                      </div>
                    </div>
                  ))
                )}
              </div>
              <button
                disabled={disabled}
                type="button"
                onClick={() =>
                  setDialog({
                    setId: set.id,
                    rule: createRule(),
                    originalRuleId: null,
                  })
                }
              >
                <Plus aria-hidden="true" size={15} />
                {t("tools.form.addRule")}
              </button>
            </section>
          ))
        )}
      </div>

      {dialog !== null && (
        <div
          className="recordingRuleDialogBackdrop"
          role="presentation"
          onClick={(event) => {
            if (event.target === event.currentTarget) {
              setDialog(null);
            }
          }}
          onKeyDown={(event) => {
            // 内层规则窗口必须先消费 Escape，避免用户取消单条编辑时连同整个工具窗口一起关闭。
            event.stopPropagation();
            if (event.key === "Escape") {
              setDialog(null);
            }
          }}
        >
          <section
            aria-modal="true"
            className="recordingRuleDialog"
            role="dialog"
          >
            <header>
              <h3>{t("tools.recordingRules.ruleDialogTitle")}</h3>
            </header>
            <label>
              <span>{t("tools.recordingRules.condition")}</span>
              <select
                disabled={disabled}
                value={dialog.rule.kind}
                onChange={(event) =>
                  setDialog({
                    ...dialog,
                    rule: {
                      ...dialog.rule,
                      kind: event.target.value as RecordingRuleKind,
                      value:
                        event.target.value === "match" ? "" : dialog.rule.value,
                    },
                  })
                }
              >
                {ruleKinds.map((kind) => (
                  <option key={kind} value={kind}>
                    {t(`tools.recordingRules.kinds.${kind}`)}
                  </option>
                ))}
              </select>
            </label>
            {dialog.rule.kind !== "match" && (
              <label>
                <span>{t("tools.recordingRules.value")}</span>
                <input
                  autoFocus
                  disabled={disabled}
                  maxLength={512}
                  required
                  value={dialog.rule.value}
                  placeholder={t(
                    `tools.recordingRules.placeholders.${dialog.rule.kind}`,
                  )}
                  onChange={(event) =>
                    setDialog({
                      ...dialog,
                      rule: { ...dialog.rule, value: event.target.value },
                    })
                  }
                />
              </label>
            )}
            <label>
              <span>{t("tools.recordingRules.action")}</span>
              <select
                disabled={disabled}
                value={dialog.rule.action}
                onChange={(event) =>
                  setDialog({
                    ...dialog,
                    rule: {
                      ...dialog.rule,
                      action: event.target.value as RecordingRuleAction,
                    },
                  })
                }
              >
                {ruleActions.map((action) => (
                  <option key={action} value={action}>
                    {t(`tools.recordingRules.actions.${action}`)}
                  </option>
                ))}
              </select>
            </label>
            <p>{t("tools.recordingRules.rejectHint")}</p>
            <footer>
              <button
                disabled={disabled}
                type="button"
                onClick={() => setDialog(null)}
              >
                {t("tools.form.cancelRule")}
              </button>
              <button
                disabled={
                  disabled ||
                  (dialog.rule.kind !== "match" &&
                    dialog.rule.value.trim() === "")
                }
                type="button"
                onClick={saveRule}
              >
                {t("tools.form.saveRule")}
              </button>
            </footer>
          </section>
        </div>
      )}
    </div>
  );
}
