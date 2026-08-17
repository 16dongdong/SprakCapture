import { Download, PauseCircle, SlidersHorizontal } from "lucide-react";
import type { TFunction } from "i18next";
import { type KeyboardEvent, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  type BlockCookiesConfiguration,
  type BlockListConfiguration,
  type BreakpointsConfiguration,
  type DnsSpoofingConfiguration,
  type AutoSaveConfiguration,
  type ExportRequest,
  type LocationPattern,
  type MapLocalConfiguration,
  type MapRemoteConfiguration,
  type NoCachingConfiguration,
  type MirrorConfiguration,
  type RewriteConfiguration,
  type PacketFilterConfiguration,
  type ServiceSnapshot,
  type ThrottlingConfiguration,
} from "../api/protocol";
import { useServiceStore } from "../state/serviceStore";
import {
  isAdvancedTool,
  ToolAdvancedEditor,
  type AdvancedToolConfiguration,
  type AdvancedToolId,
} from "./toolAdvancedEditors";
import {
  BreakpointMessageEditor,
  type BreakpointPayload,
} from "./breakpointMessageEditor";
import {
  validateBreakpointMessage,
  type BreakpointMessageValidationField,
} from "./breakpointMessageValidation";
import {
  isVisualTool,
  ToolVisualEditor,
  type VisualToolConfiguration,
  type VisualToolId,
} from "./toolVisualEditors";
import {
  isM5Tool,
  M5ToolEditor,
  type M5ToolConfiguration,
  type M5ToolId,
} from "./m5ToolEditors";
import { useModalFocus } from "./modalFocus";
import { downloadArchive } from "./downloadArchive";
import type { TransactionToolSeed } from "./transactionToolSeed";
import { PacketFiltersEditor } from "./packetFiltersEditor";
import {
  validateToolConfiguration,
  type EditableToolConfiguration,
  type ToolConfigurationValidationIssue,
} from "./toolConfigurationValidation";

/** 标识工具栏中可打开的唯一 L3 工作流；导出与规则编辑共用同一宿主，避免并列对话框。 */
export type ToolDialogId =
  | "packetFilters"
  | "blockList"
  | "noCaching"
  | "blockCookies"
  | "dnsSpoofing"
  | "mapLocal"
  | "mapRemote"
  | "rewrite"
  | "breakpoints"
  | "throttling"
  | "mirror"
  | "autoSave"
  | "export";

interface ToolSettingsDialogProps {
  open: ToolDialogId | null;
  initialSeed?: TransactionToolSeed | null;
  onClose(): void;
}

interface BreakpointHitDialogProps {
  hidden?: boolean;
  onOpenToolSettings(): void;
}

// FNV-1a 的固定参数用于生成短而稳定的草稿主键；这里只要求进程内规则去重，不承担完整性校验。
const seedHashOffset = 2_166_136_261;
const seedHashPrime = 16_777_619;

/** 从权威快照提取一个可提交工具配置；节流预设为只读目录，不能随草稿回写。 */
function createToolConfiguration(
  snapshot: ServiceSnapshot,
  tool: Exclude<ToolDialogId, "export">,
): EditableToolConfiguration {
  if (tool === "throttling") {
    const { presets: _presets, ...configuration } = snapshot.tools.throttling;
    return configuration;
  }
  if (tool === "mirror") {
    const {
      writtenFiles: _writtenFiles,
      droppedWrites: _droppedWrites,
      lastError: _lastError,
      ...configuration
    } = snapshot.tools.mirror;
    return configuration;
  }
  if (tool === "autoSave") {
    const {
      lastSavedAtMilliseconds: _lastSavedAtMilliseconds,
      lastSavedPath: _lastSavedPath,
      lastError: _lastError,
      ...configuration
    } = snapshot.tools.autoSave;
    return configuration;
  }
  return snapshot.tools[tool];
}

/** 根据统一配置确定工具开关；屏蔽列表以 mode=off 表达关闭，其余工具使用 enabled。 */
function configurationEnabled(
  configuration: EditableToolConfiguration,
): boolean {
  return "enabled" in configuration && configuration.enabled;
}

