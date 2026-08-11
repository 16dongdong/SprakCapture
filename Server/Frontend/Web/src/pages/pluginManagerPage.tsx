import {
  PackagePlus,
  PlugZap,
  RefreshCcw,
  Settings2,
  Trash2,
} from "lucide-react";
import {
  type ChangeEvent,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import {
  type PluginConfigField,
  type PluginConfigurationUpdate,
  type PluginDetails,
  type PluginSnapshot,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";
import { showIndependentWindow } from "../platform/independentWindowContract";

type PluginFormValue = string | number | boolean;
type PluginFormDraft = Record<string, PluginFormValue>;
const emptyPluginSnapshots: PluginSnapshot[] = [];

/**
 * 将后端脱敏后的配置详情转换为表单草稿。
 *
 * 运行上下文：每次切换插件或保存成功后重建草稿，密码字段始终保留为空，避免旧秘密进入浏览器内存。
 * 参数：details 是宿主返回的单插件详情。
 * 失败语义：没有声明配置 Schema 时返回空对象，调用方以只读状态呈现。
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
 * 判断配置草稿是否缺少必填字段。
 *
 * 运行上下文：保存按钮在浏览器侧预先阻止明显不完整的提交，宿主仍是全部字段约束的权威校验方。
 * 参数：details 描述字段规则，draft 是用户当前输入。
 * 失败语义：找不到 Schema 或必填字段的可用值时返回 true。
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
 * 组装配置更新正文并省略未变更的密码字段。
 *
 * 运行上下文：空密码表示保留宿主中已有的值；可选空文本和数字字段则不写入配置文件。
 * 参数：details 提供字段语义，draft 为当前表单草稿。
 * 失败语义：无 Schema 时返回空对象，调用方不会发送该请求。
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
 * 格式化插件钩子列表；空数组使用本地化占位语义而不是留出难以理解的空白。
 */
function formatHooks(plugin: PluginSnapshot, emptyLabel: string): string {
  return plugin.hooks.length === 0 ? emptyLabel : plugin.hooks.join(", ");
}

/**
 * 渲染 Native 插件完整生命周期与声明式配置管理页面。
 *
 * 运行上下文：该页面与服务设置共享主窗口路由，插件热路径仍保留在宿主进程；界面只通过控制面串行化安装、启停、重载、配置和卸载动作。
 * 失败语义：服务端请求失败由全局状态栏呈现；列表和已选详情保留最近一次权威结果，离开路由时中止尚未完成的读取。
 */
export function PluginManagerPage() {
  const { t } = useTranslation();
  const {
    actionPending,
    getPluginDetails,
    installPluginPackage,
    refresh,
    reloadPlugin,
    setPluginEnabled,
    snapshot,
    updatePluginConfiguration,
  } = useServiceStore();
  const [selectedPluginId, setSelectedPluginId] = useState<string | null>(null);
  const [details, setDetails] = useState<PluginDetails | null>(null);
  const [detailsLoading, setDetailsLoading] = useState(false);
  const [detailsLoadFailed, setDetailsLoadFailed] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [draft, setDraft] = useState<PluginFormDraft>({});
  const detailsRequestSequence = useRef(0);
  const packageInputReference = useRef<HTMLInputElement>(null);
  const plugins = snapshot?.plugins ?? emptyPluginSnapshots;

  /** 按需读取已选插件详情；每次成功读取都重建草稿并隐藏上次插件的高级字段状态。 */
  const loadPluginDetails = useCallback(
    async (
      pluginId: string,
      signal?: AbortSignal,
    ): Promise<PluginDetails | null> => {
      const sequence = detailsRequestSequence.current + 1;
      detailsRequestSequence.current = sequence;
      setDetailsLoading(true);
      setDetailsLoadFailed(false);
      try {
        const nextDetails = await getPluginDetails(pluginId, signal);
        if (detailsRequestSequence.current === sequence) {
          setDetails(nextDetails);
          setDraft(createConfigurationDraft(nextDetails));
          setShowAdvanced(false);
        }
        return nextDetails;
      } catch (error) {
        if (
          detailsRequestSequence.current === sequence &&
          !(error instanceof DOMException && error.name === "AbortError")
        ) {
          setDetails(null);
          setDetailsLoadFailed(true);
        }
        return null;
      } finally {
        if (detailsRequestSequence.current === sequence) {
          setDetailsLoading(false);
        }
      }
    },
    [getPluginDetails],
  );

  /** 插件列表由 SSE 权威快照驱动；安装或卸载后只调整选择，不发起第二次列表 GET。 */
  useEffect(() => {
    setSelectedPluginId((currentPluginId) =>
      currentPluginId !== null &&
      plugins.some((plugin) => plugin.id === currentPluginId)
        ? currentPluginId
        : (plugins[0]?.id ?? null),
    );
  }, [plugins]);

  useEffect(() => {
    if (selectedPluginId === null) {
      setDetails(null);
      setDraft({});
      setDetailsLoadFailed(false);
      return undefined;
    }
    const abortController = new AbortController();
    void loadPluginDetails(selectedPluginId, abortController.signal);
    return () => {
      detailsRequestSequence.current += 1;
      abortController.abort();
    };
  }, [loadPluginDetails, selectedPluginId]);

  /** 将表单字段变化限制在当前选择的插件草稿，避免切换列表时污染另一插件配置。 */
  const updateDraft = (fieldName: string, value: PluginFormValue) => {
    setDraft((currentDraft) => ({ ...currentDraft, [fieldName]: value }));
  };

  /** 保存配置并采用接口返回的脱敏详情；插件列表运行态由 SSE 事件更新，不追加列表 GET。 */
  const saveConfiguration = async () => {
    if (
      selectedDetails === null ||
      selectedDetails.configSchema === null ||
      actionPending ||
      hasMissingRequiredField(selectedDetails, draft)
    ) {
      return;
    }
    const nextDetails = await updatePluginConfiguration(
      selectedDetails.snapshot.id,
      createConfigurationUpdate(selectedDetails, draft),
    );
    if (nextDetails === null) {
      return;
    }
    setDetails(nextDetails);
    setDraft(createConfigurationDraft(nextDetails));
  };

  /** 切换插件启停；操作响应只确认结果，列表状态和连接计数始终以 SSE 权威快照为准。 */
  const toggleEnabled = async (plugin: PluginSnapshot) => {
    await setPluginEnabled(plugin.id, !plugin.enabled);
  };

  /** 重载当前插件；宿主会发布生命周期变化，禁用插件仍保持禁用状态。 */
  const reloadSelectedPlugin = async () => {
    if (selectedDetails === null) {
      return;
    }
    const pluginId = selectedDetails.snapshot.id;
    // 重载会替换插件实例，必须重新读取 schema 与脱敏配置，禁止沿用旧实例的表单契约。
    if (await reloadPlugin(pluginId)) {
      await loadPluginDetails(pluginId);
    }
  };

  /** 上传本地插件包；安装完成后的列表变化由宿主事件发布，同名文件仍可再次选择。 */
  const installPackage = async (event: ChangeEvent<HTMLInputElement>) => {
    const packageFile = event.currentTarget.files?.[0];
    event.currentTarget.value = "";
    if (packageFile === undefined) {
      return;
    }
    await installPluginPackage(packageFile);
  };

  /**
   * 在用户手势内直接打开原生文件选择器。
   * 文件输入完全隐藏在辅助功能树外，避免浏览器把它误识别成第二个无名称按钮；可见按钮是唯一入口，必须同步触发同一轮用户手势中的原生选择器。
   * 输入引用与按钮同一渲染分支创建，缺失代表组件结构被破坏，直接抛出错误而不伪造安装已开始的状态。
   */
  const openPackageChooser = () => {
    const packageInput = packageInputReference.current;
    if (packageInput === null) {
      throw new Error("插件包文件输入未挂载");
    }
    packageInput.click();
  };

  const selectedPlugin =
    plugins.find((plugin) => plugin.id === selectedPluginId) ?? null;
  const selectedDetails =
    details?.snapshot.id === selectedPluginId ? details : null;
  const configSchema = selectedDetails?.configSchema ?? null;
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
    selectedDetails !== null && hasMissingRequiredField(selectedDetails, draft);

  return (
    <>
      <main className="pageShell pluginManagerPage">
        <header className="pageHeader">
          <div>
            <h1>{t("plugins.title")}</h1>
            <p>{t("plugins.description")}</p>
          </div>
        </header>
        <div className="pluginManagerWorkspace">
          <div className="pluginManagerLayout">
            <aside aria-label={t("plugins.title")} className="pluginListPanel">
              <div className="pluginListActions">
                <button
                  className="fileChoiceButton"
                  disabled={actionPending}
                  type="button"
                  onClick={openPackageChooser}
                >
                  <PackagePlus aria-hidden="true" size={15} />
                  <span>{t("plugins.install")}</span>
                </button>
                <input
                  accept=".zip,.tplugin.zip,application/zip"
                  disabled={actionPending}
                  hidden
                  ref={packageInputReference}
                  type="file"
                  onChange={(event) => void installPackage(event)}
                />
                <button
                  aria-label={t("plugins.refresh")}
                  className="iconButton"
                  disabled={snapshot === null || actionPending}
                  title={t("plugins.refresh")}
                  type="button"
                  onClick={() => void refresh()}
                >
                  <RefreshCcw aria-hidden="true" size={15} />
                </button>
              </div>
              {snapshot === null && (
                <p className="viewerNotice">{t("plugins.loading")}</p>
              )}
              {snapshot !== null && plugins.length === 0 && (
                <p className="pluginEmptyState">{t("plugins.empty")}</p>
              )}
              <ul className="pluginList">
                {plugins.map((plugin) => (
                  <li key={plugin.id}>
                    <button
                      aria-current={
                        selectedPlugin?.id === plugin.id ? "true" : undefined
                      }
                      className={`pluginListItem${
                        selectedPlugin?.id === plugin.id ? " isSelected" : ""
                      }`}
                      type="button"
                      onClick={() => setSelectedPluginId(plugin.id)}
                    >
                      <PlugZap aria-hidden="true" size={16} />
                      <span>
                        <strong>{plugin.name}</strong>
                        <small>{plugin.id}</small>
                      </span>
                      <em
                        className={`pluginState pluginState--${plugin.state}`}
                      >
                        {t(`plugins.states.${plugin.state}`)}
                      </em>
                    </button>
                  </li>
                ))}
              </ul>
            </aside>
            <div className="pluginDetailsPanel">
              {selectedPlugin === null && !detailsLoading && (
                <p className="pluginEmptyState">{t("plugins.select")}</p>
              )}
              {selectedPlugin !== null &&
                (detailsLoading || selectedDetails === null) &&
                !detailsLoadFailed && (
                  <p className="viewerNotice">{t("plugins.loading")}</p>
                )}
              {detailsLoadFailed && !detailsLoading && (
                <p className="viewerNotice viewerNotice--error">
                  {t("plugins.loadFailed")}
                </p>
              )}
              {selectedPlugin !== null &&
                selectedDetails !== null &&
                !detailsLoading && (
                  <>
                    <section
                      className="pluginOverview"
                      aria-label={selectedPlugin.name}
                    >
                      <div>
                        <h3>{selectedPlugin.name}</h3>
                        <p>{selectedPlugin.id}</p>
                      </div>
                      <div className="pluginOverviewActions">
                        <button
                          className="pluginActionButton"
                          disabled={actionPending}
                          type="button"
                          onClick={() => void toggleEnabled(selectedPlugin)}
                        >
                          <PlugZap aria-hidden="true" size={14} />
                          {selectedPlugin.enabled
                            ? t("plugins.disable")
                            : t("plugins.enable")}
                        </button>
                        <button
                          className="pluginActionButton"
                          disabled={actionPending}
                          type="button"
                          onClick={() => void reloadSelectedPlugin()}
                        >
                          <RefreshCcw aria-hidden="true" size={14} />
                          {t("plugins.reload")}
                        </button>
                        <button
                          aria-label={t("plugins.uninstall")}
                          className="iconButton pluginUninstallButton"
                          disabled={
                            actionPending ||
                            selectedPlugin.activeConnections > 0
                          }
                          title={t("plugins.uninstall")}
                          type="button"
                          onClick={() =>
                            void showIndependentWindow({
                              kind: "pluginUninstall",
                              pluginId: selectedPlugin.id,
                              pluginName: selectedPlugin.name,
                            })
                          }
                        >
                          <Trash2 aria-hidden="true" size={15} />
                        </button>
                      </div>
                      <dl className="pluginMetadata">
                        <div>
                          <dt>{t("plugins.runtime")}</dt>
                          <dd>{selectedPlugin.runtime}</dd>
                        </div>
                        <div>
                          <dt>{t("plugins.apiVersion")}</dt>
                          <dd>{selectedPlugin.apiVersion}</dd>
                        </div>
                        <div>
                          <dt>{t("plugins.hooks")}</dt>
                          <dd>
                            {formatHooks(selectedPlugin, t("plugins.noHooks"))}
                          </dd>
                        </div>
                        <div>
                          <dt>
                            {t("plugins.activeConnections", { count: 0 })}
                          </dt>
                          <dd>{selectedPlugin.activeConnections}</dd>
                        </div>
                      </dl>
                      {selectedPlugin.errorCode !== null && (
                        <p className="pluginErrorCode">
                          {selectedPlugin.errorCode}
                        </p>
                      )}
                    </section>
                    <section
                      className="pluginConfiguration"
                      aria-label={t("plugins.configuration")}
                    >
                      <header>
                        <Settings2 aria-hidden="true" size={16} />
                        <div>
                          <h3>
                            {configSchema?.title || t("plugins.configuration")}
                          </h3>
                          {configSchema?.description !== "" && (
                            <p>{configSchema?.description}</p>
                          )}
                        </div>
                      </header>
                      {configSchema === null && (
                        <p className="pluginEmptyState">
                          {t("plugins.noSchema")}
                        </p>
                      )}
                      {configSchema !== null && (
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
                                configuredSecret={
                                  selectedDetails?.configuredSecretFields.includes(
                                    fieldName,
                                  ) ?? false
                                }
                                disabled={actionPending}
                                field={field}
                                fieldName={fieldName}
                                key={fieldName}
                                required={configSchema.required.includes(
                                  fieldName,
                                )}
                                value={draft[fieldName]}
                                onChange={updateDraft}
                              />
                            ))}
                          </div>
                          <div className="pluginConfigurationActions">
                            <button
                              className="primaryButton"
                              disabled={
                                actionPending || configurationIncomplete
                              }
                              type="button"
                              onClick={() => void saveConfiguration()}
                            >
                              {t("plugins.saveConfiguration")}
                            </button>
                          </div>
                        </>
                      )}
                    </section>
                  </>
                )}
            </div>
          </div>
        </div>
      </main>
    </>
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
 * 渲染单个受 Schema 约束的插件字段。
 *
 * 运行上下文：字段只允许标量值，密码输入永远不回显宿主已保存的内容。
 * 参数：field 描述类型与范围，value 是局部草稿，onChange 回传已归一化的标量。
 * 失败语义：无效的数字文本保留为空，由必填状态和服务端规则阻止提交。
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

  /** 将原生数字输入转换为有限标量；空值保留为文本以便可选字段被省略。 */
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
