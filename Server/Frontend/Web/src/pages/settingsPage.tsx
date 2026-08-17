import { Save, Settings2, ShieldCheck } from "lucide-react";
import { type ChangeEvent, type FormEvent, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { NavLink, useNavigate, useParams } from "react-router-dom";

import {
  maximumConnections,
  maximumRelayBufferSize,
  maximumShutdownTimeoutSeconds,
  maximumTotalRelayBufferSize,
  maximumUdpPacketSize,
  type ConfigurationUpdate,
  type PublicConfiguration,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";
import { LanguageSelector } from "../components/languageSelector";
import { MultiAccountSettings } from "../components/multiAccountSettings";

/** 服务设置页内可切换的二级区域。 */
export type SettingsSection =
  | "interface"
  | "listener"
  | "multiAccount"
  | "upstreamProxy"
  | "capacity"
  | "mcp";

interface SettingsDraft extends Omit<
  PublicConfiguration,
  "authenticationUsernames"
> {
  username: string;
  password: string;
  upstreamPassword: string;
  upstreamPasswordChanged: boolean;
}

const settingsSections = [
  { value: "interface", labelKey: "app.language.label" },
  { value: "listener", labelKey: "page.settings.listenGroup" },
  { value: "multiAccount", labelKey: "page.settings.multiAccountGroup" },
  { value: "upstreamProxy", labelKey: "page.settings.upstreamProxyGroup" },
  { value: "capacity", labelKey: "page.settings.capacityGroup" },
  { value: "mcp", labelKey: "page.settings.mcpGroup" },
] as const;

/**
 * 校验 URL 参数是否映射到已声明的设置区域，未知地址统一显示界面语言首页。
 *
 * 运行上下文：工具菜单和左侧设置导航都通过 `/settings/:section` 路由驱动当前区域。
 * 参数：section 为路由参数的原始字符串。
 * 失败语义：空值或未知值返回 false，不向配置表单写入无效状态。
 */
function isSettingsSection(
  section: string | undefined,
): section is SettingsSection {
  return settingsSections.some((item) => item.value === section);
}

/**
 * 从公开配置创建可编辑草稿，认证口令始终保持为空，避免服务端不可读字段回填到浏览器。
 *
 * 运行上下文：控制快照更新时由设置页重建草稿。
 * 参数：configuration 为控制接口返回的完整公开配置。
 * 失败语义：缺少已保存用户名时回退为空字符串，不生成伪造认证信息。
 */
function createDraft(configuration: PublicConfiguration): SettingsDraft {
  return {
    ...configuration,
    username: configuration.authenticationUsernames[0] ?? "",
    password: "",
    upstreamPassword: "",
    upstreamPasswordChanged: false,
  };
}

/**
 * 将数值输入转换为有限数，禁止 NaN 写入受控草稿。
 *
 * 运行上下文：监听和容量字段都经过本函数进入草稿。
 * 参数：event 为原生输入事件，previousValue 为保留的上一个有效值。
 * 失败语义：空值或无效值保持 previousValue，随后由浏览器约束提示用户修正。
 */
function readNumberInput(
  event: ChangeEvent<HTMLInputElement>,
  previousValue: number,
): number {
  const nextValue = event.target.valueAsNumber;
  return Number.isFinite(nextValue) ? nextValue : previousValue;
}

/**
 * 渲染独立服务设置页；左侧导航由路由驱动当前内容面板，不再以锚点堆叠全部设置。
 *
 * 运行上下文：工具菜单直接导航至设置路径，服务快照变化仅刷新对应草稿。
 * 参数：路由 section 指定当前区域，未知 section 回退到界面语言区域。
 * 失败语义：控制服务未连接时保留语言区域，其余区域显示不可用状态且不允许提交配置。
 */
interface SettingsPageProps {
  routeBase?: string;
  onClose?(): void;
}

/**
 * 渲染独立服务设置页；routeBase 让主路由与独立窗口复用同一表单而不复制导航实现。
 * 失败语义：无效 routeBase 只影响浏览器地址，不改变任何服务端配置提交。
 */
export function SettingsPage({
  routeBase = "/settings",
  onClose,
}: SettingsPageProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { section } = useParams<{ section?: string }>();
  const {
    snapshot,
    actionPending,
    lastError,
    updateConfiguration,
    updateMcpConfiguration,
    getManagementIdentity,
    updateManagementIdentity,
    getManagementApiKey,
  } = useServiceStore();
  const configuration = snapshot?.configuration ?? null;
  const [draft, setDraftState] = useState<SettingsDraft | null>(
    configuration === null ? null : createDraft(configuration),
  );
  const draftDirty = useRef(false);
  const activeSection = isSettingsSection(section) ? section : "interface";

  useEffect(() => {
    if (draftDirty.current) {
      return;
    }
    setDraftState(configuration === null ? null : createDraft(configuration));
  }, [configuration]);

  /**
   * 写入用户正在编辑的设置草稿并锁定其编辑代次。
   *
   * 运行上下文：控制快照会因指标和连接事件持续刷新；一旦用户修改任意字段，后续快照
   * 只能更新只读状态，不能覆盖尚未提交的输入。参数为完整设置草稿，本地状态更新不产生
   * 失败返回；提交或关闭窗口会结束该草稿代次。
   */
  const setDraft = (nextDraft: SettingsDraft) => {
    draftDirty.current = true;
    setDraftState(nextDraft);
  };

  const restartRequired = snapshot?.serviceState !== "stopped";
  const connectionLimit =
    draft === null
      ? maximumConnections
      : Math.min(
          maximumConnections,
          Math.floor(maximumTotalRelayBufferSize / (draft.relayBufferSize * 2)),
        );
  const relayBufferLimit =
    draft === null
      ? maximumRelayBufferSize
      : Math.min(
          maximumRelayBufferSize,
          Math.floor(maximumTotalRelayBufferSize / (draft.maxConnections * 2)),
        );

  /**
   * 校验并提交当前区域的完整配置；仅填写了用户名和新口令时才构造认证凭据。
   *
   * 运行上下文：监听、认证、容量区域共用单一提交入口，保持后端配置更新原子性。
   * 参数：event 为表单提交事件。
   * 失败语义：草稿或快照缺失、正在提交时不产生控制请求；服务运行时由控制面强制断连后重启。
   */
  const submitSettings = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (actionPending) {
      return;
    }
    if (draft === null) {
      return;
    }
    const update: ConfigurationUpdate = {
      startServiceOnLaunch: draft.startServiceOnLaunch,
      listenHost: draft.listenHost,
      listenPort: draft.listenPort,
      authenticationMode: draft.authenticationMode,
      maxConnections: draft.maxConnections,
      connectTimeout: draft.connectTimeout,
      bindTimeout: draft.bindTimeout,
      idleTimeout: draft.idleTimeout,
      shutdownTimeout: draft.shutdownTimeout,
      readTimeout: draft.readTimeout,
      relayBufferSize: draft.relayBufferSize,
      udpBindHost: draft.udpBindHost,
      udpMaxPacketSize: draft.udpMaxPacketSize,
      httpProxy: {
        ...draft.httpProxy,
        enabled: true,
        listenHost: draft.listenHost,
        listenPort: draft.listenPort,
      },
      upstreamProxy: {
        enabled: draft.upstreamProxy.enabled,
        protocol: draft.upstreamProxy.protocol,
        host: draft.upstreamProxy.host,
        port: draft.upstreamProxy.port,
        username: draft.upstreamProxy.username,
        password: draft.upstreamPasswordChanged
          ? draft.upstreamPassword
          : null,
      },
      processCapture: {
        enabled: draft.processCapture.enabled,
        processIds: draft.processCapture.processIds,
        proxyPort: draft.listenPort,
      },
      multiAccount: {
        enabled: draft.multiAccount.enabled,
        remoteHost: draft.multiAccount.remoteHost,
        remotePort: draft.multiAccount.remotePort,
      },
      credentials:
        draft.authenticationMode === "password" &&
        draft.username &&
        draft.password
          ? { username: draft.username, password: draft.password }
          : null,
    };
    // 只有服务端确认提交后才释放草稿。旧实现在请求发出前解锁，
    // 任一旧快照或请求失败都会把用户刚打开的开关立即关闭。
    if (await updateConfiguration(update)) {
      draftDirty.current = false;
    }
  };

  const activeLabelKey = settingsSections.find(
    (section) => section.value === activeSection,
  )?.labelKey;
  const activeLabel = t(activeLabelKey ?? "page.settings.title");
  const configurationAvailable = draft !== null && snapshot !== null;
  const sectionDescriptionKeys: Record<SettingsSection, string> = {
    interface: "page.settings.descriptionInterface",
    listener: "page.settings.descriptionListener",
    multiAccount: "page.settings.descriptionMultiAccount",
    upstreamProxy: "page.settings.descriptionUpstreamProxy",
    capacity: "page.settings.descriptionCapacity",
    mcp: "page.settings.descriptionMcp",
  };

  return (
    <main className="pageShell settingsPage">
      <header className="pageHeader">
        <div>
          <h1>{t("page.settings.title")}</h1>
          <p>{t(sectionDescriptionKeys[activeSection])}</p>
        </div>
      </header>
      <form className="settingsWorkspace" onSubmit={submitSettings}>
        <nav
          aria-label={t("page.settings.title")}
          className="settingsNavigation"
        >
          {settingsSections.map(({ value, labelKey }, index) => (
            <NavLink
              className={({ isActive }) => (isActive ? "isActive" : undefined)}
              key={value}
              to={`${routeBase}/${value}`}
            >
              <small aria-hidden="true">
                {String(index + 1).padStart(2, "0")}
              </small>
              <span>{t(labelKey)}</span>
            </NavLink>
          ))}
        </nav>
        <div className="settingsContent">
          <header className="settingsContentHeader">
            <Settings2 aria-hidden="true" size={16} />
            <h3>{activeLabel}</h3>
          </header>
          {activeSection === "interface" ? (
            <div className="settingsSectionBody">
              <LanguageSelector className="settingsLanguageSelector" />
            </div>
          ) : !configurationAvailable ? (
            <div className="settingsUnavailable">
              <ShieldCheck aria-hidden="true" size={24} />
              <strong>{t("page.settings.unavailableTitle")}</strong>
              <span>{t("page.settings.unavailableHint")}</span>
            </div>
          ) : activeSection === "mcp" ? (
            <div className="settingsListenerGroups" aria-label={activeLabel}>
              <fieldset disabled={actionPending}>
                <legend>{t("page.settings.mcpGroup")}</legend>
                <label className="settingsCheckboxRow">
                  <input
                    checked={snapshot.mcp.configuration.enabled}
                    type="checkbox"
                    onChange={(event) =>
                      void updateMcpConfiguration({
                        ...snapshot.mcp.configuration,
                        enabled: event.target.checked,
                      })
                    }
                  />
                  <span>
                    <strong>{t("page.settings.mcpEnabled")}</strong>
                    <small>{t("page.settings.mcpEnabledHint")}</small>
                  </span>
                </label>
                <label>
                  <span>{t("page.settings.mcpPort")}</span>
                  <input
                    disabled={snapshot.mcp.configuration.enabled}
                    max={65535}
                    min={1}
                    type="number"
                    value={snapshot.mcp.configuration.port}
                    onChange={(event) =>
                      void updateMcpConfiguration({
                        enabled: false,
                        port: readNumberInput(event, snapshot.mcp.configuration.port),
                      })
                    }
                  />
                </label>
                <label>
                  <span>{t("page.settings.mcpEndpoint")}</span>
                  <input readOnly value={snapshot.mcp.endpoint ?? "—"} />
                </label>
                {snapshot.mcp.lastError ? (
                  <p className="settingsRestartHint">{snapshot.mcp.lastError}</p>
                ) : null}
              </fieldset>
            </div>
          ) : activeSection === "listener" ? (
            <div className="settingsListenerGroups" aria-label={activeLabel}>
              <fieldset disabled={actionPending}>
                <legend>{t("page.settings.socksGroup")}</legend>
                <label className="settingsCheckboxRow">
                  <input
                    checked={draft.startServiceOnLaunch}
                    type="checkbox"
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        startServiceOnLaunch: event.target.checked,
                      })
                    }
                  />
                  <span>
                    <strong>{t("page.settings.startServiceOnLaunch")}</strong>
                    <small>
                      {t("page.settings.startServiceOnLaunchHint")}
                    </small>
                  </span>
                </label>
                <label>
                  <span>{t("page.settings.listenHost")}</span>
                  <input
                    required
                    value={draft.listenHost}
                    onChange={(event) =>
                      setDraft({ ...draft, listenHost: event.target.value })
                    }
                  />
                </label>
                <label>
                  <span>{t("page.settings.listenPort")}</span>
                  <input
                    required
                    max={65535}
                    min={1}
                    type="number"
                    value={draft.listenPort}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        listenPort: readNumberInput(event, draft.listenPort),
                      })
                    }
                  />
                </label>
                <label>
                  <span>{t("page.settings.udpBindHost")}</span>
                  <input
                    placeholder={t("page.settings.udpBindHostPlaceholder")}
                    value={draft.udpBindHost}
                    onChange={(event) =>
                      setDraft({ ...draft, udpBindHost: event.target.value })
                    }
                  />
                </label>
              </fieldset>
              {!draft.multiAccount.enabled ? (
                <fieldset aria-label={activeLabel} disabled={actionPending}>
                  <label>
                    <span>{t("page.settings.authenticationMode")}</span>
                    <select
                      value={draft.authenticationMode}
                      onChange={(event) =>
                        setDraft({
                          ...draft,
                          authenticationMode: event.target.value as
                            | "none"
                            | "password"
                            | "plugin",
                        })
                      }
                    >
                      <option value="none">
                        {t("page.settings.authenticationNone")}
                      </option>
                      <option value="password">
                        {t("page.settings.authenticationPassword")}
                      </option>
                      <option value="plugin">
                        {t("page.settings.authenticationPlugin")}
                      </option>
                    </select>
                  </label>
                  <label>
                    <span>{t("page.settings.username")}</span>
                    <input
                      autoComplete="username"
                      disabled={draft.authenticationMode !== "password"}
                      value={draft.username}
                      onChange={(event) =>
                        setDraft({ ...draft, username: event.target.value })
                      }
                    />
                  </label>
                  <label>
                    <span>{t("page.settings.newPassword")}</span>
                    <input
                      autoComplete="new-password"
                      disabled={draft.authenticationMode !== "password"}
                      type="password"
                      value={draft.password}
                      onChange={(event) =>
                        setDraft({ ...draft, password: event.target.value })
                      }
                    />
                  </label>
                </fieldset>
              ) : (
                <div className="settingsUnavailable">
                  <ShieldCheck aria-hidden="true" size={24} />
                  <strong>
                    {t("page.settings.multiAccountOverridesAuthentication")}
                  </strong>
                  <span>
                    {t("page.settings.multiAccountOverridesAuthenticationHint")}
                  </span>
                </div>
              )}
            </div>
          ) : activeSection === "multiAccount" ? (
            <MultiAccountSettings
              configuration={draft.multiAccount}
              disabled={actionPending}
              getApiKey={getManagementApiKey}
              getIdentity={getManagementIdentity}
              updateIdentity={updateManagementIdentity}
              onConfigurationChange={(multiAccount) =>
                setDraft({ ...draft, multiAccount })
              }
            />
          ) : activeSection === "upstreamProxy" ? (
            <div className="settingsListenerGroups" aria-label={activeLabel}>
              <fieldset disabled={actionPending}>
                <legend>{t("page.settings.upstreamProxyGroup")}</legend>
                <label className="settingsCheckboxRow">
                  <input
                    checked={draft.upstreamProxy.enabled}
                    type="checkbox"
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        upstreamProxy: {
                          ...draft.upstreamProxy,
                          enabled: event.target.checked,
                        },
                      })
                    }
                  />
                  <span>
                    <strong>{t("page.settings.upstreamProxyEnabled")}</strong>
                    <small>{t("page.settings.upstreamProxyEnabledHint")}</small>
                  </span>
                </label>
                <label>
                  <span>{t("page.settings.upstreamProxyProtocol")}</span>
                  <select
                    disabled={!draft.upstreamProxy.enabled}
                    value={draft.upstreamProxy.protocol}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        upstreamProxy: {
                          ...draft.upstreamProxy,
                          protocol: event.target.value as "http" | "socks5",
                        },
                      })
                    }
                  >
                    <option value="http">HTTP CONNECT</option>
                    <option value="socks5">SOCKS5</option>
                  </select>
                </label>
                <label>
                  <span>{t("page.settings.upstreamProxyHost")}</span>
                  <input
                    disabled={!draft.upstreamProxy.enabled}
                    required={draft.upstreamProxy.enabled}
                    value={draft.upstreamProxy.host}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        upstreamProxy: {
                          ...draft.upstreamProxy,
                          host: event.target.value,
                        },
                      })
                    }
                  />
                </label>
                <label>
                  <span>{t("page.settings.upstreamProxyPort")}</span>
                  <input
                    disabled={!draft.upstreamProxy.enabled}
                    required={draft.upstreamProxy.enabled}
                    max={65535}
                    min={1}
                    type="number"
                    value={draft.upstreamProxy.port}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        upstreamProxy: {
                          ...draft.upstreamProxy,
                          port: readNumberInput(
                            event,
                            draft.upstreamProxy.port,
                          ),
                        },
                      })
                    }
                  />
                </label>
                <label>
                  <span>{t("page.settings.upstreamProxyUsername")}</span>
                  <input
                    autoComplete="username"
                    disabled={!draft.upstreamProxy.enabled}
                    value={draft.upstreamProxy.username}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        upstreamProxy: {
                          ...draft.upstreamProxy,
                          username: event.target.value,
                        },
                      })
                    }
                  />
                </label>
                <label>
                  <span>{t("page.settings.upstreamProxyPassword")}</span>
                  <input
                    autoComplete="new-password"
                    disabled={!draft.upstreamProxy.enabled}
                    placeholder={
                      draft.upstreamProxy.hasPassword
                        ? t("page.settings.upstreamProxyPasswordSaved")
                        : undefined
                    }
                    type="password"
                    value={draft.upstreamPassword}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        upstreamPassword: event.target.value,
                        upstreamPasswordChanged: true,
                      })
                    }
                  />
                  {draft.upstreamProxy.hasPassword ? (
                    <button
                      type="button"
                      onClick={() =>
                        setDraft({
                          ...draft,
                          upstreamPassword: "",
                          upstreamPasswordChanged: true,
                        })
                      }
                    >
                      {t("page.settings.upstreamProxyPasswordClear")}
                    </button>
                  ) : null}
                </label>
              </fieldset>
            </div>
          ) : (
            <div className="settingsCapacityGroups" aria-label={activeLabel}>
              {(
                [
                  {
                    legendKey: "page.settings.capacityConnections",
                    fields: [
                      {
                        fieldName: "maxConnections" as const,
                        label: t("page.settings.maxConnections"),
                        minimum: 1,
                        maximum: connectionLimit,
                      },
                    ],
                  },
                  {
                    legendKey: "page.settings.capacityTimeouts",
                    fields: [
                      {
                        fieldName: "connectTimeout" as const,
                        label: t("page.settings.connectTimeout"),
                        minimum: 0.1,
                        maximum: undefined,
                      },
                      {
                        fieldName: "bindTimeout" as const,
                        label: t("page.settings.bindTimeout"),
                        minimum: 0.1,
                        maximum: undefined,
                      },
                      {
                        fieldName: "idleTimeout" as const,
                        label: t("page.settings.idleTimeout"),
                        minimum: 0.1,
                        maximum: undefined,
                      },
                      {
                        fieldName: "shutdownTimeout" as const,
                        label: t("page.settings.shutdownTimeout"),
                        minimum: 0.1,
                        maximum: maximumShutdownTimeoutSeconds,
                      },
                      {
                        fieldName: "readTimeout" as const,
                        label: t("page.settings.readTimeout"),
                        minimum: 0.1,
                        maximum: undefined,
                      },
                    ],
                  },
                  {
                    legendKey: "page.settings.capacityBuffers",
                    fields: [
                      {
                        fieldName: "relayBufferSize" as const,
                        label: t("page.settings.relayBufferSize"),
                        minimum: 1024,
                        maximum: relayBufferLimit,
                      },
                      {
                        fieldName: "udpMaxPacketSize" as const,
                        label: t("page.settings.udpMaxPacketSize"),
                        minimum: 512,
                        maximum: maximumUdpPacketSize,
                      },
                    ],
                  },
                ] as const
              ).map((group) => (
                <details
                  className="settingsCapacityGroup"
                  key={group.legendKey}
                  open
                >
                  <summary>{t(group.legendKey)}</summary>
                  <fieldset
                    className="settingsCapacityFields"
                    disabled={actionPending}
                  >
                    {group.fields.map(
                      ({ fieldName, label, minimum, maximum }) => (
                        <label key={fieldName}>
                          <span>{label}</span>
                          <input
                            required
                            max={maximum}
                            min={minimum}
                            step={minimum < 1 ? 0.1 : 1}
                            type="number"
                            value={draft[fieldName]}
                            onChange={(event) =>
                              setDraft({
                                ...draft,
                                [fieldName]: readNumberInput(
                                  event,
                                  draft[fieldName],
                                ),
                              })
                            }
                          />
                        </label>
                      ),
                    )}
                  </fieldset>
                </details>
              ))}
            </div>
          )}
          {restartRequired && activeSection !== "interface" && activeSection !== "mcp" && (
            <p className="settingsRestartHint">
              {t("page.settings.restartHint")}
            </p>
          )}
          {lastError !== null && activeSection !== "interface" && (
            <p className="settingsError" role="alert">{lastError}</p>
          )}
        </div>
        <footer className="settingsActions">
          <button
            disabled={actionPending}
            type="button"
            onClick={() =>
              onClose === undefined ? navigate("/overview") : onClose()
            }
          >
            {t("page.settings.back")}
          </button>
          {activeSection !== "interface" && activeSection !== "mcp" && (
            <button
              className="primaryButton"
              disabled={!configurationAvailable || actionPending}
              type="submit"
            >
              <Save aria-hidden="true" size={15} />
              {actionPending
                ? t("page.settings.applying")
                : t("page.settings.apply")}
            </button>
          )}
        </footer>
      </form>
    </main>
  );
}