/** 原子切换具备 enabled 字段的工具；屏蔽列表由 mode 选择器单独维护 blockList/allowList/off 语义。 */
function setConfigurationEnabled(
  configuration: EditableToolConfiguration,
  enabled: boolean,
): EditableToolConfiguration {
  if (!("enabled" in configuration)) {
    return configuration;
  }
  return { ...configuration, enabled };
}

/**
 * 复制可编辑配置，防止可视化表单直接修改快照中的权威对象。
 * 工具配置仅含普通数据字段，结构化复制可完整保留规则顺序与可空字段。
 */
function cloneConfiguration(
  configuration: EditableToolConfiguration,
): EditableToolConfiguration {
  return structuredClone(configuration);
}

/**
 * 生成配置内容指纹，用于仅在服务端工具配置实际变化时重置本地草稿。
 * 事务流量会频繁刷新快照；若只依赖快照版本，用户正在填写的规则会被无关流量覆盖。
 */
function configurationFingerprint(
  configuration: EditableToolConfiguration | null,
): string {
  return configuration === null ? "" : JSON.stringify(configuration);
}

/**
 * 为事务上下文规则生成稳定标识。
 * 运行上下文：React 严格模式可能重复执行草稿初始化，稳定标识允许初始化函数保持幂等。
 * 参数：prefix 区分工具类型，seed 同时区分事务与树节点位置；返回值仅用于本地工具规则主键。
 * 失败语义：不读取外部状态，相同参数始终返回相同标识。
 */
function createSeedIdentifier(
  prefix: string,
  seed: TransactionToolSeed,
): string {
  const transactionIdentity =
    seed.transactionId.replace(/[^a-zA-Z0-9-]/g, "-").slice(0, 64) ||
    "transaction";
  const identity = JSON.stringify(seed.location);
  let hash = seedHashOffset;
  for (let index = 0; index < identity.length; index += 1) {
    hash = Math.imul(hash ^ identity.charCodeAt(index), seedHashPrime);
  }
  return `${prefix}-${transactionIdentity}-${(hash >>> 0).toString(16)}`;
}

/**
 * 比较两个位置规则是否完全一致。
 * 运行上下文：无主键的位置列表接收右键种子前调用。
 * 参数：left 与 right 是待比较规则；失败语义：缺省查询串统一按 null 比较。
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
    (left.query ?? null) === (right.query ?? null)
  );
}

/**
 * 把事务位置追加到指定工具的可编辑草稿；只创建该工具能够真实执行的规则字段。
 *
 * 运行上下文：设置对话框首次消费右键上下文时调用，权威服务配置保持只读。
 * 参数：tool 决定规则形状，configuration 为克隆后的当前配置，seed 提供协议、主机、端口、路径和查询。
 * 失败语义：不使用位置规则的工具原样返回配置；Map Local 的目标文件仍由用户明确选择。
 */
function applyTransactionSeed(
  tool: Exclude<ToolDialogId, "export">,
  configuration: EditableToolConfiguration,
  seed: TransactionToolSeed,
): EditableToolConfiguration {
  const location = structuredClone(seed.location);
  if (tool === "noCaching") {
    const current = configuration as NoCachingConfiguration;
    if (
      current.locations.some((candidate) => locationsEqual(candidate, location))
    ) {
      return current;
    }
    return { ...current, locations: [...current.locations, location] };
  }
  if (tool === "blockCookies") {
    const current = configuration as BlockCookiesConfiguration;
    if (
      current.locations.some((candidate) => locationsEqual(candidate, location))
    ) {
      return current;
    }
    return { ...current, locations: [...current.locations, location] };
  }
  if (tool === "mapLocal") {
    const current = configuration as MapLocalConfiguration;
    const identifier = createSeedIdentifier("local-context", seed);
    if (current.rules.some((rule) => rule.id === identifier)) {
      return { ...current, enabled: true };
    }
    return {
      ...current,
      enabled: true,
      rules: [
        ...current.rules,
        {
          id: identifier,
          enabled: true,
          location,
          localPath: "",
          isDirectory: false,
          statusCode: 200,
          responseHeaders: [],
          contentTypeOverride: seed.contentType,
        },
      ],
    };
  }
  if (tool === "mapRemote") {
    const current = configuration as MapRemoteConfiguration;
    const identifier = createSeedIdentifier("remote-context", seed);
    if (current.rules.some((rule) => rule.id === identifier)) {
      return { ...current, enabled: true };
    }
    return {
      ...current,
      enabled: true,
      rules: [
        ...current.rules,
        {
          id: identifier,
          enabled: true,
          from: location,
          to: { protocol: "", host: "", port: "", path: "" },
        },
      ],
    };
  }
  if (tool === "rewrite") {
    const current = configuration as RewriteConfiguration;
    const identifier = createSeedIdentifier("rewrite-context", seed);
    if (current.sets.some((set) => set.id === identifier)) {
      return { ...current, enabled: true };
    }
    return {
      ...current,
      enabled: true,
      sets: [
        ...current.sets,
        {
          id: identifier,
          name: `${location.host}${location.path || "/*"}`.slice(0, 256),
          enabled: true,
          locations: [location],
          rules: [],
        },
      ],
    };
  }
  if (tool === "breakpoints") {
    const current = configuration as BreakpointsConfiguration;
    const identifier = createSeedIdentifier("breakpoint-context", seed);
    if (current.rules.some((rule) => rule.id === identifier)) {
      return { ...current, enabled: true };
    }
    return {
      ...current,
      enabled: true,
      rules: [
        ...current.rules,
        {
          id: identifier,
          enabled: true,
          location,
          onRequest: true,
          onResponse: true,
        },
      ],
    };
  }
  return configuration;
}

