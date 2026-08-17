import { Settings2 } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "react-router-dom";

import type {
  PluginConfigField,
  PluginConfigurationUpdate,
  PluginDetails,
} from "../api/protocol";
import { closeCurrentManagedWindow } from "../platform/managedWindow";
import { useServiceStore } from "../state/serviceStore";
import { WindowSurface } from "./windowSurface";

type PluginFormValue = string | number | boolean;
type PluginFormDraft = Record<string, PluginFormValue>;

/**
 * 将脱敏插件详情转换为独立窗口表单草稿。
 *
 * 运行上下文：窗口首次加载及保存成功后重建草稿，秘密字段永远不从宿主回读。
 * 参数：details 是控制服务返回的单插件详情。
 * 失败语义：插件没有声明配置 Schema 时返回空对象，由页面显示无 UI 声明状态。
 */
function createConfigurationDraft(details: PluginDetails): PluginFormDraft {
  const configSchema = details.configSchema;
  if (configSchema === null) {
    return {};
  }

  const draft: PluginFormDraft = {};
  for (const [fieldName, field] of Object.entries(configSchema.properties)) {
    if (field.format === "password") {
      draft[fieldName] = "";
      continue;
    }
    const configuredValue = details.configuration[fieldName];
    if (
      typeof configuredValue === "string" ||
      typeof configuredValue === "number" ||
      typeof configuredValue === "boolean"
    ) {
      draft[fieldName] = configuredValue;
      continue;
    }
    if (field.default !== null) {
      draft[fieldName] = field.default;
      continue;
    }
    draft[fieldName] = field.type === "boolean" ? false : "";
  }
  return draft;
}

/**
 * 判断插件 UI 草稿是否缺少必填字段。
 *
 * 运行上下文：仅用于浏览器侧禁用明显无效的保存动作，宿主仍执行完整 Schema 校验。
 * 参数：details 提供字段约束，draft 是当前独立窗口输入。
 * 失败语义：缺少 Schema、字段定义或必填值时返回 true。
 */
function hasMissingRequiredField(
  details: PluginDetails,
  draft: PluginFormDraft,
): boolean {
  const configSchema = details.configSchema;
  if (configSchema === null) {
    return true;
  }

  return configSchema.required.some((fieldName) => {
    const field = configSchema.properties[fieldName];
    if (field === undefined) {
      return true;
    }
    const value = draft[fieldName];
    if (field.format === "password") {
      return (
        typeof value !== "string" ||
        (value.trim() === "" &&
          !details.configuredSecretFields.includes(fieldName))
      );
    }
    return typeof value === "string" && value.trim() === "";
  });
}

/**
 * 组装插件配置更新正文。
 *
 * 运行上下文：空秘密表示保留宿主现值；可选空文本和数字字段不写入配置文件。
 * 参数：details 提供字段语义，draft 是当前窗口草稿。
 * 失败语义：无 Schema 时返回空对象，但页面不会发送该请求。
 */
function createConfigurationUpdate(
  details: PluginDetails,
  draft: PluginFormDraft,
): PluginConfigurationUpdate {
  const configSchema = details.configSchema;
  if (configSchema === null) {
    return { configuration: {} };
  }

  const configuration: Record<string, PluginFormValue> = {};
  for (const [fieldName, field] of Object.entries(configSchema.properties)) {
    const value = draft[fieldName];
    if (field.format === "password" && value === "") {
      continue;
    }
    if (value === "" && !configSchema.required.includes(fieldName)) {
      continue;
    }
    configuration[fieldName] = value;
  }
  return { configuration };
}

/**
 * 渲染由插件配置 Schema 声明的独立 UI 窗口。
 *
 * 运行上下文：插件管理页按钮以插件 ID 打开该路由；窗口单独读取、编辑和保存当前插件配置，不共享主页面草稿。
 * 关键约束：秘密只显示“已配置”状态，窗口关闭即销毁全部表单值；插件未声明 Schema 时明确显示没有 UI，而不是回退到主页面内嵌表单。
 * 失败语义：插件不存在或读取失败时保留窗口并显示错误；保存失败时保留用户草稿供重试。
 */
