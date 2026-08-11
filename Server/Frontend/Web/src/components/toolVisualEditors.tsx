import { FolderOpen, Plus, Trash2 } from "lucide-react";
import { useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  type BlockCookiesConfiguration,
  type BlockListConfiguration,
  type DnsSpoofingConfiguration,
  type HeaderField,
  type LocationPattern,
  type MapLocalConfiguration,
  type MapRemoteConfiguration,
  type NoCachingConfiguration,
} from "../api/protocol";
import type { MapLocalImportSelection } from "../api/controlClient";
import { IntegerField } from "./integerField";
import { moveListItem, RuleList } from "./ruleList";
import { RuleEditorDialog } from "./ruleEditorDialog";

/** 采用可视化编辑器的工具标识，其他工具继续由各自的专用编辑器负责。 */
export type VisualToolId =
  | "blockList"
  | "noCaching"
  | "blockCookies"
  | "dnsSpoofing"
  | "mapLocal"
  | "mapRemote";

/** 可视化编辑器能够提交的协议配置联合类型。 */
export type VisualToolConfiguration =
  | BlockCookiesConfiguration
  | BlockListConfiguration
  | DnsSpoofingConfiguration
  | MapLocalConfiguration
  | MapRemoteConfiguration
  | NoCachingConfiguration;

interface ToolVisualEditorProps {
  tool: VisualToolId;
  configuration: VisualToolConfiguration;
  disabled: boolean;
  onImportMapLocalFiles(selection: MapLocalImportSelection): Promise<string | null>;
  onChange(configuration: VisualToolConfiguration): void;
}

interface LocationFieldsProps {
  legend: string;
  location: LocationPattern;
  disabled: boolean;
  onChange(location: LocationPattern): void;
}

interface LocationScopeEditorProps {
  locations: LocationPattern[];
  disabled: boolean;
  onChange(locations: LocationPattern[]): void;
}

interface HeaderFieldsEditorProps {
  headers: HeaderField[];
  disabled: boolean;
  onChange(headers: HeaderField[]): void;
}

/** M3 工具流水线当前只接入 HTTP 代理，位置作用域限定为 HTTP 与 WebSocket。 */
const commonProtocols = ["", "http", "https", "ws", "wss"];

/** 映射目标必须是可建立上游连接的 HTTP/WebSocket 协议，不能将目标改写为 SOCKS。 */
const mapRemoteTargetProtocols = ["", "http", "https", "ws", "wss"];

/** 创建空的 Location 草稿；空字段沿用协议中的“任意匹配”语义。 */
function createLocation(): LocationPattern {
  return {
    protocol: "",
    host: "",
    port: "",
    path: "",
    query: null,
  };
}

/** 将 Location 压缩成规则列表中稳定且易扫读的一行摘要。 */
function formatLocation(location: LocationPattern): string {
  const protocol = location.protocol === "" ? "*" : location.protocol;
  const host = location.host === "" ? "*" : location.host;
  const port = location.port === "" ? "" : `:${location.port}`;
  const path = location.path === "" ? "/*" : location.path;
  const query = location.query === null || location.query === "" ? "" : `?${location.query}`;
  return `${protocol}://${host}${port}${path}${query}`;
}

/** 为新增映射规则生成当前集合内唯一的稳定标识，避免删除后出现重复 ID。 */
function createRuleId(prefix: string, existingIds: readonly string[]): string {
  let suffix = existingIds.length + 1;
  let candidate = `${prefix}-${suffix}`;
  while (existingIds.includes(candidate)) {
    suffix += 1;
    candidate = `${prefix}-${suffix}`;
  }
  return candidate;
}

/** 创建可立即编辑的 DNS 映射规则；保留显式空字段以触发浏览器和协议层校验。 */
function createDnsSpoofingRule(
  existingIds: readonly string[],
): DnsSpoofingConfiguration["rules"][number] {
  return {
    id: createRuleId("dns", existingIds),
    enabled: true,
    hostPattern: "",
    ipAddress: "",
  };
}

/** 创建本地映射的可编辑默认规则；文件路径由用户在替换区域明确填写。 */
function createMapLocalRule(
  existingIds: readonly string[],
): MapLocalConfiguration["rules"][number] {
  return {
    id: createRuleId("local", existingIds),
    enabled: true,
    location: createLocation(),
    localPath: "",
    isDirectory: false,
    statusCode: 200,
    responseHeaders: [],
    contentTypeOverride: "",
  };
}

/** 创建远程映射的可编辑默认规则；空目标字段表示保留对应的原始字段。 */
function createMapRemoteRule(
  existingIds: readonly string[],
): MapRemoteConfiguration["rules"][number] {
  return {
    id: createRuleId("remote", existingIds),
    enabled: true,
    from: createLocation(),
    to: {
      protocol: "",
      host: "",
      port: "",
      path: "",
    },
  };
}