/** 将前置校验结果转换为当前工具的字段定位提示，索引始终从一开始展示以匹配规则列表。 */
function formatValidationMessage(
  t: TFunction,
  tool: Exclude<ToolDialogId, "export">,
  issue: ToolConfigurationValidationIssue,
): string {
  const fieldKey =
    issue.field === "configuration"
      ? "tools.configuration"
      : `tools.form.${issue.field}`;
  const fieldLabel = t(fieldKey);
  const positions = [
    issue.setIndex === undefined
      ? null
      : `${t("tools.form.ruleSets")} ${issue.setIndex + 1}`,
    issue.ruleIndex === undefined
      ? null
      : `${t("tools.form.rules")} ${issue.ruleIndex + 1}`,
  ].filter((position): position is string => position !== null);
  const field = [fieldLabel, ...positions].join(" · ");
  return t("tools.form.invalidField", { field });
}

/** 将断点草稿校验字段映射为现有表单标签，继续按钮始终在发起控制请求前给出可见定位。 */
function formatBreakpointValidationMessage(
  t: TFunction,
  field: BreakpointMessageValidationField,
): string {
  return t("tools.form.invalidField", { field: t(`tools.form.${field}`) });
}

/** 渲染统一工具配置对话框；八类 M3 工具都使用字段化草稿编辑，不向用户暴露协议对象文本。 */
export function ToolSettingsDialog({
  open,
  initialSeed = null,
  onClose,
}: ToolSettingsDialogProps) {
  const { t } = useTranslation();
  const {
    snapshot,
    actionPending,
    updateBlockList,
    updatePacketFilters,
    updateNoCaching,
    updateBlockCookies,
    updateDnsSpoofing,
    updateMapLocal,
    importMapLocalFiles,
    updateMapRemote,
    updateRewrite,
    updateBreakpoints,
    updateThrottling,
    updateMirror,
    updateAutoSave,
    saveAutoSaveNow,
    exportRecording,
  } = useServiceStore();
  const [draftConfiguration, setDraftConfiguration] = useState<{
    tool: Exclude<ToolDialogId, "export">;
    configuration: EditableToolConfiguration;
  } | null>(null);
  const [includeBodies, setIncludeBodies] = useState(true);
  const titleId = useId();
  const descriptionId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const configurationFormRef = useRef<HTMLFormElement>(null);
  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  const [validationAttempted, setValidationAttempted] = useState(false);
  const tool = open === "export" ? null : open;
  const configuration =
    snapshot !== null && tool !== null
      ? createToolConfiguration(snapshot, tool)
      : null;
  const visualTool: VisualToolId | null =
    tool !== null && isVisualTool(tool) ? tool : null;
  const advancedTool: AdvancedToolId | null =
    tool !== null && isAdvancedTool(tool) ? tool : null;
  const m5Tool: M5ToolId | null = tool !== null && isM5Tool(tool) ? tool : null;
  const configurationKey = configurationFingerprint(configuration);
  const activeConfiguration =
    draftConfiguration?.tool === tool
      ? draftConfiguration.configuration
      : configuration;
  const visualConfiguration =
    visualTool !== null && activeConfiguration !== null
      ? (activeConfiguration as VisualToolConfiguration)
      : null;
  const advancedConfiguration =
    advancedTool !== null && activeConfiguration !== null
      ? (activeConfiguration as AdvancedToolConfiguration)
      : null;
  const displayedConfiguration = activeConfiguration ?? configuration;
  const validationIssue =
    tool !== null && activeConfiguration !== null
      ? validateToolConfiguration(tool, activeConfiguration, {
          presetIds: snapshot?.tools.throttling.presets.map(
            (preset) => preset.id,
          ),
        })
      : null;
  const validationMessage =
    validationAttempted && tool !== null && validationIssue !== null
      ? formatValidationMessage(t, tool, validationIssue)
      : null;
  const { onKeyDown: handleModalFocusKeyDown } = useModalFocus({
    containerRef: dialogRef,
    initialFocusRef: cancelButtonRef,
    open: open !== null,
  });

  /**
   * 仅当正在编辑的工具或服务端工具配置确实变更时重建草稿。
   * 事务记录刷新不改变 configurationKey，因此不会覆盖用户尚未应用的表单输入。
   */
  useEffect(() => {
    const nextConfiguration =
      tool === null || configuration === null
        ? null
        : cloneConfiguration(configuration);
    setDraftConfiguration(
      open === null || tool === null || nextConfiguration === null
        ? null
        : {
            tool,
            configuration:
              initialSeed !== null
                ? applyTransactionSeed(tool, nextConfiguration, initialSeed)
                : nextConfiguration,
          },
    );
    setValidationAttempted(false);
  }, [configurationKey, initialSeed, open, tool]);

  if (open === null) {
    return null;
  }

  /** Escape 仅在没有正在进行控制操作时关闭，避免用户在提交过程中丢失未同步草稿。 */
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    handleModalFocusKeyDown(event);
    if (event.key === "Escape" && !actionPending) {
      event.preventDefault();
      onClose();
    }
  };

  /**
   * 把当前字段草稿交给对应 Store 动作；只有用户点击应用后才公开语义校验错误。
   * 原生字段约束在提交瞬间直接读取 DOM，避免嵌套规则窗口取消后遗留过期的无效状态。
   */
  const applyConfiguration = async () => {
    if (activeConfiguration === null || tool === null) {
      return;
    }
    const formIsValid =
      configurationFormRef.current?.reportValidity() ?? true;
    if (validationIssue !== null || !formIsValid) {
      setValidationAttempted(true);
      return;
    }
    setValidationAttempted(false);
    if (tool === "packetFilters") {
      await updatePacketFilters(
        activeConfiguration as PacketFilterConfiguration,
      );
    } else if (tool === "blockList") {
      await updateBlockList(activeConfiguration as BlockListConfiguration);
    } else if (tool === "noCaching") {
      await updateNoCaching(activeConfiguration as NoCachingConfiguration);
    } else if (tool === "blockCookies") {
      await updateBlockCookies(
        activeConfiguration as BlockCookiesConfiguration,
      );
    } else if (tool === "dnsSpoofing") {
      await updateDnsSpoofing(activeConfiguration as DnsSpoofingConfiguration);
    } else if (tool === "mapLocal") {
      await updateMapLocal(activeConfiguration as MapLocalConfiguration);
    } else if (tool === "mapRemote") {
      await updateMapRemote(activeConfiguration as MapRemoteConfiguration);
    } else if (tool === "rewrite") {
      await updateRewrite(activeConfiguration as RewriteConfiguration);
    } else if (tool === "breakpoints") {
      await updateBreakpoints(activeConfiguration as BreakpointsConfiguration);
    } else if (tool === "throttling") {
      await updateThrottling(activeConfiguration as ThrottlingConfiguration);
    } else if (tool === "mirror") {
      await updateMirror(activeConfiguration as MirrorConfiguration);
    } else if (tool === "autoSave") {
      await updateAutoSave(activeConfiguration as AutoSaveConfiguration);
    }
  };

  /** 仅修改草稿内总开关；规则与高级字段保持原样，应用前不会影响正在运行的流水线。 */
  const toggleConfiguration = (enabled: boolean) => {
    if (activeConfiguration === null || tool === null) {
      return;
    }
    setDraftConfiguration({
      tool,
      configuration: setConfigurationEnabled(activeConfiguration, enabled),
    });
  };

  /** 请求并下载 HAR；导出不改写代理配置，失败由共享 Store 写入可见错误。 */
  const exportHar = async () => {
    const request: ExportRequest = {
      format: "har",
      includeBodies,
    };
    try {
      downloadArchive(await exportRecording(request), "recording.har");
      onClose();
    } catch {
      // Store 已写入结构化错误；对话框保留在当前步骤供用户调整范围后重试。
    }
  };

  const titleKey =
    open === "export" ? "tools.export.title" : `tools.names.${open}`;
  const descriptionKey =
    open === "export" ? "tools.export.description" : "tools.description";
  const available =
    snapshot !== null && (open === "export" || configuration !== null);
  const canApply = available;

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
        className={
          open === "export"
            ? "toolSettingsDialog"
            : "toolSettingsDialog toolSettingsDialog--visual"
        }
        ref={dialogRef}
        role="dialog"
        tabIndex={-1}
      >
        <header className="toolDialogHeader">
          <div>
            <h2 id={titleId}>{t(titleKey)}</h2>
            <p id={descriptionId}>{t(descriptionKey)}</p>
          </div>
        </header>

        {!available ? (
          <div className="toolUnavailable">
            <SlidersHorizontal aria-hidden="true" size={24} />
            <strong>{t("tools.unavailable")}</strong>
          </div>
        ) : open === "export" ? (
          <div className="toolDialogBody toolExportBody">
            <Download aria-hidden="true" size={26} />
            <div>
              <strong>{t("tools.export.harTitle")}</strong>
              <p>{t("tools.export.harHint")}</p>
            </div>
            <label className="toolEnabledRow">
              <input
                checked={includeBodies}
                disabled={actionPending}
                type="checkbox"
                onChange={(event) => setIncludeBodies(event.target.checked)}
              />
              <span>{t("tools.export.includeBodies")}</span>
            </label>
          </div>
        ) : (
          <form
            className="toolDialogBody"
            key={`${tool ?? "unavailable"}-${configurationKey}`}
            ref={configurationFormRef}
            onSubmit={(event) => event.preventDefault()}
          >
            {displayedConfiguration !== null && tool !== "blockList" && (
              <label className="toolEnabledRow">
                <input
                  checked={configurationEnabled(displayedConfiguration)}
                  disabled={actionPending}
                  type="checkbox"
                  onChange={(event) =>
                    toggleConfiguration(event.target.checked)
                  }
                />
                <span>
                  <strong>{t("tools.enabled", { tool: t(titleKey) })}</strong>
                  <small>{t("tools.enabledHint")}</small>
                </span>
              </label>
            )}
            {tool === "packetFilters" && activeConfiguration !== null ? (
              <PacketFiltersEditor
                configuration={
                  activeConfiguration as PacketFilterConfiguration
                }
                disabled={actionPending}
                onApply={updatePacketFilters}
                onChange={(nextConfiguration) => {
                  setDraftConfiguration({
                    tool: "packetFilters",
                    configuration: nextConfiguration,
                  });
                }}
              />
            ) : visualTool !== null && visualConfiguration !== null ? (
              <ToolVisualEditor
                configuration={visualConfiguration}
                disabled={actionPending}
                tool={visualTool}
                onImportMapLocalFiles={async (selection) => {
                  const result = await importMapLocalFiles(selection);
                  return result?.localPath ?? null;
                }}
                onChange={(nextConfiguration) => {
                  if (tool !== null) {
                    setDraftConfiguration({
                      tool,
                      configuration: nextConfiguration,
                    });
                  }
                }}
              />
            ) : advancedTool !== null && advancedConfiguration !== null ? (
              <ToolAdvancedEditor
                configuration={advancedConfiguration}
                disabled={actionPending}
                presets={
                  advancedTool === "throttling" && snapshot !== null
                    ? snapshot.tools.throttling.presets
                    : undefined
                }
                tool={advancedTool}
                onChange={(nextConfiguration) => {
                  if (tool !== null) {
                    setDraftConfiguration({
                      tool,
                      configuration: nextConfiguration,
                    });
                  }
                }}
              />
            ) : m5Tool !== null && activeConfiguration !== null ? (
              <M5ToolEditor
                configuration={activeConfiguration as M5ToolConfiguration}
                disabled={actionPending}
                tool={m5Tool}
                onChange={(nextConfiguration) => {
                  if (tool !== null) {
                    setDraftConfiguration({
                      tool,
                      configuration: nextConfiguration,
                    });
                  }
                }}
              />
            ) : null}
            {validationMessage !== null && (
              <p className="toolValidationMessage" role="alert">
                {validationMessage}
              </p>
            )}
          </form>
        )}

        <footer className="toolDialogFooter">
          <button
            disabled={actionPending}
            ref={cancelButtonRef}
            type="button"
            onClick={onClose}
          >
            {t("tools.cancel")}
          </button>
          {open === "export" ? (
            <button
              className="primaryButton"
              disabled={actionPending || !available}
              type="button"
              onClick={() => void exportHar()}
            >
              <Download aria-hidden="true" size={14} />
              {actionPending ? t("tools.exporting") : t("tools.export.action")}
            </button>
          ) : (
            <>
              {open === "autoSave" && (
                <button
                  disabled={
                    actionPending ||
                    !available ||
                    !snapshot.tools.autoSave.enabled
                  }
                  type="button"
                  onClick={() => void saveAutoSaveNow()}
                >
                  {t("tools.autoSave.saveNow")}
                </button>
              )}
              <button
                className="primaryButton"
                disabled={actionPending || !canApply}
                type="button"
                onClick={() => void applyConfiguration()}
              >
                {actionPending ? t("tools.applying") : t("tools.apply")}
              </button>
            </>
          )}
        </footer>
      </section>
    </div>
  );
}