export function PluginWindowPage() {
  const { t } = useTranslation();
  const { pluginId } = useParams<{ pluginId?: string }>();
  const {
    actionPending,
    getPluginDetails,
    lastError,
    updatePluginConfiguration,
  } = useServiceStore();
  const [details, setDetails] = useState<PluginDetails | null>(null);
  const [draft, setDraft] = useState<PluginFormDraft>({});
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);

  /**
   * 读取当前路由指向的插件 UI 契约。
   *
   * 运行上下文：每个插件窗口只绑定一个插件 ID，切换路由或销毁窗口会取消在途请求。
   * 失败语义：无 ID 或请求失败时进入稳定错误页，不沿用旧插件详情。
   */
  useEffect(() => {
    const abortController = new AbortController();
    setDetails(null);
    setDraft({});
    setShowAdvanced(false);
    setLoadFailed(false);
    if (pluginId === undefined || pluginId === "") {
      setLoadFailed(true);
      return () => abortController.abort();
    }
    void getPluginDetails(pluginId, abortController.signal)
      .then((nextDetails) => {
        if (abortController.signal.aborted) {
          return;
        }
        setDetails(nextDetails);
        setDraft(createConfigurationDraft(nextDetails));
      })
      .catch((error: unknown) => {
        if (
          !abortController.signal.aborted &&
          !(error instanceof DOMException && error.name === "AbortError")
        ) {
          setLoadFailed(true);
        }
      });
    return () => abortController.abort();
  }, [getPluginDetails, pluginId]);

  /**
   * 只更新当前窗口的字段草稿。
   *
   * 运行上下文：表单控件统一调用，插件之间不会共享该对象。
   * 参数：fieldName 是 Schema 字段名，value 是已归一化标量。
   * 失败语义：该同步更新不抛错，宿主在保存时校验字段集合。
   */
  const updateDraft = (fieldName: string, value: PluginFormValue) => {
    setDraft((currentDraft) => ({ ...currentDraft, [fieldName]: value }));
  };

  /**
   * 保存当前插件窗口配置。
   *
   * 运行上下文：宿主写盘并重建需要重载的运行实例，成功响应重新生成脱敏草稿。
   * 失败语义：无 Schema、必填字段缺失或请求失败时不销毁当前输入。
   */
  const saveConfiguration = async () => {
    if (
      details === null ||
      details.configSchema === null ||
      actionPending ||
      hasMissingRequiredField(details, draft)
    ) {
      return;
    }
    const nextDetails = await updatePluginConfiguration(
      details.snapshot.id,
      createConfigurationUpdate(details, draft),
    );
    if (nextDetails === null) {
      return;
    }
    setDetails(nextDetails);
    setDraft(createConfigurationDraft(nextDetails));
  };

  const configSchema = details?.configSchema ?? null;
  const visibleFields =
    configSchema === null
      ? []
      : Object.entries(configSchema.properties).filter(
          ([fieldName, field]) =>
            !field.xAdvanced ||
            showAdvanced ||
            configSchema.required.includes(fieldName),
        );
  const configurationIncomplete =
    details !== null && hasMissingRequiredField(details, draft);

  return (
    <WindowSurface>
      <main className="pluginWindowPage">
        <header className="pluginWindowHeader">
          <div>
            <span>{t("plugins.windowEyebrow")}</span>
            <h1>{details?.snapshot.name ?? t("plugins.windowTitle")}</h1>
            {details !== null && <p>{details.snapshot.id}</p>}
          </div>
          <button type="button" onClick={() => void closeCurrentManagedWindow()}>
            {t("plugins.closeWindow")}
          </button>
        </header>
        {details === null && !loadFailed && (
          <p className="viewerNotice">{t("plugins.loading")}</p>
        )}
        {loadFailed && (
          <p className="viewerNotice viewerNotice--error">
            {t("plugins.loadFailed")}
          </p>
        )}
        {details !== null && (
          <section
            className="pluginConfiguration pluginWindowConfiguration"
            aria-label={t("plugins.configuration")}
          >
            <header>
              <Settings2 aria-hidden="true" size={16} />
              <div>
                <h2>{configSchema?.title || t("plugins.configuration")}</h2>
                {configSchema !== null && configSchema.description !== "" && (
                  <p>{configSchema.description}</p>
                )}
              </div>
            </header>
            {configSchema === null ? (
              <p className="pluginEmptyState">{t("plugins.noWindowUi")}</p>
            ) : (
              <>
                {Object.values(configSchema.properties).some(
                  (field) => field.xAdvanced,
                ) && (
                  <label className="pluginAdvancedToggle">
                    <input
                      checked={showAdvanced}
                      disabled={actionPending}
                      type="checkbox"
                      onChange={(event) =>
                        setShowAdvanced(event.target.checked)
                      }
                    />
                    {t("plugins.advanced")}
                  </label>
                )}
                <div className="pluginConfigurationFields">
                  {visibleFields.map(([fieldName, field]) => (
                    <PluginConfigurationField
                      configuredSecret={details.configuredSecretFields.includes(
                        fieldName,
                      )}
                      disabled={actionPending}
                      field={field}
                      fieldName={fieldName}
                      key={fieldName}
                      required={configSchema.required.includes(fieldName)}
                      value={draft[fieldName]}
                      onChange={updateDraft}
                    />
                  ))}
                </div>
                {lastError !== null && (
                  <p className="viewerNotice viewerNotice--error">{lastError}</p>
                )}
                <div className="pluginConfigurationActions">
                  <button
                    className="primaryButton"
                    disabled={actionPending || configurationIncomplete}
                    type="button"
                    onClick={() => void saveConfiguration()}
                  >
                    {t("plugins.saveConfiguration")}
                  </button>
                </div>
              </>
            )}
          </section>
        )}
      </main>
    </WindowSurface>
  );
}

