import {
  Download,
  Plus,
  RefreshCcw,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import {
  type KeyboardEvent,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";

import {
  maximumCachedCertificates,
  type LocationPattern,
  type SslConfiguration,
  type SslPublicState,
} from "../api/protocol";
import i18n from "../i18n";
import { useServiceStore } from "../state/serviceStore";
import { ConfirmDialog } from "./confirmDialog";
import { ClientCertificateManager } from "./clientCertificateManager";
import { RuleEditorDialog } from "./ruleEditorDialog";

interface SslSettingsDialogProps {
  open: boolean;
  focusClientCertificate?: boolean;
  initialLocation?: LocationPattern | null;
  onClose(): void;
}

type RuleCollection = "includeLocations" | "excludeLocations";

/// 创建只约束 HTTPS 主机的 Location 规则；高级端口和路径边界后续仍由共用协议结构保留。
function createHostRule(host = ""): LocationPattern {
  return {
    protocol: "https",
    host,
    port: "",
    path: "",
    query: null,
  };
}

/// 从公开状态提取可提交配置，确保证书元数据和握手计数不会被误发回更新端点。
function createConfiguration(state: SslPublicState): SslConfiguration {
  return {
    enabled: state.enabled,
    includeLocations: state.includeLocations,
    excludeLocations: state.excludeLocations,
    maxCachedCertificates: state.maxCachedCertificates,
    useClientSni: state.useClientSni,
  };
}

/**
 * 判断两条 SSL Location 是否完全相同。
 * 运行上下文：SSL 草稿合并右键主机范围前调用，避免追加重复规则。
 * 参数：left 与 right 是标准化后的规则；失败语义：任一字段不同即视为不同范围。
 */
function locationsEqual(
  left: LocationPattern,
  right: LocationPattern,
): boolean {
  return (
    left.protocol === right.protocol &&
    left.host === right.host &&
    left.port === right.port &&
    left.path === right.path &&
    left.query === right.query
  );
}

/**
 * 将右键事务位置合并进 SSL 包含规则；保留原有顺序并收敛为握手阶段可匹配的主机范围。
 *
 * 运行上下文：SSL 设置从权威快照创建草稿时调用。
 * 参数：configuration 为新草稿，initialLocation 为右键事务携带的位置上下文。
 * 失败语义：没有位置或位置已存在时原样返回，不制造重复规则。
 */
function applyInitialLocation(
  configuration: SslConfiguration,
  initialLocation: LocationPattern | null,
): SslConfiguration {
  if (initialLocation === null) {
    return configuration;
  }
  const normalizedLocation = {
    ...initialLocation,
    protocol: "https",
    // CONNECT 分类发生在 HTTP 路径和查询串可见之前；保留资源路径会生成永远无法命中的规则。
    path: "",
    query: null,
  };
  if (
    configuration.includeLocations.some((location) =>
      locationsEqual(location, normalizedLocation),
    )
  ) {
    return configuration;
  }
  return {
    ...configuration,
    includeLocations: [...configuration.includeLocations, normalizedLocation],
  };
}

/// 按当前界面语言格式化证书时间；无效时间返回稳定占位而不是浏览器异常文本。
function formatCertificateTime(milliseconds: number): string {
  const date = new Date(milliseconds);
  if (Number.isNaN(date.valueOf())) {
    return "—";
  }
  return new Intl.DateTimeFormat(i18n.resolvedLanguage ?? "en", {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

/// 创建并立即触发根证书下载；对象 URL 只在当前同步动作期间存活。
function downloadCertificate(certificate: Blob, format: "pem" | "cer") {
  const objectUrl = URL.createObjectURL(certificate);
  const anchor = document.createElement("a");
  anchor.href = objectUrl;
  anchor.download = `root.${format}`;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(objectUrl);
}

interface RuleEditorProps {
  label: string;
  emptyHint: string;
  inputLabel: string;
  rules: LocationPattern[];
  disabled: boolean;
  onChange(rules: LocationPattern[]): void;
}

/**
 * 渲染 SSL 主机规则摘要与二级编辑器。
 * 运行上下文：包含和排除列表共用此组件；主机与端口只在用户确认后写回父级草稿。
 * 参数：rules 是当前有序规则，inputLabel 同时提供编辑字段和摘要按钮的辅助名称。
 * 失败语义：required 校验失败或用户取消时不调用 onChange，不产生空主机规则。
 */
function HostRuleEditor({
  label,
  emptyHint,
  inputLabel,
  rules,
  disabled,
  onChange,
}: RuleEditorProps) {
  const { t } = useTranslation();
  const [editor, setEditor] = useState<{
    index: number | null;
    draft: LocationPattern;
  } | null>(null);

  /** 确认后追加或替换完整主机范围；取消只销毁本地草稿，不产生空规则。 */
  const saveRule = () => {
    if (editor === null) {
      return;
    }
    onChange(
      editor.index === null
        ? [...rules, editor.draft]
        : rules.map((rule, index) =>
            index === editor.index ? editor.draft : rule,
          ),
    );
    setEditor(null);
  };

  return (
    <section className="sslRuleGroup">
      <div className="sslRuleHeader">
        <div>
          <strong>{label}</strong>
          {rules.length === 0 && <span>{emptyHint}</span>}
        </div>
        <button
          disabled={disabled}
          type="button"
          onClick={() => setEditor({ index: null, draft: createHostRule() })}
        >
          <Plus aria-hidden="true" size={14} />
          {t("ssl.rules.add")}
        </button>
      </div>
      {rules.length > 0 && (
        <div className="sslRuleRows">
          {rules.map((rule, index) => (
            <div className="sslRuleRow" key={`${index}-${rule.protocol}`}>
              <ShieldCheck aria-hidden="true" size={15} />
              <button
                aria-label={`${inputLabel} ${index + 1}`}
                className="sslRuleSummary"
                disabled={disabled}
                type="button"
                onClick={() => setEditor({ index, draft: { ...rule } })}
              >
                <strong>{rule.host}</strong>
                <span>{rule.port === "" ? "*" : rule.port}</span>
              </button>
              <button
                className="iconButton"
                disabled={disabled}
                type="button"
                aria-label={t("ssl.rules.remove", { index: index + 1 })}
                onClick={() =>
                  onChange(rules.filter((_, ruleIndex) => ruleIndex !== index))
                }
              >
                <Trash2 aria-hidden="true" size={14} />
              </button>
            </div>
          ))}
        </div>
      )}
      <RuleEditorDialog
        cancelLabel={t("tools.form.cancelRule")}
        confirmLabel={t("tools.form.saveRule")}
        disabled={disabled}
        open={editor !== null}
        title={`${t("tools.form.ruleDialogTitle")} — ${label}`}
        onCancel={() => setEditor(null)}
        onConfirm={saveRule}
      >
        {editor !== null && (
          <fieldset className="toolLocationFields sslHostRuleFields">
            <legend>{label}</legend>
            <label>
              <span>{t("tools.form.host")}</span>
              <input
                aria-label={inputLabel}
                disabled={disabled}
                placeholder={t("ssl.rules.hostPlaceholder")}
                required
                value={editor.draft.host}
                onChange={(event) =>
                  setEditor({
                    ...editor,
                    draft: { ...editor.draft, host: event.target.value },
                  })
                }
              />
            </label>
            <label>
              <span>{t("tools.form.port")}</span>
              <input
                aria-label={`${inputLabel} ${t("tools.form.port")}`}
                disabled={disabled}
                placeholder={t("tools.form.portPlaceholder")}
                value={editor.draft.port}
                onChange={(event) =>
                  setEditor({
                    ...editor,
                    draft: { ...editor.draft, port: event.target.value },
                  })
                }
              />
            </label>
          </fieldset>
        )}
      </RuleEditorDialog>
    </section>
  );
}

/**
 * 渲染 SSL 独立设置窗口；规则、根证书和客户端身份共享同一份权威状态。
 * 运行上下文：工具栏进入时展示完整设置，事务右键进入时可预填主机规则或直接定位客户端证书表单。
 * 参数：initialLocation 提供主机边界，focusClientCertificate 控制初始焦点；失败由 Store 的状态区展示。
 */
export function SslSettingsDialog({
  open,
  focusClientCertificate = false,
  initialLocation = null,
  onClose,
}: SslSettingsDialogProps) {
  const { t } = useTranslation();
  const {
    snapshot,
    actionPending,
    updateSsl,
    regenerateSslRoot,
    exportSslRoot,
  } = useServiceStore();
  const [draft, setDraft] = useState<SslConfiguration | null>(null);
  const [regenerateConfirmationOpen, setRegenerateConfirmationOpen] =
    useState(false);
  const titleId = useId();
  const descriptionId = useId();
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const sslState = snapshot?.ssl ?? null;

  useEffect(() => {
    if (!open) {
      return undefined;
    }
    restoreFocusRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    setDraft(
      sslState === null
        ? null
        : applyInitialLocation(createConfiguration(sslState), initialLocation),
    );
    // 页脚“取消”是唯一无副作用退出入口；普通入口聚焦它，避免标题栏再提供一个重复关闭动作。
    if (!focusClientCertificate) {
      cancelButtonRef.current?.focus();
    }
    return () => {
      restoreFocusRef.current?.focus();
      restoreFocusRef.current = null;
    };
  }, [focusClientCertificate, initialLocation, open, sslState]);

  if (!open) {
    return null;
  }

  /**
   * Escape 在非忙碌状态关闭设置；提交期间保持单一请求生命周期，禁止中途丢弃视图。
   */
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape" && !actionPending) {
      event.preventDefault();
      onClose();
    }
  };

  /**
   * 替换一组规则；字段名受 RuleCollection 联合约束，不接受任意动态属性。
   */
  const updateRules = (
    collection: RuleCollection,
    rules: LocationPattern[],
  ) => {
    if (draft !== null) {
      setDraft({ ...draft, [collection]: rules });
    }
  };

  /**
   * 提交当前草稿；去掉空主机规则，避免留下 host 为空或仅空白的幽灵条目。
   * 应用成功后保留窗口，用户可以继续导出证书或检查服务端回读状态。
   */
  const applyConfiguration = async () => {
    if (draft === null) {
      return;
    }
    // 空主机规则无法命中任何流量；提交前剔除，防止禁用后仍残留无效条目。
    const normalized: SslConfiguration = {
      ...draft,
      includeLocations: draft.includeLocations.filter(
        (rule) => rule.host.trim() !== "",
      ),
      excludeLocations: draft.excludeLocations.filter(
        (rule) => rule.host.trim() !== "",
      ),
    };
    setDraft(normalized);
    await updateSsl(normalized);
  };

  /**
   * 下载指定公开格式；失败由 Store 写入全局错误，当前对话框保持打开。
   */
  const exportCertificate = async (format: "pem" | "cer") => {
    try {
      downloadCertificate(await exportSslRoot(format), format);
    } catch {
      // Store 已经把结构化失败写入状态，对话框不重复生成第二套错误文案。
    }
  };

  /**
   * 在二次确认后更换根证书；完成后关闭确认层并保留设置对话框展示新指纹。
   */
  const confirmRegeneration = async () => {
    const succeeded = await regenerateSslRoot();
    if (succeeded) {
      setRegenerateConfirmationOpen(false);
    }
  };

  return (
    <div
      className="dialogBackdrop"
      role="presentation"
      onKeyDown={handleKeyDown}
    >
      <section
        aria-describedby={descriptionId}
        aria-labelledby={titleId}
        aria-modal="true"
        className="sslSettingsDialog"
        role="dialog"
      >
        <header className="sslDialogHeader">
          <div>
            <h2 id={titleId}>{t("ssl.title")}</h2>
            <p id={descriptionId}>{t("ssl.description")}</p>
          </div>
        </header>

        {draft === null || sslState === null ? (
          <div className="sslUnavailable">
            <ShieldCheck aria-hidden="true" size={24} />
            <strong>{t("ssl.unavailable")}</strong>
          </div>
        ) : (
          <div className="sslDialogBody">
            <label className="sslEnabledRow">
              <input
                checked={draft.enabled}
                disabled={actionPending}
                type="checkbox"
                onChange={(event) =>
                  setDraft({ ...draft, enabled: event.target.checked })
                }
              />
              <span>
                <strong>{t("ssl.enabled")}</strong>
                <small>{t("ssl.enabledHint")}</small>
              </span>
            </label>

            <HostRuleEditor
              disabled={actionPending}
              emptyHint={t("ssl.rules.includeEmpty")}
              inputLabel={t("ssl.rules.includeInput")}
              label={t("ssl.rules.include")}
              rules={draft.includeLocations}
              onChange={(rules) => updateRules("includeLocations", rules)}
            />
            <HostRuleEditor
              disabled={actionPending}
              emptyHint={t("ssl.rules.excludeEmpty")}
              inputLabel={t("ssl.rules.excludeInput")}
              label={t("ssl.rules.exclude")}
              rules={draft.excludeLocations}
              onChange={(rules) => updateRules("excludeLocations", rules)}
            />

            <section className="sslCertificateCard">
              <div className="sslCertificateHeading">
                <div>
                  <strong>{t("ssl.certificate.title")}</strong>
                  <span>{sslState.ca.subject}</span>
                </div>
                <span className="sslInstalledBadge">
                  {t("ssl.certificate.installed")}
                </span>
              </div>
              <dl>
                <div>
                  <dt>{t("ssl.certificate.fingerprint")}</dt>
                  <dd>{sslState.ca.fingerprintSha256}</dd>
                </div>
                <div>
                  <dt>{t("ssl.certificate.validity")}</dt>
                  <dd>
                    {formatCertificateTime(
                      sslState.ca.validFromMilliseconds,
                    )}
                    {" — "}
                    {formatCertificateTime(
                      sslState.ca.validToMilliseconds,
                    )}
                  </dd>
                </div>
                <div>
                  <dt>{t("ssl.certificate.cache")}</dt>
                  <dd>
                    {t("ssl.certificate.cacheValue", {
                      count: sslState.cachedLeafCount,
                    })}
                  </dd>
                </div>
              </dl>
              <div className="sslCertificateActions">
                <button
                  disabled={actionPending}
                  type="button"
                  onClick={() => void exportCertificate("pem")}
                >
                  <Download aria-hidden="true" size={14} />
                  {t("ssl.certificate.exportPem")}
                </button>
                <button
                  disabled={actionPending}
                  type="button"
                  onClick={() => void exportCertificate("cer")}
                >
                  <Download aria-hidden="true" size={14} />
                  {t("ssl.certificate.exportCer")}
                </button>
                <button
                  className="dangerTextButton"
                  disabled={actionPending}
                  type="button"
                  onClick={() => setRegenerateConfirmationOpen(true)}
                >
                  <RefreshCcw aria-hidden="true" size={14} />
                  {t("ssl.certificate.regenerate")}
                </button>
              </div>
            </section>

            <ClientCertificateManager
              disabled={actionPending}
              focusOnMount={focusClientCertificate}
              initialLocation={initialLocation}
              state={sslState}
            />

            <details className="sslAdvanced">
              <summary>{t("ssl.advanced.title")}</summary>
              <div>
                <label>
                  <span>{t("ssl.advanced.cacheLimit")}</span>
                  <input
                    disabled={actionPending}
                    max={maximumCachedCertificates}
                    min={1}
                    type="number"
                    value={draft.maxCachedCertificates}
                    onChange={(event) => {
                      const value = event.target.valueAsNumber;
                      if (Number.isFinite(value)) {
                        setDraft({
                          ...draft,
                          maxCachedCertificates: value,
                        });
                      }
                    }}
                  />
                </label>
                <label className="sslCheckboxRow">
                  <input
                    checked={draft.useClientSni}
                    disabled={actionPending}
                    type="checkbox"
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        useClientSni: event.target.checked,
                      })
                    }
                  />
                  <span>{t("ssl.advanced.useClientSni")}</span>
                </label>
                <p>{sslState.supportedHttpVersions.join(" · ")}</p>
              </div>
            </details>

            <details className="sslHelp">
              <summary>{t("ssl.help.title")}</summary>
              <ol>
                <li>{t("ssl.help.export")}</li>
                <li>{t("ssl.help.install")}</li>
                <li>{t("ssl.help.store")}</li>
                <li>{t("ssl.help.restart")}</li>
              </ol>
              <p>{t("ssl.help.scope")}</p>
            </details>
          </div>
        )}

        <footer className="sslDialogFooter">
          <button
            disabled={actionPending}
            ref={cancelButtonRef}
            type="button"
            onClick={onClose}
          >
            {t("ssl.actions.cancel")}
          </button>
          <button
            className="primaryButton"
            disabled={actionPending || draft === null}
            type="button"
            onClick={() => void applyConfiguration()}
          >
            {actionPending
              ? t("ssl.actions.applying")
              : t("ssl.actions.apply")}
          </button>
        </footer>
      </section>
      <ConfirmDialog
        busy={actionPending}
        cancelLabel={t("ssl.regenerate.cancel")}
        confirmLabel={
          actionPending
            ? t("ssl.regenerate.running")
            : t("ssl.regenerate.confirm")
        }
        message={t("ssl.regenerate.message")}
        open={regenerateConfirmationOpen}
        title={t("ssl.regenerate.title")}
        onCancel={() => setRegenerateConfirmationOpen(false)}
        onConfirm={() => void confirmRegeneration()}
      />
    </div>
  );
}