/** 判断工具是否使用 Charles 风格的结构化配置表单。 */
export function isVisualTool(tool: string): tool is VisualToolId {
  return [
    "blockList",
    "noCaching",
    "blockCookies",
    "dnsSpoofing",
    "mapLocal",
    "mapRemote",
  ].includes(tool);
}

/** 渲染一组可复用的 Location 输入字段，所有修改只回写当前草稿。 */
function LocationFields({
  legend,
  location,
  disabled,
  onChange,
}: LocationFieldsProps) {
  const { t } = useTranslation();
  const protocolOptions = commonProtocols.includes(location.protocol)
    ? commonProtocols
    : [location.protocol, ...commonProtocols];

  /** 更新单个 Location 字段，并将空查询标准化为协议要求的 null。 */
  const updateLocation = <Field extends keyof LocationPattern>(
    field: Field,
    value: LocationPattern[Field],
  ) => {
    onChange({ ...location, [field]: value });
  };

  return (
    <fieldset className="toolLocationFields">
      <legend>{legend}</legend>
      <label>
        <span>{t("tools.form.protocol")}</span>
        <select
          aria-label={`${legend} ${t("tools.form.protocol")}`}
          disabled={disabled}
          value={location.protocol}
          onChange={(event) => updateLocation("protocol", event.target.value)}
        >
          {protocolOptions.map((protocol) => (
            <option key={protocol || "any"} value={protocol}>
              {protocol === "" ? t("tools.form.protocolAny") : protocol}
            </option>
          ))}
        </select>
      </label>
      <label>
        <span>{t("tools.form.host")}</span>
        <input
          aria-label={`${legend} ${t("tools.form.host")}`}
          disabled={disabled}
          placeholder={t("tools.form.hostPlaceholder")}
          value={location.host}
          onChange={(event) => updateLocation("host", event.target.value)}
        />
      </label>
      <label>
        <span>{t("tools.form.port")}</span>
        <input
          aria-label={`${legend} ${t("tools.form.port")}`}
          disabled={disabled}
          placeholder={t("tools.form.portPlaceholder")}
          value={location.port}
          onChange={(event) => updateLocation("port", event.target.value)}
        />
      </label>
      <label>
        <span>{t("tools.form.path")}</span>
        <input
          aria-label={`${legend} ${t("tools.form.path")}`}
          disabled={disabled}
          placeholder={t("tools.form.pathPlaceholder")}
          value={location.path}
          onChange={(event) => updateLocation("path", event.target.value)}
        />
      </label>
      <label className="toolLocationQueryField">
        <span>{t("tools.form.query")}</span>
        <input
          aria-label={`${legend} ${t("tools.form.query")}`}
          disabled={disabled}
          placeholder={t("tools.form.queryPlaceholder")}
          value={location.query ?? ""}
          onChange={(event) =>
            updateLocation(
              "query",
              event.target.value === "" ? null : event.target.value,
            )
          }
        />
      </label>
    </fieldset>
  );
}

/** 编辑一组共享作用域，空列表在协议中明确表示作用于全部位置。 */
export function LocationScopeEditor({
  locations,
  disabled,
  onChange,
}: LocationScopeEditorProps) {
  const { t } = useTranslation();
  const [editor, setEditor] = useState<{
    index: number | null;
    draft: LocationPattern;
  } | null>(null);

  /** 移动作用域；列表顺序是规则匹配优先级的一部分。 */
  const moveLocation = (fromIndex: number, toIndex: number) => {
    onChange(moveListItem(locations, fromIndex, toIndex));
  };

  /** 确认后一次性追加或替换 Location，取消对话框不会污染父级配置草稿。 */
  const saveLocation = () => {
    if (editor === null) {
      return;
    }
    onChange(
      editor.index === null
        ? [...locations, editor.draft]
        : locations.map((location, index) =>
            index === editor.index ? editor.draft : location,
          ),
    );
    setEditor(null);
  };

  return (
    <section className="toolRuleEditor isDialogBased">
      <RuleList
        addLabel={t("tools.form.addLocation")}
        disabled={disabled}
        emptyHint={t("tools.form.allLocations")}
        items={locations}
        moveDownLabel={t("tools.form.moveDown")}
        moveUpLabel={t("tools.form.moveUp")}
        removeLabel={t("tools.form.removeLocation")}
        selectedIndex={editor?.index ?? -1}
        title={t("tools.form.scope")}
        itemLabel={(_, location) => formatLocation(location)}
        onAdd={() => setEditor({ index: null, draft: createLocation() })}
        onMove={moveLocation}
        onRemove={(index) =>
          onChange(locations.filter((_, locationIndex) => locationIndex !== index))
        }
        onSelect={(index) =>
          setEditor({ index, draft: { ...(locations[index] as LocationPattern) } })
        }
      />
      <RuleEditorDialog
        cancelLabel={t("tools.form.cancelRule")}
        confirmLabel={t("tools.form.saveRule")}
        disabled={disabled}
        open={editor !== null}
        title={t("tools.form.locationDialogTitle")}
        onCancel={() => setEditor(null)}
        onConfirm={saveLocation}
      >
        {editor !== null && (
          <LocationFields
            disabled={disabled}
            legend={t("tools.form.scope")}
            location={editor.draft}
            onChange={(draft) => setEditor({ ...editor, draft })}
          />
        )}
      </RuleEditorDialog>
    </section>
  );
}