interface PluginConfigurationFieldProps {
  fieldName: string;
  field: PluginConfigField;
  value: PluginFormValue | undefined;
  required: boolean;
  configuredSecret: boolean;
  disabled: boolean;
  onChange(fieldName: string, value: PluginFormValue): void;
}

/**
 * 渲染插件独立窗口中的单个声明式字段。
 *
 * 运行上下文：字段只允许宿主支持的标量类型，密码输入不回显已保存内容。
 * 参数：field 描述类型和范围，value 是窗口草稿，onChange 回传归一化标量。
 * 失败语义：无效数字文本保留为空，由必填状态和宿主校验阻止提交。
 */
function PluginConfigurationField({
  fieldName,
  field,
  value,
  required,
  configuredSecret,
  disabled,
  onChange,
}: PluginConfigurationFieldProps) {
  const { t } = useTranslation();
  const inputId = useId();
  const hintId = useId();
  const title = field.title === "" ? fieldName : field.title;
  const description = field.description;
  const isPassword = field.format === "password";
  const hasEnum = field.enum.length > 0;
  const stringValue = typeof value === "string" ? value : String(value ?? "");

  /**
   * 将数字输入转换为有限标量。
   *
   * 运行上下文：浏览器 number 输入仍以字符串触发事件，需要在写入草稿前归一化。
   * 参数：nextValue 是当前输入文本。
   * 失败语义：空文本保留为空，非有限数不更新草稿。
   */
  const updateNumberValue = (nextValue: string) => {
    if (nextValue === "") {
      onChange(fieldName, "");
      return;
    }
    const numberValue = Number(nextValue);
    if (Number.isFinite(numberValue)) {
      onChange(fieldName, numberValue);
    }
  };

  return (
    <label className="pluginConfigurationField" htmlFor={inputId}>
      <span>
        <strong>{title}</strong>
        {required && <em>{t("plugins.required")}</em>}
      </span>
      {hasEnum ? (
        <select
          aria-describedby={description === "" ? undefined : hintId}
          disabled={disabled}
          id={inputId}
          value={stringValue}
          onChange={(event) => {
            const selectedValue = event.target.value;
            const enumValue = field.enum.find(
              (candidate) => String(candidate) === selectedValue,
            );
            if (enumValue !== undefined) {
              onChange(fieldName, enumValue);
            }
          }}
        >
          {field.enum.map((enumValue) => (
            <option key={String(enumValue)} value={String(enumValue)}>
              {String(enumValue)}
            </option>
          ))}
        </select>
      ) : field.type === "boolean" ? (
        <input
          checked={value === true}
          disabled={disabled}
          id={inputId}
          type="checkbox"
          onChange={(event) => onChange(fieldName, event.target.checked)}
        />
      ) : (
        <input
          aria-describedby={description === "" ? undefined : hintId}
          disabled={disabled}
          id={inputId}
          max={field.maximum ?? undefined}
          maxLength={field.maxLength ?? undefined}
          min={field.minimum ?? undefined}
          minLength={field.minLength ?? undefined}
          placeholder={isPassword ? t("plugins.secretConfigured") : undefined}
          step={field.type === "integer" ? 1 : "any"}
          type={
            isPassword
              ? "password"
              : field.type === "number" || field.type === "integer"
                ? "number"
                : "text"
          }
          value={stringValue}
          onChange={(event) =>
            field.type === "number" || field.type === "integer"
              ? updateNumberValue(event.target.value)
              : onChange(fieldName, event.target.value)
          }
        />
      )}
      {description !== "" && <small id={hintId}>{description}</small>}
      {isPassword && configuredSecret && (
        <small>{t("plugins.secretConfigured")}</small>
      )}
    </label>
  );
}
