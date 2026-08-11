import { useTranslation } from "react-i18next";

import {
  type AutoSaveConfiguration,
  type MirrorConfiguration,
} from "../api/protocol";
import { IntegerField } from "./integerField";
import { LocationScopeEditor } from "./toolVisualEditors";

/** 标识 M5 中使用独立字段化编辑器的持久化工具。 */
export type M5ToolId = "mirror" | "autoSave";

/** 收敛镜像与自动保存配置的联合类型，禁止把运行时公开字段误写回控制接口。 */
export type M5ToolConfiguration = MirrorConfiguration | AutoSaveConfiguration;

interface M5ToolEditorProps {
  tool: M5ToolId;
  configuration: M5ToolConfiguration;
  disabled: boolean;
  onChange(configuration: M5ToolConfiguration): void;
}

/** 判断工具标识是否由 M5 的文件化配置编辑器处理。 */
export function isM5Tool(tool: string): tool is M5ToolId {
  return tool === "mirror" || tool === "autoSave";
}

/** 渲染镜像和自动保存的字段化表单；本地路径、数值、枚举和作用域均不暴露 JSON 文本。 */
export function M5ToolEditor({
  tool,
  configuration,
  disabled,
  onChange,
}: M5ToolEditorProps) {
  const { t } = useTranslation();
  if (tool === "mirror") {
    const mirror = configuration as MirrorConfiguration;
    return (
      <div className="toolEditorFields">
        <label>
          <span>{t("tools.mirror.rootDirectory")}</span>
          <input
            disabled={disabled}
            required={mirror.enabled}
            value={mirror.rootDirectory}
            onChange={(event) =>
              onChange({ ...mirror, rootDirectory: event.target.value })
            }
          />
        </label>
        <div className="toolFormGrid">
          <label className="toolEnabledRow">
            <input
              checked={mirror.mirrorRequest}
              disabled={disabled}
              type="checkbox"
              onChange={(event) =>
                onChange({ ...mirror, mirrorRequest: event.target.checked })
              }
            />
            <span>{t("tools.mirror.request")}</span>
          </label>
          <label className="toolEnabledRow">
            <input
              checked={mirror.mirrorResponse}
              disabled={disabled}
              type="checkbox"
              onChange={(event) =>
                onChange({ ...mirror, mirrorResponse: event.target.checked })
              }
            />
            <span>{t("tools.mirror.response")}</span>
          </label>
          <label>
            <span>{t("tools.mirror.layout")}</span>
            <select
              disabled={disabled}
              value={mirror.layout}
              onChange={(event) =>
                onChange({
                  ...mirror,
                  layout: event.target.value as MirrorConfiguration["layout"],
                })
              }
            >
              <option value="hierarchical">{t("tools.mirror.hierarchical")}</option>
              <option value="flat">{t("tools.mirror.flat")}</option>
            </select>
          </label>
          <label>
            <span>{t("tools.mirror.overflow")}</span>
            <select
              disabled={disabled}
              value={mirror.onOverflow}
              onChange={(event) =>
                onChange({
                  ...mirror,
                  onOverflow: event.target.value as MirrorConfiguration["onOverflow"],
                })
              }
            >
              <option value="drop">{t("tools.mirror.drop")}</option>
              <option value="block">{t("tools.mirror.block")}</option>
            </select>
          </label>
          <IntegerField
            disabled={disabled}
            label={t("tools.mirror.queueLength")}
            max={4_096}
            min={1}
            value={mirror.maxQueueLength}
            onChange={(maxQueueLength) => onChange({ ...mirror, maxQueueLength })}
          />
        </div>
        <LocationScopeEditor
          disabled={disabled}
          locations={mirror.locations}
          onChange={(locations) => onChange({ ...mirror, locations })}
        />
      </div>
    );
  }

  const autoSave = configuration as AutoSaveConfiguration;
  return (
    <div className="toolEditorFields">
      <label>
        <span>{t("tools.autoSave.directory")}</span>
        <input
          disabled={disabled}
          required={autoSave.enabled}
          value={autoSave.directory}
          onChange={(event) =>
            onChange({ ...autoSave, directory: event.target.value })
          }
        />
      </label>
      <div className="toolFormGrid">
        <IntegerField
          disabled={disabled}
          label={t("tools.autoSave.interval")}
          max={86_400}
          min={0}
          value={autoSave.intervalSeconds}
          onChange={(intervalSeconds) => onChange({ ...autoSave, intervalSeconds })}
        />
        <IntegerField
          disabled={disabled}
          label={t("tools.autoSave.transactionCount")}
          max={100_000}
          min={0}
          value={autoSave.everyNTransactions}
          onChange={(everyNTransactions) => onChange({ ...autoSave, everyNTransactions })}
        />
        <IntegerField
          disabled={disabled}
          label={t("tools.autoSave.maxFiles")}
          max={1_000}
          min={1}
          value={autoSave.maxFiles}
          onChange={(maxFiles) => onChange({ ...autoSave, maxFiles })}
        />
        <label>
          <span>{t("tools.autoSave.format")}</span>
          <select
            disabled={disabled}
            value={autoSave.format}
            onChange={(event) =>
              onChange({
                ...autoSave,
                format: event.target.value as AutoSaveConfiguration["format"],
              })
            }
          >
            <option value="native">{t("tools.autoSave.native")}</option>
            <option value="har">HAR 1.2</option>
          </select>
        </label>
      </div>
      <label className="toolEnabledRow">
        <input
          checked={autoSave.includeBodies}
          disabled={disabled}
          type="checkbox"
          onChange={(event) =>
            onChange({ ...autoSave, includeBodies: event.target.checked })
          }
        />
        <span>{t("tools.autoSave.includeBodies")}</span>
      </label>
    </div>
  );
}
