import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  type BreakpointsConfiguration,
  type LocationPattern,
  type RewriteConfiguration,
  type ThrottlingConfiguration,
  type ThrottlingPublicState,
} from "../api/protocol";
import { IntegerField } from "./integerField";
import { moveListItem, RuleList } from "./ruleList";
import { RuleEditorDialog } from "./ruleEditorDialog";

/** 使用高级可视化表单的工具标识；这些工具的配置均包含嵌套规则或运行参数。 */
export type AdvancedToolId = "rewrite" | "breakpoints" | "throttling";

/** 高级表单能够提交的完整协议配置联合类型。 */
export type AdvancedToolConfiguration =
  | BreakpointsConfiguration
  | RewriteConfiguration
  | ThrottlingConfiguration;

type RewriteSet = RewriteConfiguration["sets"][number];
type RewriteRule = RewriteSet["rules"][number];
type BreakpointRule = BreakpointsConfiguration["rules"][number];
type ThrottlePreset = ThrottlingPublicState["presets"][number];
type ThrottleProfile = ThrottlingConfiguration["custom"];

interface ToolAdvancedEditorProps {
  tool: AdvancedToolId;
  configuration: AdvancedToolConfiguration;
  disabled: boolean;
  presets?: readonly ThrottlePreset[];
  onChange(configuration: AdvancedToolConfiguration): void;
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

interface RewriteRuleEditorProps {
  rule: RewriteRule;
  disabled: boolean;
  onChange(rule: RewriteRule): void;
}

interface BreakpointRuleEditorProps {
  rule: BreakpointRule;
  disabled: boolean;
  onChange(rule: BreakpointRule): void;
}

/** M3 工具流水线当前只接入 HTTP 代理，位置作用域限定为 HTTP 与 WebSocket。 */
const commonProtocols = ["", "http", "https", "ws", "wss"];
const rewriteRuleTypes: readonly RewriteRule["type"][] = [
  "urlHost",
  "urlPath",
  "urlQuery",
  "requestHeader",
  "responseHeader",
  "requestBody",
  "responseBody",
  "responseStatus",
];

/** 创建空的 Location 草稿；空字段保持协议中的任意匹配语义。 */
function createLocation(): LocationPattern {
  return {
    protocol: "",
    host: "",
    port: "",
    path: "",
    query: null,
  };
}

/** 将 Location 生成稳定摘要，用于规则列表中快速识别当前作用域。 */
function formatLocation(location: LocationPattern): string {
  const protocol = location.protocol === "" ? "*" : location.protocol;
  const host = location.host === "" ? "*" : location.host;
  const port = location.port === "" ? "" : `:${location.port}`;
  const path = location.path === "" ? "/*" : location.path;
  const query = location.query === null || location.query === "" ? "" : `?${location.query}`;
  return `${protocol}://${host}${port}${path}${query}`;
}

/** 在同类规则集合中生成唯一 ID，避免视觉编辑删除后重用已经存在的协议标识。 */
function createIdentifier(prefix: string, existingIdentifiers: readonly string[]): string {
  let suffix = existingIdentifiers.length + 1;
  let identifier = `${prefix}-${suffix}`;
  while (existingIdentifiers.includes(identifier)) {
    suffix += 1;
    identifier = `${prefix}-${suffix}`;
  }
  return identifier;
}

/** 用不可变数组替换指定条目，避免直接修改来自权威快照的配置对象。 */
function replaceListItem<Item>(items: readonly Item[], index: number, item: Item): Item[] {
  return items.map((candidate, candidateIndex) =>
    candidateIndex === index ? item : candidate,
  );
}

/** 创建默认 Rewrite 规则；所有协议字段均显式初始化，避免新增规则漏字段。 */
function createRewriteRule(existingIdentifiers: readonly string[]): RewriteRule {
  return {
    id: createIdentifier("rewrite-rule", existingIdentifiers),
    enabled: true,
    type: "urlPath",
    matchRegex: "",
    replace: "",
    headerName: null,
    matchValueRegex: null,
    headerAction: null,
    caseSensitive: false,
    matchAllOccurrences: true,
  };
}

/** 创建默认 Rewrite 规则集；名称由调用方本地化，ID 仍保持纯协议标识。 */
function createRewriteSet(
  existingIdentifiers: readonly string[],
  name: string,
): RewriteSet {
  return {
    id: createIdentifier("rewrite-set", existingIdentifiers),
    name,
    enabled: true,
    locations: [],
    rules: [],
  };
}

/** 创建默认断点规则，默认仅在请求阶段暂停以避免新增规则无匹配阶段。 */
function createBreakpointRule(existingIdentifiers: readonly string[]): BreakpointRule {
  return {
    id: createIdentifier("breakpoint-rule", existingIdentifiers),
    enabled: true,
    location: createLocation(),
    onRequest: true,
    onResponse: false,
  };
}

/** 判断工具是否必须使用高级可视化表单，避免宿主回退到 JSON 文本编辑。 */
export function isAdvancedTool(tool: string): tool is AdvancedToolId {
  return tool === "rewrite" || tool === "breakpoints" || tool === "throttling";
}

/** 渲染一组完整的 Location 字段，修改时只替换当前 Location 草稿。 */
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