/** 渲染命中断点的消息编辑器；队列按事务标识保持稳定选择，继续与中止直接映射到数据面恢复语义。 */
export function BreakpointHitDialog({
  hidden = false,
  onOpenToolSettings,
}: BreakpointHitDialogProps) {
  const { t } = useTranslation();
  const {
    actionPending,
    abortBreakpoint,
    continueBreakpoint,
    suspendedBreakpoints,
  } = useServiceStore();
  const [selectedTransactionId, setSelectedTransactionId] = useState<
    string | null
  >(null);
  const [draft, setDraft] = useState<{
    transactionId: string;
    payload: BreakpointPayload;
  } | null>(null);
  const activeBreakpoint =
    suspendedBreakpoints.find(
      (breakpoint) => breakpoint.transactionId === selectedTransactionId,
    ) ??
    suspendedBreakpoints[0] ??
    null;
  const activeDraft =
    draft !== null &&
    activeBreakpoint !== null &&
    draft.transactionId === activeBreakpoint.transactionId
      ? draft.payload
      : (activeBreakpoint?.draft ?? null);
  const breakpointDialogRef = useRef<HTMLElement>(null);
  const breakpointFormRef = useRef<HTMLFormElement>(null);
  const continueButtonRef = useRef<HTMLButtonElement>(null);
  const [breakpointFormValid, setBreakpointFormValid] = useState(true);
  const breakpointValidationField =
    activeBreakpoint !== null && activeDraft !== null
      ? validateBreakpointMessage(activeDraft, activeBreakpoint.phase)
      : null;
  const breakpointValidationMessage =
    breakpointValidationField === null
      ? !breakpointFormValid
        ? t("tools.form.invalidField", { field: t("tools.breakpoints.draft") })
        : null
      : formatBreakpointValidationMessage(t, breakpointValidationField);
  const { onKeyDown: handleBreakpointFocusKeyDown } = useModalFocus({
    containerRef: breakpointDialogRef,
    initialFocusRef: continueButtonRef,
    open: !hidden && activeBreakpoint !== null && activeDraft !== null,
  });

  useEffect(() => {
    if (activeBreakpoint === null) {
      setSelectedTransactionId(null);
      setDraft(null);
      return;
    }
    setSelectedTransactionId(activeBreakpoint.transactionId);
    setDraft({
      transactionId: activeBreakpoint.transactionId,
      payload: activeBreakpoint.draft,
    });
  }, [activeBreakpoint?.transactionId]);

  /** 切换挂起事务时重新读取浏览器原生数值约束，避免上一份草稿的无效状态残留到新事务。 */
  useEffect(() => {
    setBreakpointFormValid(breakpointFormRef.current?.checkValidity() ?? true);
  }, [activeBreakpoint?.transactionId]);

  /** 同步数值输入的原生有效性，正文、URL 和 HTTP 头的语义错误由专用验证器显示。 */
  const syncBreakpointFormValidity = () => {
    setBreakpointFormValid(breakpointFormRef.current?.checkValidity() ?? true);
  };

  if (hidden || activeBreakpoint === null || activeDraft === null) {
    return null;
  }

  /** 将当前字段草稿继续写入被挂起的事务；消息语义和正文边界仍由服务端统一校验。 */
  const continueActiveBreakpoint = async () => {
    if (
      breakpointValidationField !== null ||
      (breakpointFormRef.current !== null &&
        !breakpointFormRef.current.reportValidity())
    ) {
      syncBreakpointFormValidity();
      return;
    }
    await continueBreakpoint(activeBreakpoint.transactionId, activeDraft);
  };

  return (
    <div className="dialogBackdrop breakpointBackdrop" role="presentation">
      <section
        aria-modal="true"
        aria-labelledby="breakpoint-hit-title"
        className="breakpointHitDialog"
        ref={breakpointDialogRef}
        role="dialog"
        tabIndex={-1}
        onKeyDown={handleBreakpointFocusKeyDown}
      >
        <header className="toolDialogHeader">
          <div>
            <h2 id="breakpoint-hit-title">{t("tools.breakpoints.hitTitle")}</h2>
            <p>{t("tools.breakpoints.hitDescription")}</p>
          </div>
          <button
            aria-label={t("tools.breakpoints.settings")}
            className="iconButton"
            disabled={actionPending}
            type="button"
            onClick={onOpenToolSettings}
          >
            <PauseCircle aria-hidden="true" size={17} />
          </button>
        </header>
        <form
          className="breakpointDialogBody"
          ref={breakpointFormRef}
          onInput={syncBreakpointFormValidity}
          onSubmit={(event) => event.preventDefault()}
        >
          <aside
            aria-label={t("tools.breakpoints.queue")}
            className="breakpointQueue"
          >
            {suspendedBreakpoints.map((breakpoint) => (
              <button
                aria-current={
                  breakpoint.transactionId === activeBreakpoint.transactionId
                    ? "true"
                    : undefined
                }
                className={
                  breakpoint.transactionId === activeBreakpoint.transactionId
                    ? "isSelected"
                    : ""
                }
                key={breakpoint.transactionId}
                type="button"
                onClick={() =>
                  setSelectedTransactionId(breakpoint.transactionId)
                }
              >
                <strong>
                  {breakpoint.phase === "request"
                    ? t("tools.breakpoints.request")
                    : t("tools.breakpoints.response")}
                </strong>
                <span>{breakpoint.transactionId}</span>
              </button>
            ))}
          </aside>
          <BreakpointMessageEditor
            disabled={actionPending}
            key={activeBreakpoint.transactionId}
            payload={activeDraft}
            phase={activeBreakpoint.phase}
            onChange={(payload) =>
              setDraft({
                transactionId: activeBreakpoint.transactionId,
                payload,
              })
            }
          />
          {breakpointValidationMessage !== null && (
            <p className="toolValidationMessage" role="alert">
              {breakpointValidationMessage}
            </p>
          )}
        </form>
        <footer className="toolDialogFooter">
          <button
            className="dangerTextButton"
            disabled={actionPending}
            type="button"
            onClick={() => void abortBreakpoint(activeBreakpoint.transactionId)}
          >
            {t("tools.breakpoints.abort")}
          </button>
          <button
            className="primaryButton"
            disabled={
              actionPending ||
              breakpointValidationField !== null ||
              !breakpointFormValid
            }
            ref={continueButtonRef}
            type="button"
            onClick={() => void continueActiveBreakpoint()}
          >
            {t("tools.breakpoints.continue")}
          </button>
        </footer>
      </section>
    </div>
  );
}