/** 编辑 Map Local 命中后附加到合成响应的可重复 HTTP 头字段。 */
function HeaderFieldsEditor({
  headers,
  disabled,
  onChange,
}: HeaderFieldsEditorProps) {
  const { t } = useTranslation();

  /** 更新指定响应头字段，使用新数组避免修改来自快照的对象。 */
  const updateHeader = (
    index: number,
    field: keyof HeaderField,
    value: string,
  ) => {
    onChange(
      headers.map((header, headerIndex) =>
        headerIndex === index ? { ...header, [field]: value } : header,
      ),
    );
  };

  return (
    <section className="toolHeaderEditor">
      <div className="toolSectionHeading">
        <strong>{t("tools.form.responseHeaders")}</strong>
        <button
          disabled={disabled}
          type="button"
          onClick={() => onChange([...headers, { name: "", value: "" }])}
        >
          <Plus aria-hidden="true" size={14} />
          {t("tools.form.addHeader")}
        </button>
      </div>
      {headers.length > 0 && (
        <div className="toolHeaderTableWrap">
          <table>
            <thead>
              <tr>
                <th scope="col">{t("tools.form.headerName")}</th>
                <th scope="col">{t("tools.form.headerValue")}</th>
                <th scope="col">
                  <span className="visuallyHidden">{t("tools.form.removeHeader")}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {headers.map((header, index) => (
                <tr key={index}>
                  <td>
                    <input
                      aria-label={`${t("tools.form.headerName")} ${index + 1}`}
                      disabled={disabled}
                      value={header.name}
                      onChange={(event) => updateHeader(index, "name", event.target.value)}
                    />
                  </td>
                  <td>
                    <input
                      aria-label={`${t("tools.form.headerValue")} ${index + 1}`}
                      disabled={disabled}
                      value={header.value}
                      onChange={(event) => updateHeader(index, "value", event.target.value)}
                    />
                  </td>
                  <td>
                    <button
                      aria-label={`${t("tools.form.removeHeader")} ${index + 1}`}
                      className="iconButton"
                      disabled={disabled}
                      type="button"
                      onClick={() =>
                        onChange(headers.filter((_, headerIndex) => headerIndex !== index))
                      }
                    >
                      <Trash2 aria-hidden="true" size={14} />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

/** 提供屏蔽列表的模式、合成响应与作用域表单。 */
function BlockListEditor({
  configuration,
  disabled,
  onChange,
}: {
  configuration: BlockListConfiguration;
  disabled: boolean;
  onChange(configuration: BlockListConfiguration): void;
}) {
  const { t } = useTranslation();

  return (
    <div className="toolVisualEditor">
      <section className="toolOptionPanel">
        <div className="toolFieldGrid">
          <label>
            <span>{t("tools.form.blockMode")}</span>
            <select
              aria-label={t("tools.form.blockMode")}
              disabled={disabled}
              value={configuration.mode}
              onChange={(event) =>
                onChange({
                  ...configuration,
                  mode: event.target.value as BlockListConfiguration["mode"],
                })
              }
            >
              <option value="off">{t("tools.form.blockModeOff")}</option>
              <option value="blockList">{t("tools.form.blockModeBlockList")}</option>
              <option value="allowList">{t("tools.form.blockModeAllowList")}</option>
            </select>
          </label>
          <label>
            <span>{t("tools.form.statusCode")}</span>
            <IntegerField
              disabled={disabled}
              label={t("tools.form.statusCode")}
              max={599}
              min={100}
              value={configuration.statusCode}
              wrap={false}
              onChange={(statusCode) => onChange({ ...configuration, statusCode })}
            />
          </label>
        </div>
        <label className="toolTextAreaField">
          <span>{t("tools.form.responseBody")}</span>
          <textarea
            aria-label={t("tools.form.responseBody")}
            disabled={disabled}
            value={configuration.responseBody}
            onChange={(event) => onChange({ ...configuration, responseBody: event.target.value })}
          />
        </label>
        <label className="toolCheckboxRow">
          <input
            checked={configuration.closeConnection}
            disabled={disabled}
            type="checkbox"
            onChange={(event) => onChange({ ...configuration, closeConnection: event.target.checked })}
          />
          <span>{t("tools.form.closeConnection")}</span>
        </label>
      </section>
      <LocationScopeEditor
        disabled={disabled}
        locations={configuration.locations}
        onChange={(locations) => onChange({ ...configuration, locations })}
      />
    </div>
  );
}

/** 提供无缓存请求头与响应头策略的可视化开关和作用域编辑。 */
function NoCachingEditor({
  configuration,
  disabled,
  onChange,
}: {
  configuration: NoCachingConfiguration;
  disabled: boolean;
  onChange(configuration: NoCachingConfiguration): void;
}) {
  const { t } = useTranslation();

  return (
    <div className="toolVisualEditor">
      <section className="toolOptionPanel toolOptionColumns">
        <div>
          <strong>{t("tools.form.request")}</strong>
          <label className="toolCheckboxRow">
            <input
              checked={configuration.stripRequestHeaders}
              disabled={disabled}
              type="checkbox"
              onChange={(event) =>
                onChange({ ...configuration, stripRequestHeaders: event.target.checked })
              }
            />
            <span>{t("tools.form.stripHeaders")}</span>
          </label>
          <label className="toolCheckboxRow">
            <input
              checked={configuration.injectRequestNoCache}
              disabled={disabled}
              type="checkbox"
              onChange={(event) =>
                onChange({ ...configuration, injectRequestNoCache: event.target.checked })
              }
            />
            <span>{t("tools.form.injectNoCache")}</span>
          </label>
        </div>
        <div>
          <strong>{t("tools.form.response")}</strong>
          <label className="toolCheckboxRow">
            <input
              checked={configuration.stripResponseHeaders}
              disabled={disabled}
              type="checkbox"
              onChange={(event) =>
                onChange({ ...configuration, stripResponseHeaders: event.target.checked })
              }
            />
            <span>{t("tools.form.stripHeaders")}</span>
          </label>
          <label className="toolCheckboxRow">
            <input
              checked={configuration.injectResponseNoStore}
              disabled={disabled}
              type="checkbox"
              onChange={(event) =>
                onChange({ ...configuration, injectResponseNoStore: event.target.checked })
              }
            />
            <span>{t("tools.form.injectNoStore")}</span>
          </label>
        </div>
      </section>
      <LocationScopeEditor
        disabled={disabled}
        locations={configuration.locations}
        onChange={(locations) => onChange({ ...configuration, locations })}
      />
    </div>
  );
}

/** 提供请求 Cookie 与响应 Set-Cookie 两个独立方向的可视化开关。 */
function BlockCookiesEditor({
  configuration,
  disabled,
  onChange,
}: {
  configuration: BlockCookiesConfiguration;
  disabled: boolean;
  onChange(configuration: BlockCookiesConfiguration): void;
}) {
  const { t } = useTranslation();

  return (
    <div className="toolVisualEditor">
      <section className="toolOptionPanel toolOptionColumns">
        <label className="toolCheckboxRow">
          <input
            checked={configuration.stripRequestCookie}
            disabled={disabled}
            type="checkbox"
            onChange={(event) =>
              onChange({ ...configuration, stripRequestCookie: event.target.checked })
            }
          />
          <span>{t("tools.form.cookieRequest")}</span>
        </label>
        <label className="toolCheckboxRow">
          <input
            checked={configuration.stripResponseSetCookie}
            disabled={disabled}
            type="checkbox"
            onChange={(event) =>
              onChange({ ...configuration, stripResponseSetCookie: event.target.checked })
            }
          />
          <span>{t("tools.form.cookieResponse")}</span>
        </label>
      </section>
      <LocationScopeEditor
        disabled={disabled}
        locations={configuration.locations}
        onChange={(locations) => onChange({ ...configuration, locations })}
      />
    </div>
  );
}

/** 提供 DNS 主机模式到 IP 的有序规则编辑器；规则顺序直接决定首条命中结果。 */
function DnsSpoofingEditor({
  configuration,
  disabled,
  onChange,
}: {
  configuration: DnsSpoofingConfiguration;
  disabled: boolean;
  onChange(configuration: DnsSpoofingConfiguration): void;
}) {
  const { t } = useTranslation();
  type DnsRule = DnsSpoofingConfiguration["rules"][number];
  const [editor, setEditor] = useState<{
    index: number | null;
    draft: DnsRule;
  } | null>(null);

  /** 移动规则，确保界面顺序与后端首条命中语义一致。 */
  const moveRule = (fromIndex: number, toIndex: number) => {
    onChange({
      ...configuration,
      rules: moveListItem(configuration.rules, fromIndex, toIndex),
    });
  };

  /** 仅在子对话框确认后写回完整 DNS 规则，避免空规则进入父窗口配置。 */
  const saveRule = () => {
    if (editor === null) {
      return;
    }
    const rules =
      editor.index === null
        ? [...configuration.rules, editor.draft]
        : configuration.rules.map((rule, index) =>
            index === editor.index ? editor.draft : rule,
          );
    onChange({ ...configuration, rules });
    setEditor(null);
  };

  return (
    <section className="toolRuleEditor toolMappingEditor isDialogBased">
      <RuleList
        addLabel={t("tools.form.addRule")}
        disabled={disabled}
        emptyHint={t("tools.form.noRules")}
        items={configuration.rules}
        moveDownLabel={t("tools.form.moveDown")}
        moveUpLabel={t("tools.form.moveUp")}
        removeLabel={t("tools.form.removeRule")}
        selectedIndex={editor?.index ?? -1}
        title={t("tools.form.rules")}
        itemLabel={(index, rule) =>
          `${index + 1}. ${rule.hostPattern || t("tools.form.hostPattern")}`
        }
        onAdd={() =>
          setEditor({
            index: null,
            draft: createDnsSpoofingRule(
              configuration.rules.map((rule) => rule.id),
            ),
          })
        }
        onMove={moveRule}
        onRemove={(index) =>
          onChange({
            ...configuration,
            rules: configuration.rules.filter((_, ruleIndex) => ruleIndex !== index),
          })
        }
        onSelect={(index) =>
          setEditor({ index, draft: { ...(configuration.rules[index] as DnsRule) } })
        }
      />
      <RuleEditorDialog
        cancelLabel={t("tools.form.cancelRule")}
        confirmLabel={t("tools.form.saveRule")}
        disabled={disabled}
        open={editor !== null}
        title={t("tools.form.ruleDialogTitle")}
        onCancel={() => setEditor(null)}
        onConfirm={saveRule}
      >
        {editor !== null && (
          <div className="toolVisualEditor">
            <label className="toolCheckboxRow">
              <input
                checked={editor.draft.enabled}
                disabled={disabled}
                type="checkbox"
                onChange={(event) =>
                  setEditor({
                    ...editor,
                    draft: { ...editor.draft, enabled: event.target.checked },
                  })
                }
              />
              <span>{t("tools.form.ruleEnabled")}</span>
            </label>
            <section className="toolOptionPanel">
              <div className="toolFieldGrid">
                <label>
                  <span>{t("tools.form.hostPattern")}</span>
                  <input
                    aria-label={t("tools.form.hostPattern")}
                    disabled={disabled}
                    maxLength={255}
                    placeholder={t("tools.form.hostPlaceholder")}
                    required
                    value={editor.draft.hostPattern}
                    onChange={(event) =>
                      setEditor({
                        ...editor,
                        draft: { ...editor.draft, hostPattern: event.target.value },
                      })
                    }
                  />
                </label>
                <label>
                  <span>{t("tools.form.ipAddress")}</span>
                  <input
                    aria-label={t("tools.form.ipAddress")}
                    disabled={disabled}
                    placeholder="127.0.0.1"
                    required
                    value={editor.draft.ipAddress}
                    onChange={(event) =>
                      setEditor({
                        ...editor,
                        draft: { ...editor.draft, ipAddress: event.target.value },
                      })
                    }
                  />
                </label>
              </div>
              <p className="toolFieldHint">{t("tools.dnsSpoofing.hint")}</p>
            </section>
          </div>
        )}
      </RuleEditorDialog>
    </section>
  );
}

/** 提供 Map Local 的规则列表；规则字段在独立子对话框确认后才写回配置。 */
function MapLocalEditor({
  configuration,
  disabled,
  onImportMapLocalFiles,
  onChange,
}: {
  configuration: MapLocalConfiguration;
  disabled: boolean;
  onImportMapLocalFiles(selection: MapLocalImportSelection): Promise<string | null>;
  onChange(configuration: MapLocalConfiguration): void;
}) {
  const { t } = useTranslation();
  type MapLocalRule = MapLocalConfiguration["rules"][number];
  const [editor, setEditor] = useState<{
    index: number | null;
    draft: MapLocalRule;
  } | null>(null);
  const localPathInputId = useId();
  const filePickerRef = useRef<HTMLInputElement>(null);

  /** 当前规则只在子对话框中更新，父配置不会出现尚未确认的半成品。 */
  const updateSelectedRule = (draft: MapLocalRule) => {
    setEditor((current) => (current === null ? null : { ...current, draft }));
  };

  /** 移动本地映射规则，顺序直接决定首个命中的替换。 */
  const moveRule = (fromIndex: number, toIndex: number) => {
    onChange({
      ...configuration,
      rules: moveListItem(configuration.rules, fromIndex, toIndex),
    });
  };

  /** 依据当前规则类型打开 Chrome 文件或目录选择器；每次点击先清空旧值，允许连续选择同一资源。 */
  const openLocalPathPicker = () => {
    const picker = filePickerRef.current;
    if (picker === null || editor === null) {
      return;
    }
    picker.value = "";
    picker.multiple = editor.draft.isDirectory;
    if (editor.draft.isDirectory) {
      picker.setAttribute("webkitdirectory", "");
    } else {
      picker.removeAttribute("webkitdirectory");
    }
    picker.click();
  };

  /** 将选择结果上传到后端受管映射根，并仅在完整导入成功后回填当前规则路径；取消选择保持草稿不变。 */
  const importSelectedLocalPath = async (files: FileList | null) => {
    if (files === null || files.length === 0 || editor === null) {
      return;
    }
    const directory = editor.draft.isDirectory;
    const selectedFiles = Array.from(files, (file) => ({
      file,
      relativePath:
        directory && file.webkitRelativePath !== ""
          ? file.webkitRelativePath
          : file.name,
    }));
    const localPath = await onImportMapLocalFiles({ directory, files: selectedFiles });
    if (localPath !== null) {
      setEditor((current) =>
        current === null
          ? null
          : { ...current, draft: { ...current.draft, localPath } },
      );
    }
  };

  /** 确认后原子追加或替换规则；required 校验失败时本函数不会被调用。 */
  const saveRule = () => {
    if (editor === null) {
      return;
    }
    const rules = editor.index === null
      ? [...configuration.rules, editor.draft]
      : configuration.rules.map((rule, index) =>
          index === editor.index ? editor.draft : rule,
        );
    // 新增规则表示用户已经完成显式配置，同时开启总开关可避免规则保存成功却在数据面静默失效。
    onChange({
      ...configuration,
      enabled: editor.index === null || configuration.enabled,
      rules,
    });
    setEditor(null);
  };

  return (
    <section className="toolRuleEditor toolMappingEditor isDialogBased">
      <RuleList
        addLabel={t("tools.form.addRule")}
        disabled={disabled}
        emptyHint={t("tools.form.noRules")}
        items={configuration.rules}
        moveDownLabel={t("tools.form.moveDown")}
        moveUpLabel={t("tools.form.moveUp")}
        removeLabel={t("tools.form.removeRule")}
        selectedIndex={editor?.index ?? -1}
        title={t("tools.form.rules")}
        itemLabel={(index, rule) => `${index + 1}. ${formatLocation(rule.location)}`}
        onAdd={() => setEditor({
          index: null,
          draft: createMapLocalRule(configuration.rules.map((rule) => rule.id)),
        })}
        onMove={moveRule}
        onRemove={(index) => onChange({
          ...configuration,
          rules: configuration.rules.filter((_, ruleIndex) => ruleIndex !== index),
        })}
        onSelect={(index) => {
          const rule = configuration.rules[index] as MapLocalRule;
          setEditor({
            index,
            draft: {
              ...rule,
              location: { ...rule.location },
              responseHeaders: rule.responseHeaders.map((header) => ({ ...header })),
            },
          });
        }}
      />
      <RuleEditorDialog
        cancelLabel={t("tools.form.cancelRule")}
        confirmLabel={t("tools.form.saveRule")}
        disabled={disabled}
        open={editor !== null}
        title={t("tools.form.ruleDialogTitle")}
        onCancel={() => setEditor(null)}
        onConfirm={saveRule}
      >
        {editor !== null && (
          <div className="toolVisualEditor">
            <label className="toolCheckboxRow">
              <input
                checked={editor.draft.enabled}
                disabled={disabled}
                type="checkbox"
                onChange={(event) =>
                  updateSelectedRule({ ...editor.draft, enabled: event.target.checked })
                }
              />
              <span>{t("tools.form.ruleEnabled")}</span>
            </label>
            <LocationFields
              disabled={disabled}
              legend={t("tools.form.matchLocation")}
              location={editor.draft.location}
              onChange={(location) => updateSelectedRule({ ...editor.draft, location })}
            />
            <section className="toolOptionPanel">
              <div className="toolFieldGrid">
                <div className="toolPathField toolWideField">
                  <label htmlFor={localPathInputId}>{t("tools.form.localPath")}</label>
                  <div className="toolPathPicker">
                    <input
                      id={localPathInputId}
                      aria-label={t("tools.form.localPath")}
                      disabled={disabled}
                      placeholder={t("tools.form.localPathPlaceholder")}
                      required
                      value={editor.draft.localPath}
                      onChange={(event) =>
                        updateSelectedRule({ ...editor.draft, localPath: event.target.value })
                      }
                    />
                    <button
                      className="toolPathPickerButton"
                      disabled={disabled}
                      title={
                        editor.draft.isDirectory
                          ? t("tools.form.chooseDirectory")
                          : t("tools.form.chooseFile")
                      }
                      type="button"
                      onClick={openLocalPathPicker}
                    >
                      <FolderOpen aria-hidden="true" size={16} strokeWidth={1.8} />
                      <span>
                        {editor.draft.isDirectory
                          ? t("tools.form.chooseDirectory")
                          : t("tools.form.chooseFile")}
                      </span>
                    </button>
                  </div>
                  <input
                    ref={filePickerRef}
                    className="toolPathNativePicker"
                    disabled={disabled}
                    tabIndex={-1}
                    type="file"
                    onChange={(event) => void importSelectedLocalPath(event.target.files)}
                  />
                </div>
                <label>
                  <span>{t("tools.form.statusCode")}</span>
                  <IntegerField
                    disabled={disabled}
                    label={t("tools.form.statusCode")}
                    max={599}
                    min={100}
                    value={editor.draft.statusCode}
                    wrap={false}
                    onChange={(statusCode) =>
                      updateSelectedRule({ ...editor.draft, statusCode })
                    }
                  />
                </label>
                <label>
                  <span>{t("tools.form.contentTypeOverride")}</span>
                  <input
                    aria-label={t("tools.form.contentTypeOverride")}
                    disabled={disabled}
                    value={editor.draft.contentTypeOverride}
                    onChange={(event) =>
                      updateSelectedRule({
                        ...editor.draft,
                        contentTypeOverride: event.target.value,
                      })
                    }
                  />
                </label>
              </div>
              <label className="toolCheckboxRow">
                <input
                  checked={editor.draft.isDirectory}
                  disabled={disabled}
                  type="checkbox"
                  onChange={(event) =>
                    updateSelectedRule({ ...editor.draft, isDirectory: event.target.checked })
                  }
                />
                <span>{t("tools.form.directoryMapping")}</span>
              </label>
            </section>
            <HeaderFieldsEditor
              disabled={disabled}
              headers={editor.draft.responseHeaders}
              onChange={(responseHeaders) =>
                updateSelectedRule({ ...editor.draft, responseHeaders })
              }
            />
          </div>
        )}
      </RuleEditorDialog>
    </section>
  );
}

/** 提供 Map Remote 的规则列表，以及映射来源和映射目标两个独立的可视化区域。 */
function MapRemoteEditor({
  configuration,
  disabled,
  onChange,
}: {
  configuration: MapRemoteConfiguration;
  disabled: boolean;
  onChange(configuration: MapRemoteConfiguration): void;
}) {
  const { t } = useTranslation();
  type MapRemoteRule = MapRemoteConfiguration["rules"][number];
  const [editor, setEditor] = useState<{
    index: number | null;
    draft: MapRemoteRule;
  } | null>(null);
  const selectedRule = editor?.draft ?? null;
  const targetProtocols =
    selectedRule === null || mapRemoteTargetProtocols.includes(selectedRule.to.protocol)
      ? mapRemoteTargetProtocols
      : [selectedRule.to.protocol, ...mapRemoteTargetProtocols];

  /** 当前映射只更新子对话框草稿，用户取消时父配置保持原值。 */
  const updateSelectedRule = (draft: MapRemoteRule) => {
    setEditor((current) => (current === null ? null : { ...current, draft }));
  };

  /** 移动远程映射规则；数组顺序就是后端匹配优先级。 */
  const moveRule = (fromIndex: number, toIndex: number) => {
    onChange({
      ...configuration,
      rules: moveListItem(configuration.rules, fromIndex, toIndex),
    });
  };

  /** 更新映射目标的单个字段；空值在服务端表示不覆写原始目标。 */
  const updateTarget = (
    field: keyof MapRemoteConfiguration["rules"][number]["to"],
    value: string,
  ) => {
    if (selectedRule !== null) {
      updateSelectedRule({
        ...selectedRule,
        to: { ...selectedRule.to, [field]: value },
      });
    }
  };

  /** 确认后原子写回映射来源和目标，避免两侧字段出现不同提交代际。 */
  const saveRule = () => {
    if (editor === null) {
      return;
    }
    const rules = editor.index === null
      ? [...configuration.rules, editor.draft]
      : configuration.rules.map((rule, index) =>
          index === editor.index ? editor.draft : rule,
        );
    // 规则两端确认后立即启用映射；编辑既有规则时仍尊重用户手动关闭的总开关。
    onChange({
      ...configuration,
      enabled: editor.index === null || configuration.enabled,
      rules,
    });
    setEditor(null);
  };

  return (
    <section className="toolRuleEditor toolMappingEditor isDialogBased">
      <RuleList
        addLabel={t("tools.form.addRule")}
        disabled={disabled}
        emptyHint={t("tools.form.noRules")}
        items={configuration.rules}
        moveDownLabel={t("tools.form.moveDown")}
        moveUpLabel={t("tools.form.moveUp")}
        removeLabel={t("tools.form.removeRule")}
        selectedIndex={editor?.index ?? -1}
        title={t("tools.form.rules")}
        itemLabel={(index, rule) => `${index + 1}. ${formatLocation(rule.from)}`}
        onAdd={() => setEditor({
          index: null,
          draft: createMapRemoteRule(configuration.rules.map((rule) => rule.id)),
        })}
        onMove={moveRule}
        onRemove={(index) => onChange({
          ...configuration,
          rules: configuration.rules.filter((_, ruleIndex) => ruleIndex !== index),
        })}
        onSelect={(index) => {
          const rule = configuration.rules[index] as MapRemoteRule;
          setEditor({
            index,
            draft: { ...rule, from: { ...rule.from }, to: { ...rule.to } },
          });
        }}
      />
      <RuleEditorDialog
        cancelLabel={t("tools.form.cancelRule")}
        confirmLabel={t("tools.form.saveRule")}
        disabled={disabled}
        open={editor !== null}
        title={t("tools.form.ruleDialogTitle")}
        onCancel={() => setEditor(null)}
        onConfirm={saveRule}
      >
        {selectedRule !== null && (
          <div className="toolVisualEditor">
            <label className="toolCheckboxRow">
              <input
                checked={selectedRule.enabled}
                disabled={disabled}
                type="checkbox"
                onChange={(event) =>
                  updateSelectedRule({ ...selectedRule, enabled: event.target.checked })
                }
              />
              <span>{t("tools.form.ruleEnabled")}</span>
            </label>
            <LocationFields
              disabled={disabled}
              legend={t("tools.form.mapFrom")}
              location={selectedRule.from}
              onChange={(from) => updateSelectedRule({ ...selectedRule, from })}
            />
            <fieldset className="toolLocationFields toolTargetFields">
              <legend>{t("tools.form.mapTo")}</legend>
              <label>
                <span>{t("tools.form.protocol")}</span>
                <select
                  aria-label={`${t("tools.form.mapTo")} ${t("tools.form.protocol")}`}
                  disabled={disabled}
                  value={selectedRule.to.protocol}
                  onChange={(event) => updateTarget("protocol", event.target.value)}
                >
                  {targetProtocols.map((protocol) => (
                    <option key={protocol || "keep-original"} value={protocol}>
                      {protocol === "" ? t("tools.form.keepOriginal") : protocol}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                <span>{t("tools.form.host")}</span>
                <input
                  aria-label={`${t("tools.form.mapTo")} ${t("tools.form.host")}`}
                  disabled={disabled}
                  placeholder={t("tools.form.keepOriginal")}
                  value={selectedRule.to.host}
                  onChange={(event) => updateTarget("host", event.target.value)}
                />
              </label>
              <label>
                <span>{t("tools.form.port")}</span>
                <input
                  aria-label={`${t("tools.form.mapTo")} ${t("tools.form.port")}`}
                  disabled={disabled}
                  placeholder={t("tools.form.keepOriginal")}
                  value={selectedRule.to.port}
                  onChange={(event) => updateTarget("port", event.target.value)}
                />
              </label>
              <label>
                <span>{t("tools.form.path")}</span>
                <input
                  aria-label={`${t("tools.form.mapTo")} ${t("tools.form.path")}`}
                  disabled={disabled}
                  placeholder={t("tools.form.keepOriginal")}
                  value={selectedRule.to.path}
                  onChange={(event) => updateTarget("path", event.target.value)}
                />
              </label>
            </fieldset>
          </div>
        )}
      </RuleEditorDialog>
    </section>
  );
}

/** 按工具标识选择对应的结构化编辑器，并维持统一的受控草稿接口。 */
export function ToolVisualEditor({
  tool,
  configuration,
  disabled,
  onImportMapLocalFiles,
  onChange,
}: ToolVisualEditorProps) {
  if (tool === "blockList") {
    return (
      <BlockListEditor
        configuration={configuration as BlockListConfiguration}
        disabled={disabled}
        onChange={onChange}
      />
    );
  }
  if (tool === "noCaching") {
    return (
      <NoCachingEditor
        configuration={configuration as NoCachingConfiguration}
        disabled={disabled}
        onChange={onChange}
      />
    );
  }
  if (tool === "blockCookies") {
    return (
      <BlockCookiesEditor
        configuration={configuration as BlockCookiesConfiguration}
        disabled={disabled}
        onChange={onChange}
      />
    );
  }
  if (tool === "dnsSpoofing") {
    return (
      <DnsSpoofingEditor
        configuration={configuration as DnsSpoofingConfiguration}
        disabled={disabled}
        onChange={onChange}
      />
    );
  }
  if (tool === "mapLocal") {
    return (
      <MapLocalEditor
        configuration={configuration as MapLocalConfiguration}
        disabled={disabled}
        onImportMapLocalFiles={onImportMapLocalFiles}
        onChange={onChange}
      />
    );
  }
  return (
    <MapRemoteEditor
      configuration={configuration as MapRemoteConfiguration}
      disabled={disabled}
      onChange={onChange}
    />
  );
}