  /** 更新单个 Location 字段，并把空查询标准化为协议要求的 null。 */
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
            updateLocation("query", event.target.value === "" ? null : event.target.value)
          }
        />
      </label>
    </fieldset>
  );
}

/** 编辑可重复 Location 作用域；空列表保持“所有位置”的协议语义而不是插入伪规则。 */
function LocationScopeEditor({
  locations,
  disabled,
  onChange,
}: LocationScopeEditorProps) {
  const { t } = useTranslation();
  const [editor, setEditor] = useState<{
    index: number | null;
    draft: LocationPattern;
  } | null>(null);

  /** 移动作用域；位置顺序由用户明确维护而不是删除后重建。 */
  const moveLocation = (fromIndex: number, toIndex: number) => {
    onChange(moveListItem(locations, fromIndex, toIndex));
  };

  /** 确认后一次性写回 Location；取消二级对话框不会改变规则集草稿。 */
  const saveLocation = () => {
    if (editor === null) {
      return;
    }
    onChange(
      editor.index === null
        ? [...locations, editor.draft]
        : replaceListItem(locations, editor.index, editor.draft),
    );
    setEditor(null);
  };

  return (
    <section className="toolRuleEditor isDialogBased">
      <RuleList
        addLabel={t("tools.form.addLocation")}
        disabled={disabled}
        emptyHint={t("tools.form.allLocations")}
        itemLabel={(_, location) => formatLocation(location)}
        items={locations}
        moveDownLabel={t("tools.form.moveDown")}
        moveUpLabel={t("tools.form.moveUp")}
        removeLabel={t("tools.form.removeLocation")}
        selectedIndex={editor?.index ?? -1}
        title={t("tools.form.scope")}
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

/** 编辑单条 Rewrite 规则的所有协议字段；头字段仅在头规则类型下可见但始终保持原值。 */
function RewriteRuleEditor({
  rule,
  disabled,
  onChange,
}: RewriteRuleEditorProps) {
  const { t } = useTranslation();
  const isHeaderRule = rule.type === "requestHeader" || rule.type === "responseHeader";

  return (
    <section className="toolOptionPanel">
      <div className="toolFieldGrid">
        <label>
          <span>{t("tools.form.rewriteType")}</span>
          <select
            aria-label={t("tools.form.rewriteType")}
            disabled={disabled}
            value={rule.type}
            onChange={(event) =>
              onChange({ ...rule, type: event.target.value as RewriteRule["type"] })
            }
          >
            {rewriteRuleTypes.map((type) => (
              <option key={type} value={type}>
                {t(`tools.form.${type}`)}
              </option>
            ))}
          </select>
        </label>
        <label className="toolCheckboxRow">
          <input
            checked={rule.enabled}
            disabled={disabled}
            type="checkbox"
            onChange={(event) => onChange({ ...rule, enabled: event.target.checked })}
          />
          <span>{t("tools.form.ruleEnabled")}</span>
        </label>
        <label className="toolWideField">
          <span>{t("tools.form.matchExpression")}</span>
          <input
            aria-label={t("tools.form.matchExpression")}
            disabled={disabled}
            value={rule.matchRegex}
            onChange={(event) => onChange({ ...rule, matchRegex: event.target.value })}
          />
        </label>
        <label className="toolWideField">
          <span>{t("tools.form.replacement")}</span>
          <input
            aria-label={t("tools.form.replacement")}
            disabled={disabled}
            value={rule.replace}
            onChange={(event) => onChange({ ...rule, replace: event.target.value })}
          />
        </label>
      </div>
      {isHeaderRule && (
        <div className="toolFieldGrid">
          <label>
            <span>{t("tools.form.headerName")}</span>
            <input
              aria-label={t("tools.form.headerName")}
              disabled={disabled}
              value={rule.headerName ?? ""}
              onChange={(event) =>
                onChange({
                  ...rule,
                  headerName: event.target.value === "" ? null : event.target.value,
                })
              }
            />
          </label>
          <label>
            <span>{t("tools.form.headerAction")}</span>
            <select
              aria-label={t("tools.form.headerAction")}
              disabled={disabled}
              value={rule.headerAction ?? ""}
              onChange={(event) =>
                onChange({
                  ...rule,
                  headerAction:
                    event.target.value === ""
                      ? null
                      : (event.target.value as NonNullable<RewriteRule["headerAction"]>),
                })
              }
            >
              <option value="">{t("tools.form.headerAction")}</option>
              <option value="add">{t("tools.form.headerActionAdd")}</option>
              <option value="modify">{t("tools.form.headerActionModify")}</option>
              <option value="remove">{t("tools.form.headerActionRemove")}</option>
            </select>
          </label>
          <label className="toolWideField">
            <span>{t("tools.form.matchValue")}</span>
            <input
              aria-label={t("tools.form.matchValue")}
              disabled={disabled}
              value={rule.matchValueRegex ?? ""}
              onChange={(event) =>
                onChange({
                  ...rule,
                  matchValueRegex: event.target.value === "" ? null : event.target.value,
                })
              }
            />
          </label>
        </div>
      )}
      <div className="toolOptionColumns">
        <label className="toolCheckboxRow">
          <input
            checked={rule.caseSensitive}
            disabled={disabled}
            type="checkbox"
            onChange={(event) => onChange({ ...rule, caseSensitive: event.target.checked })}
          />
          <span>{t("tools.form.caseSensitive")}</span>
        </label>
        <label className="toolCheckboxRow">
          <input
            checked={rule.matchAllOccurrences}
            disabled={disabled}
            type="checkbox"
            onChange={(event) =>
              onChange({ ...rule, matchAllOccurrences: event.target.checked })
            }
          />
          <span>{t("tools.form.allOccurrences")}</span>
        </label>
      </div>
    </section>
  );
}

/** 编辑 Rewrite 的规则集、作用域和规则优先级；数组顺序直接保持后端执行顺序。 */
function RewriteEditor({
  configuration,
  disabled,
  onChange,
}: {
  configuration: RewriteConfiguration;
  disabled: boolean;
  onChange(configuration: RewriteConfiguration): void;
}) {
  const { t } = useTranslation();
  const [selectedSetIndex, setSelectedSetIndex] = useState(-1);
  const [selectedRuleIndex, setSelectedRuleIndex] = useState(-1);
  const selectedSet = configuration.sets[selectedSetIndex] ?? null;
  const selectedRule = selectedSet?.rules[selectedRuleIndex] ?? null;

  useEffect(() => {
    setSelectedSetIndex((currentIndex) => {
      if (configuration.sets.length === 0) {
        return -1;
      }
      return currentIndex < 0 ? 0 : Math.min(currentIndex, configuration.sets.length - 1);
    });
  }, [configuration.sets.length]);

  useEffect(() => {
    setSelectedRuleIndex((currentIndex) => {
      if (selectedSet === null || selectedSet.rules.length === 0) {
        return -1;
      }
      return currentIndex < 0 ? 0 : Math.min(currentIndex, selectedSet.rules.length - 1);
    });
  }, [selectedSet?.id, selectedSet?.rules.length]);

  /** 用当前选中规则集替换同一索引，保证其他规则集的优先级和配置原样保留。 */
  const updateSelectedSet = (set: RewriteSet) => {
    if (selectedSetIndex < 0) {
      return;
    }
    onChange({
      ...configuration,
      sets: replaceListItem(configuration.sets, selectedSetIndex, set),
    });
  };

  /** 在列表末尾新增规则集，并选择新集的第一处可编辑位置。 */
  const addSet = () => {
    const nextSets = [
      ...configuration.sets,
      createRewriteSet(
        configuration.sets.map((set) => set.id),
        `${t("tools.form.ruleSets")} ${configuration.sets.length + 1}`,
      ),
    ];
    // 新建规则集是明确启用意图，直接加入热更新流水线，避免只保存配置却没有执行效果。
    onChange({ ...configuration, enabled: true, sets: nextSets });
    setSelectedSetIndex(nextSets.length - 1);
    setSelectedRuleIndex(-1);
  };

  /** 删除规则集时移动选择到相邻集，防止明细面板保留过期对象。 */
  const removeSet = (index: number) => {
    const nextSets = configuration.sets.filter((_, setIndex) => setIndex !== index);
    onChange({ ...configuration, sets: nextSets });
    setSelectedSetIndex(nextSets.length === 0 ? -1 : Math.min(index, nextSets.length - 1));
    setSelectedRuleIndex(-1);
  };

  /** 移动重写集并重置内部规则选择，集合顺序就是同一请求上的执行顺序。 */
  const moveSet = (fromIndex: number, toIndex: number) => {
    onChange({
      ...configuration,
      sets: moveListItem(configuration.sets, fromIndex, toIndex),
    });
    setSelectedSetIndex(toIndex);
    setSelectedRuleIndex(0);
  };

  /** 切换规则集时重置规则选择，避免相同索引指向另一个集合的旧规则。 */
  const selectSet = (index: number) => {
    setSelectedSetIndex(index);
    setSelectedRuleIndex(0);
  };

  /** 为当前规则集追加全局唯一的规则标识，规则顺序即流水线匹配顺序。 */
  const addRule = () => {
    if (selectedSet === null) {
      return;
    }
    const existingIdentifiers = configuration.sets.flatMap((set) =>
      set.rules.map((rule) => rule.id),
    );
    const nextRules = [...selectedSet.rules, createRewriteRule(existingIdentifiers)];
    updateSelectedSet({ ...selectedSet, rules: nextRules });
    setSelectedRuleIndex(nextRules.length - 1);
  };

  /** 删除当前集内规则后选择相邻规则，避免编辑区滞留在已删除条目。 */
  const removeRule = (index: number) => {
    if (selectedSet === null) {
      return;
    }
    const nextRules = selectedSet.rules.filter((_, ruleIndex) => ruleIndex !== index);
    updateSelectedSet({ ...selectedSet, rules: nextRules });
    setSelectedRuleIndex(nextRules.length === 0 ? -1 : Math.min(index, nextRules.length - 1));
  };

  /** 移动当前重写集中的规则并保持明细面板跟随规则，避免调整优先级后误改其它规则。 */
  const moveRule = (fromIndex: number, toIndex: number) => {
    if (selectedSet === null) {
      return;
    }
    updateSelectedSet({
      ...selectedSet,
      rules: moveListItem(selectedSet.rules, fromIndex, toIndex),
    });
    setSelectedRuleIndex(toIndex);
  };

  /** 替换选中规则，保留本规则集内其余规则和全局集合排序。 */
  const updateSelectedRule = (rule: RewriteRule) => {
    if (selectedSet === null || selectedRuleIndex < 0) {
      return;
    }
    updateSelectedSet({
      ...selectedSet,
      rules: replaceListItem(selectedSet.rules, selectedRuleIndex, rule),
    });
  };

  return (
    <div className="toolVisualEditor">
      <section className="toolRuleEditor">
        <RuleList
          addLabel={t("tools.form.addSet")}
          disabled={disabled}
          emptyHint={t("tools.form.noRules")}
          itemLabel={(_, set) => set.name}
          items={configuration.sets}
          moveDownLabel={t("tools.form.moveDown")}
          moveUpLabel={t("tools.form.moveUp")}
          removeLabel={t("tools.form.removeSet")}
          selectedIndex={selectedSetIndex}
          title={t("tools.form.ruleSets")}
          onAdd={addSet}
          onMove={moveSet}
          onRemove={removeSet}
          onSelect={selectSet}
        />
        <div className="toolRuleDetailPane">
          {selectedSet !== null && (
            <div className="toolVisualEditor">
              <section className="toolOptionPanel">
                <div className="toolFieldGrid">
                  <label>
                    <span>{t("tools.form.setName")}</span>
                    <input
                      aria-label={t("tools.form.setName")}
                      disabled={disabled}
                      value={selectedSet.name}
                      onChange={(event) =>
                        updateSelectedSet({ ...selectedSet, name: event.target.value })
                      }
                    />
                  </label>
                  <label className="toolCheckboxRow">
                    <input
                      checked={selectedSet.enabled}
                      disabled={disabled}
                      type="checkbox"
                      onChange={(event) =>
                        updateSelectedSet({ ...selectedSet, enabled: event.target.checked })
                      }
                    />
                    <span>{t("tools.form.ruleEnabled")}</span>
                  </label>
                </div>
              </section>
              <LocationScopeEditor
                key={selectedSet.id}
                disabled={disabled}
                locations={selectedSet.locations}
                onChange={(locations) => updateSelectedSet({ ...selectedSet, locations })}
              />
              <section className="toolRuleEditor">
                <RuleList
                  addLabel={t("tools.form.addRule")}
                  disabled={disabled}
                  emptyHint={t("tools.form.noRules")}
                  itemLabel={(_, rule) => t(`tools.form.${rule.type}`)}
                  items={selectedSet.rules}
                  moveDownLabel={t("tools.form.moveDown")}
                  moveUpLabel={t("tools.form.moveUp")}
                  removeLabel={t("tools.form.removeRule")}
                  selectedIndex={selectedRuleIndex}
                  title={t("tools.form.rules")}
                  onAdd={addRule}
                  onMove={moveRule}
                  onRemove={removeRule}
                  onSelect={setSelectedRuleIndex}
                />
                <div className="toolRuleDetailPane">
                  {selectedRule !== null && (
                    <RewriteRuleEditor
                      disabled={disabled}
                      rule={selectedRule}
                      onChange={updateSelectedRule}
                    />
                  )}
                </div>
              </section>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}

/** 编辑单条断点规则的启用态、Location 以及请求/响应阶段，至少保留一个暂停阶段。 */
function BreakpointRuleEditor({
  rule,
  disabled,
  onChange,
}: BreakpointRuleEditorProps) {
  const { t } = useTranslation();

  /** 更新断点阶段并阻止两个阶段同时关闭，保持后端可执行的不变量。 */
  const updatePhase = (phase: "onRequest" | "onResponse", enabled: boolean) => {
    const otherPhase = phase === "onRequest" ? "onResponse" : "onRequest";
    if (!enabled && !rule[otherPhase]) {
      return;
    }
    onChange({ ...rule, [phase]: enabled });
  };

  return (
    <div className="toolVisualEditor">
      <section className="toolOptionPanel">
        <label className="toolCheckboxRow">
          <input
            checked={rule.enabled}
            disabled={disabled}
            type="checkbox"
            onChange={(event) => onChange({ ...rule, enabled: event.target.checked })}
          />
          <span>{t("tools.form.ruleEnabled")}</span>
        </label>
        <div className="toolOptionColumns">
          <label className="toolCheckboxRow">
            <input
              checked={rule.onRequest}
              disabled={disabled}
              type="checkbox"
              onChange={(event) => updatePhase("onRequest", event.target.checked)}
            />
            <span>{t("tools.form.request")}</span>
          </label>
          <label className="toolCheckboxRow">
            <input
              checked={rule.onResponse}
              disabled={disabled}
              type="checkbox"
              onChange={(event) => updatePhase("onResponse", event.target.checked)}
            />
            <span>{t("tools.form.response")}</span>
          </label>
        </div>
      </section>
      <LocationFields
        disabled={disabled}
        legend={t("tools.form.matchLocation")}
        location={rule.location}
        onChange={(location) => onChange({ ...rule, location })}
      />
    </div>
  );
}

/** 编辑断点队列的超时边界和规则列表；规则 ID 仅自动生成并随配置稳定保留。 */
function BreakpointsEditor({
  configuration,
  disabled,
  onChange,
}: {
  configuration: BreakpointsConfiguration;
  disabled: boolean;
  onChange(configuration: BreakpointsConfiguration): void;
}) {
  const { t } = useTranslation();
  const [selectedIndex, setSelectedIndex] = useState(-1);
  const selectedRule = configuration.rules[selectedIndex] ?? null;

  useEffect(() => {
    setSelectedIndex((currentIndex) => {
      if (configuration.rules.length === 0) {
        return -1;
      }
      return currentIndex < 0 ? 0 : Math.min(currentIndex, configuration.rules.length - 1);
    });
  }, [configuration.rules.length]);

  /** 替换选中断点规则，确保其他规则顺序与执行优先级保持不变。 */
  const updateSelectedRule = (rule: BreakpointRule) => {
    if (selectedIndex < 0) {
      return;
    }
    onChange({
      ...configuration,
      rules: replaceListItem(configuration.rules, selectedIndex, rule),
    });
  };

  /** 追加带唯一标识的默认断点规则，并将明细面板切换到新增规则。 */
  const addRule = () => {
    const nextRules = [
      ...configuration.rules,
      createBreakpointRule(configuration.rules.map((rule) => rule.id)),
    ];
    // 新增断点后同步开启总开关，后端会在当前服务代际内立即读取这份配置。
    onChange({ ...configuration, enabled: true, rules: nextRules });
    setSelectedIndex(nextRules.length - 1);
  };

  /** 删除断点规则后选择相邻项，避免视觉编辑器悬挂在已删除对象上。 */
  const removeRule = (index: number) => {
    const nextRules = configuration.rules.filter((_, ruleIndex) => ruleIndex !== index);
    onChange({ ...configuration, rules: nextRules });
    setSelectedIndex(nextRules.length === 0 ? -1 : Math.min(index, nextRules.length - 1));
  };

  /** 移动断点规则并让编辑区继续对应当前规则，命中优先级随数组顺序即时可见。 */
  const moveRule = (fromIndex: number, toIndex: number) => {
    onChange({
      ...configuration,
      rules: moveListItem(configuration.rules, fromIndex, toIndex),
    });
    setSelectedIndex(toIndex);
  };

  return (
    <div className="toolVisualEditor">
      <section className="toolOptionPanel">
        <div className="toolFieldGrid">
          <IntegerField
            disabled={disabled}
            label={t("tools.form.suspendTimeout")}
            max={3600}
            min={1}
            value={configuration.suspendTimeoutSeconds}
            onChange={(suspendTimeoutSeconds) =>
              onChange({ ...configuration, suspendTimeoutSeconds })
            }
          />
          <IntegerField
            disabled={disabled}
            label={t("tools.form.maxSuspended")}
            max={1024}
            min={1}
            value={configuration.maxSuspended}
            onChange={(maxSuspended) =>
              onChange({ ...configuration, maxSuspended })
            }
          />
          <label className="toolWideField">
            <span>{t("tools.form.timeoutAction")}</span>
            <select
              aria-label={t("tools.form.timeoutAction")}
              disabled={disabled}
              value={configuration.onTimeout}
              onChange={(event) =>
                onChange({
                  ...configuration,
                  onTimeout: event.target.value as BreakpointsConfiguration["onTimeout"],
                })
              }
            >
              <option value="continue">{t("tools.form.timeoutContinue")}</option>
              <option value="abort">{t("tools.form.timeoutAbort")}</option>
            </select>
          </label>
        </div>
      </section>
      <section className="toolRuleEditor">
        <RuleList
          addLabel={t("tools.form.addRule")}
          disabled={disabled}
          emptyHint={t("tools.form.noRules")}
          itemLabel={(_, rule) => formatLocation(rule.location)}
          items={configuration.rules}
          moveDownLabel={t("tools.form.moveDown")}
          moveUpLabel={t("tools.form.moveUp")}
          removeLabel={t("tools.form.removeRule")}
          selectedIndex={selectedIndex}
          title={t("tools.form.rules")}
          onAdd={addRule}
          onMove={moveRule}
          onRemove={removeRule}
          onSelect={setSelectedIndex}
        />
        <div className="toolRuleDetailPane">
          {selectedRule !== null && (
            <BreakpointRuleEditor
              disabled={disabled}
              rule={selectedRule}
              onChange={updateSelectedRule}
            />
          )}
        </div>
      </section>
    </div>
  );
}

/** 编辑节流自定义速率、内置预设选择和作用域；预设目录只读，提交时只回写协议配置字段。 */
function ThrottlingEditor({
  configuration,
  disabled,
  presets,
  onChange,
}: {
  configuration: ThrottlingConfiguration;
  disabled: boolean;
  presets: readonly ThrottlePreset[];
  onChange(configuration: ThrottlingConfiguration): void;
}) {
  const { t } = useTranslation();
  const activePresetMissing =
    configuration.activePresetId !== null &&
    !presets.some((preset) => preset.id === configuration.activePresetId);

  /** 修改自定义速率时清除预设选择；后端有预设时忽略 custom，必须先切换为自定义才能使本次编辑生效。 */
  const updateProfile = <Field extends keyof ThrottleProfile>(
    field: Field,
    value: ThrottleProfile[Field],
  ) => {
    onChange({
      ...configuration,
      activePresetId: null,
      custom: { ...configuration.custom, [field]: value },
    });
  };

  return (
    <div className="toolVisualEditor">
      <section className="toolOptionPanel">
        <div className="toolFieldGrid">
          <label>
            <span>{t("tools.form.preset")}</span>
            <select
              aria-label={t("tools.form.preset")}
              disabled={disabled}
              value={configuration.activePresetId ?? ""}
              onChange={(event) =>
                onChange({
                  ...configuration,
                  activePresetId: event.target.value === "" ? null : event.target.value,
                })
              }
            >
              <option value="">{t("tools.form.noPreset")}</option>
              {activePresetMissing && configuration.activePresetId !== null && (
                <option value={configuration.activePresetId}>{configuration.activePresetId}</option>
              )}
              {presets.map((preset) => (
                <option key={preset.id} value={preset.id}>
                  {preset.name}
                </option>
              ))}
            </select>
          </label>
        </div>
      </section>
      <section className="toolOptionPanel">
        <strong>{t("tools.form.custom")}</strong>
        <div className="toolFieldGrid">
          <IntegerField
            disabled={disabled}
            label={t("tools.form.downloadSpeed")}
            max={Number.MAX_SAFE_INTEGER}
            min={1}
            value={configuration.custom.downloadBytesPerSecond}
            onChange={(downloadBytesPerSecond) =>
              updateProfile("downloadBytesPerSecond", downloadBytesPerSecond)
            }
          />
          <IntegerField
            disabled={disabled}
            label={t("tools.form.uploadSpeed")}
            max={Number.MAX_SAFE_INTEGER}
            min={1}
            value={configuration.custom.uploadBytesPerSecond}
            onChange={(uploadBytesPerSecond) =>
              updateProfile("uploadBytesPerSecond", uploadBytesPerSecond)
            }
          />
          <IntegerField
            disabled={disabled}
            label={t("tools.form.latency")}
            max={300_000}
            min={0}
            value={configuration.custom.latencyMilliseconds}
            onChange={(latencyMilliseconds) =>
              updateProfile("latencyMilliseconds", latencyMilliseconds)
            }
          />
          <IntegerField
            disabled={disabled}
            label={t("tools.form.latencyJitter")}
            max={300_000}
            min={0}
            value={configuration.custom.latencyJitterMilliseconds}
            onChange={(latencyJitterMilliseconds) =>
              updateProfile("latencyJitterMilliseconds", latencyJitterMilliseconds)
            }
          />
          <IntegerField
            disabled={disabled}
            label={t("tools.form.reliability")}
            max={100}
            min={0}
            value={configuration.custom.reliabilityPercent}
            onChange={(reliabilityPercent) =>
              updateProfile("reliabilityPercent", reliabilityPercent)
            }
          />
          <IntegerField
            disabled={disabled}
            label={t("tools.form.mtu")}
            max={65535}
            min={64}
            value={configuration.custom.mtu}
            onChange={(mtu) => updateProfile("mtu", mtu)}
          />
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

/** 按工具标识分派高级表单；不渲染任何 JSON 文本框，草稿始终保持完整结构化协议对象。 */
export function ToolAdvancedEditor({
  tool,
  configuration,
  disabled,
  presets = [],
  onChange,
}: ToolAdvancedEditorProps) {
  if (tool === "rewrite") {
    return (
      <RewriteEditor
        configuration={configuration as RewriteConfiguration}
        disabled={disabled}
        onChange={onChange}
      />
    );
  }
  if (tool === "breakpoints") {
    return (
      <BreakpointsEditor
        configuration={configuration as BreakpointsConfiguration}
        disabled={disabled}
        onChange={onChange}
      />
    );
  }
  return (
    <ThrottlingEditor
      configuration={configuration as ThrottlingConfiguration}
      disabled={disabled}
      presets={presets}
      onChange={onChange}
    />
  );
}
