import { ArrowDown, ArrowUp, Pencil, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import type {
  PacketFilterAction,
  PacketFilterConfiguration,
  PacketFilterDirection,
  PacketFilterRule,
  PacketFilterTransport,
} from "../api/protocol";
import {
  isPacketByteGridValueValid,
  PacketFilterByteGrid,
} from "./packetFilterByteGrid";
import { RuleEditorDialog } from "./ruleEditorDialog";

interface PacketFiltersEditorProps {
  configuration: PacketFilterConfiguration;
  disabled: boolean;
  onChange(configuration: PacketFilterConfiguration): void;
  onApply?(configuration: PacketFilterConfiguration): Promise<boolean>;
}

const transports: readonly PacketFilterTransport[] = ["any", "tcp", "udp"];
const directions: readonly PacketFilterDirection[] = ["any", "up", "down"];
const actions: readonly PacketFilterAction[] = ["modify", "drop", "close"];

/** 创建一条只匹配客户端上行 TCP 的透明草稿；提交前由编辑器补齐名称。 */
function createRule(): PacketFilterRule {
  return {
    id: crypto.randomUUID(),
    name: "",
    enabled: true,
    transport: "tcp",
    direction: "up",
    host: "",
    port: null,
    minimumLength: null,
    maximumLength: null,
    pattern: "",
    replacement: "",
    action: "modify",
    replaceAll: false,
    continueMatching: false,
  };
}

/** 把空数值输入转换为可选整数；原生 min/max 负责拒绝越界或小数。 */
function optionalNumber(value: string): number | null {
  return value === "" ? null : Number(value);
}

/** 渲染封包滤镜的有序列表和独立规则编辑窗口；草稿取消时不会修改外层配置。 */
export function PacketFiltersEditor({
  configuration,
  disabled,
  onChange,
  onApply,
}: PacketFiltersEditorProps) {
  const { t } = useTranslation();
  const [dialog, setDialog] = useState<{
    index: number | null;
    rule: PacketFilterRule;
  } | null>(null);

  /** 原子替换指定规则；所有其它规则与执行顺序保持不变。 */
  const updateRule = (
    index: number,
    update: (rule: PacketFilterRule) => PacketFilterRule,
  ) => {
    onChange({
      ...configuration,
      rules: configuration.rules.map((rule, ruleIndex) =>
        ruleIndex === index ? update(rule) : rule,
      ),
    });
  };

  /** 在相邻位置间移动规则；越界请求保持配置不变。 */
  const moveRule = (index: number, offset: -1 | 1) => {
    const destination = index + offset;
    if (destination < 0 || destination >= configuration.rules.length) {
      return;
    }
    const rules = [...configuration.rules];
    [rules[index], rules[destination]] = [rules[destination], rules[index]];
    onChange({ ...configuration, rules });
  };

  /**
   * 保存二级窗口中的完整规则，并在宿主提供提交动作时立即热应用整份配置。
   * 网络提交失败时保留窗口和草稿，成功后才关闭，避免“规则已保存”的错误反馈或要求用户再点一次外层应用。
   */
  const saveDialogRule = async () => {
    if (dialog === null) {
      return;
    }
    const normalizedRule = {
      ...dialog.rule,
      name: dialog.rule.name.trim(),
      host: dialog.rule.host.trim(),
      pattern: dialog.rule.pattern.trim(),
      replacement:
        dialog.rule.action === "modify"
          ? dialog.rule.replacement.trim()
          : "",
    };
    const rules = [...configuration.rules];
    if (dialog.index === null) {
      rules.push(normalizedRule);
    } else {
      rules[dialog.index] = normalizedRule;
    }
    const nextConfiguration = { ...configuration, rules };
    onChange(nextConfiguration);
    if (onApply !== undefined && !(await onApply(nextConfiguration))) {
      return;
    }
    setDialog(null);
  };

  return (
    <div className="packetFiltersEditor">
      <div className="packetFiltersSummary">
        <p>{t("tools.packetFilters.orderHint")}</p>
        <button
          disabled={disabled || configuration.rules.length >= 256}
          type="button"
          onClick={() => setDialog({ index: null, rule: createRule() })}
        >
          <Plus aria-hidden="true" size={16} />
          {t("tools.packetFilters.addRule")}
        </button>
      </div>

      <div className="packetFilterRuleList">
        {configuration.rules.length === 0 ? (
          <div className="packetFiltersEmpty">
            {t("tools.packetFilters.empty")}
          </div>
        ) : (
          configuration.rules.map((rule, index) => (
            <article className="packetFilterRule" key={rule.id}>
              <label className="packetFilterRuleEnabled">
                <input
                  checked={rule.enabled}
                  disabled={disabled}
                  type="checkbox"
                  onChange={(event) =>
                    updateRule(index, (current) => ({
                      ...current,
                      enabled: event.target.checked,
                    }))
                  }
                />
                <span>
                  <strong>{rule.name}</strong>
                  <small>
                    {t(`tools.packetFilters.transports.${rule.transport}`)} ·{" "}
                    {t(`tools.packetFilters.directions.${rule.direction}`)} ·{" "}
                    {rule.host || t("tools.packetFilters.anyHost")}
                    {rule.port === null ? "" : `:${rule.port}`}
                  </small>
                </span>
              </label>
              <code>{rule.pattern || t("tools.packetFilters.anyBytes")}</code>
              <span
                className={`packetFilterAction packetFilterAction--${rule.action}`}
              >
                {t(`tools.packetFilters.actions.${rule.action}`)}
              </span>
              <div className="packetFilterRuleActions">
                <button
                  aria-label={t("tools.moveUp")}
                  disabled={disabled || index === 0}
                  type="button"
                  onClick={() => moveRule(index, -1)}
                >
                  <ArrowUp aria-hidden="true" size={15} />
                </button>
                <button
                  aria-label={t("tools.moveDown")}
                  disabled={
                    disabled || index === configuration.rules.length - 1
                  }
                  type="button"
                  onClick={() => moveRule(index, 1)}
                >
                  <ArrowDown aria-hidden="true" size={15} />
                </button>
                <button
                  aria-label={t("tools.form.editRule")}
                  disabled={disabled}
                  type="button"
                  onClick={() =>
                    setDialog({ index, rule: structuredClone(rule) })
                  }
                >
                  <Pencil aria-hidden="true" size={15} />
                </button>
                <button
                  aria-label={t("tools.form.removeRule")}
                  disabled={disabled}
                  type="button"
                  onClick={() =>
                    onChange({
                      ...configuration,
                      rules: configuration.rules.filter(
                        (_candidate, ruleIndex) => ruleIndex !== index,
                      ),
                    })
                  }
                >
                  <Trash2 aria-hidden="true" size={15} />
                </button>
              </div>
            </article>
          ))
        )}
      </div>

      <RuleEditorDialog
        cancelLabel={t("tools.cancel")}
        confirmLabel={t("tools.apply")}
        disabled={disabled}
        confirmDisabled={
          dialog !== null &&
          (!isPacketByteGridValueValid(dialog.rule.pattern) ||
            (dialog.rule.action === "modify" &&
              (dialog.rule.pattern.trim() === "" ||
                dialog.rule.replacement.trim() === "" ||
                !isPacketByteGridValueValid(dialog.rule.replacement))))
        }
        open={dialog !== null}
        title={t("tools.packetFilters.ruleDialogTitle")}
        onCancel={() => setDialog(null)}
        onConfirm={saveDialogRule}
      >
        {dialog !== null && (
          <div className="packetFilterRuleForm">
            <label>
              <span>{t("tools.packetFilters.name")}</span>
              <input
                required
                maxLength={128}
                value={dialog.rule.name}
                onChange={(event) =>
                  setDialog({
                    ...dialog,
                    rule: { ...dialog.rule, name: event.target.value },
                  })
                }
              />
            </label>
            <div className="packetFilterFormGrid">
              <label>
                <span>{t("tools.packetFilters.transport")}</span>
                <select
                  value={dialog.rule.transport}
                  onChange={(event) =>
                    setDialog({
                      ...dialog,
                      rule: {
                        ...dialog.rule,
                        transport: event.target.value as PacketFilterTransport,
                      },
                    })
                  }
                >
                  {transports.map((transport) => (
                    <option key={transport} value={transport}>
                      {t(`tools.packetFilters.transports.${transport}`)}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                <span>{t("tools.packetFilters.direction")}</span>
                <select
                  value={dialog.rule.direction}
                  onChange={(event) =>
                    setDialog({
                      ...dialog,
                      rule: {
                        ...dialog.rule,
                        direction: event.target.value as PacketFilterDirection,
                      },
                    })
                  }
                >
                  {directions.map((direction) => (
                    <option key={direction} value={direction}>
                      {t(`tools.packetFilters.directions.${direction}`)}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                <span>{t("tools.packetFilters.host")}</span>
                <input
                  maxLength={253}
                  placeholder="*.example.com"
                  value={dialog.rule.host}
                  onChange={(event) =>
                    setDialog({
                      ...dialog,
                      rule: { ...dialog.rule, host: event.target.value },
                    })
                  }
                />
              </label>
              <label>
                <span>{t("tools.packetFilters.port")}</span>
                <input
                  max={65_535}
                  min={1}
                  placeholder={t("tools.packetFilters.anyValue")}
                  type="number"
                  value={dialog.rule.port ?? ""}
                  onChange={(event) =>
                    setDialog({
                      ...dialog,
                      rule: {
                        ...dialog.rule,
                        port: optionalNumber(event.target.value),
                      },
                    })
                  }
                />
              </label>
              <label>
                <span>{t("tools.packetFilters.minimumLength")}</span>
                <input
                  max={16 * 1024 * 1024}
                  min={1}
                  placeholder={t("tools.packetFilters.anyValue")}
                  type="number"
                  value={dialog.rule.minimumLength ?? ""}
                  onChange={(event) =>
                    setDialog({
                      ...dialog,
                      rule: {
                        ...dialog.rule,
                        minimumLength: optionalNumber(event.target.value),
                      },
                    })
                  }
                />
              </label>
              <label>
                <span>{t("tools.packetFilters.maximumLength")}</span>
                <input
                  max={16 * 1024 * 1024}
                  min={1}
                  placeholder={t("tools.packetFilters.anyValue")}
                  type="number"
                  value={dialog.rule.maximumLength ?? ""}
                  onChange={(event) =>
                    setDialog({
                      ...dialog,
                      rule: {
                        ...dialog.rule,
                        maximumLength: optionalNumber(event.target.value),
                      },
                    })
                  }
                />
              </label>
            </div>
            <PacketFilterByteGrid
              disabled={disabled}
              pattern={dialog.rule.pattern}
              replacement={
                dialog.rule.action === "modify" ? dialog.rule.replacement : null
              }
              onPatternChange={(pattern) =>
                setDialog((current) =>
                  current === null
                    ? current
                    : {
                        ...current,
                        rule: { ...current.rule, pattern },
                      },
                )
              }
              onReplacementChange={(replacement) =>
                setDialog((current) =>
                  current === null
                    ? current
                    : {
                        ...current,
                        rule: { ...current.rule, replacement },
                      },
                )
              }
            />
            <label>
              <span>{t("tools.packetFilters.action")}</span>
              <select
                value={dialog.rule.action}
                onChange={(event) => {
                  const action = event.target.value as PacketFilterAction;
                  setDialog({
                    ...dialog,
                    rule: {
                      ...dialog.rule,
                      action,
                      replacement:
                        action === "modify" ? dialog.rule.replacement : "",
                    },
                  });
                }}
              >
                {actions.map((action) => (
                  <option key={action} value={action}>
                    {t(`tools.packetFilters.actions.${action}`)}
                  </option>
                ))}
              </select>
            </label>
            <label
              className="packetFilterCheckbox"
              htmlFor="packetFilterReplaceAll"
            >
              <input
                id="packetFilterReplaceAll"
                checked={dialog.rule.replaceAll}
                type="checkbox"
                onChange={(event) =>
                  setDialog({
                    ...dialog,
                    rule: {
                      ...dialog.rule,
                      replaceAll: event.target.checked,
                    },
                  })
                }
              />
              <span>{t("tools.packetFilters.replaceAll")}</span>
            </label>
            <label
              className="packetFilterCheckbox"
              htmlFor="packetFilterContinueMatching"
            >
              <input
                id="packetFilterContinueMatching"
                checked={dialog.rule.continueMatching}
                type="checkbox"
                onChange={(event) =>
                  setDialog({
                    ...dialog,
                    rule: {
                      ...dialog.rule,
                      continueMatching: event.target.checked,
                    },
                  })
                }
              />
              <span>{t("tools.packetFilters.continueMatching")}</span>
            </label>
          </div>
        )}
      </RuleEditorDialog>
    </div>
  );
}
