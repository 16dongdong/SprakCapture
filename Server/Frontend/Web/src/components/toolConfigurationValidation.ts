import {
  blockCookiesConfigurationSchema,
  blockListConfigurationSchema,
  breakpointsConfigurationSchema,
  autoSaveConfigurationSchema,
  dnsSpoofingConfigurationSchema,
  dnsSpoofingRuleSchema,
  mapLocalConfigurationSchema,
  mapRemoteConfigurationSchema,
  noCachingConfigurationSchema,
  packetFilterConfigurationSchema,
  mirrorConfigurationSchema,
  rewriteConfigurationSchema,
  recordingRuleConfigurationSchema,
  throttlingConfigurationSchema,
  type BlockCookiesConfiguration,
  type BlockListConfiguration,
  type BreakpointsConfiguration,
  type AutoSaveConfiguration,
  type DnsSpoofingConfiguration,
  type HeaderField,
  type LocationPattern,
  type MapLocalConfiguration,
  type MapRemoteConfiguration,
  type NoCachingConfiguration,
  type PacketFilterConfiguration,
  type MirrorConfiguration,
  type RewriteConfiguration,
  type RecordingRuleConfiguration,
  type ThrottlingConfiguration,
} from "../api/protocol";
import { maximumPacketFilterBytes } from "./packetFilterLimits";

/** 可在设置对话框中编辑并提交的工具标识。 */
export type EditableToolId =
  | "recordingRules"
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
  | "autoSave";

/** 所有工具的可编辑配置联合类型，确保前置校验和控制 API 使用同一对象形状。 */
export type EditableToolConfiguration =
  | RecordingRuleConfiguration
  | PacketFilterConfiguration
  | BlockCookiesConfiguration
  | BlockListConfiguration
  | BreakpointsConfiguration
  | AutoSaveConfiguration
  | DnsSpoofingConfiguration
  | MapLocalConfiguration
  | MapRemoteConfiguration
  | NoCachingConfiguration
  | MirrorConfiguration
  | RewriteConfiguration
  | ThrottlingConfiguration;

/** 对应可视化表单字段的首个不可提交问题；索引用于将错误精确指向规则或规则集。 */
export interface ToolConfigurationValidationIssue {
  field:
    | "configuration"
    | "responseBody"
    | "scope"
    | "localPath"
    | "responseHeaders"
    | "mapFrom"
    | "mapTo"
    | "hostPattern"
    | "ipAddress"
    | "setName"
    | "rules"
    | "maxSuspended"
    | "preset"
    | "custom";
  ruleIndex?: number;
  setIndex?: number;
}

/** 节流预设目录来自实时快照；只有目录中存在的 ID 才能通过控制面保存。 */
export interface ToolConfigurationValidationOptions {
  presetIds?: readonly string[];
}

const locationProtocols = new Set([
  "",
  "*",
  "http",
  "https",
  "ws",
  "wss",
  "socks",
  "tcp",
  "udp",
]);
const targetProtocols = new Set(["", "http", "https", "ws", "wss"]);
const headerNamePattern = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
const forbiddenResponseHeaders = new Set([
  "content-length",
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
  "proxy-connection",
]);
const maximumBlockListResponseBodyBytes = 64 * 1024;
const maximumLocalPathBytes = 4_096;
const maximumResponseHeaderCount = 128;
const maximumContentTypeOverrideBytes = 512;
const maximumRemotePathBytes = 2_048;
const utf8Encoder = new TextEncoder();

/**
 * 以 UTF-8 字节数衡量文本配置，和 Rust 的 `String::len` 校验保持一致。
 * 本函数只用于带资源上限的字段，避免多字节文件名或正文绕过前端可见的提交边界。
 */
function utf8ByteLength(value: string): number {
  return utf8Encoder.encode(value).byteLength;
}

/**
 * 校验工具对象能否被前端协议 schema 接受。
 * 表单编辑会改变字符串与数值边界，先阻止超长或越界对象进入控制 API，再执行与 Rust 相同的语义校验。
 */
function hasValidSchema(
  tool: EditableToolId,
  configuration: EditableToolConfiguration,
): boolean {
  if (tool === "recordingRules") {
    return recordingRuleConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "packetFilters") {
    return packetFilterConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "blockList") {
    return blockListConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "noCaching") {
    return noCachingConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "blockCookies") {
    return blockCookiesConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "dnsSpoofing") {
    return dnsSpoofingConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "mapLocal") {
    return mapLocalConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "mapRemote") {
    return mapRemoteConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "rewrite") {
    return rewriteConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "breakpoints") {
    return breakpointsConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "mirror") {
    return mirrorConfigurationSchema.safeParse(configuration).success;
  }
  if (tool === "autoSave") {
    return autoSaveConfigurationSchema.safeParse(configuration).success;
  }
  return throttlingConfigurationSchema.safeParse(configuration).success;
}

/** 验证录制规则标识、名称和必填条件；运行时会执行更严格的 CIDR 与端口语义校验。 */
function validateRecordingRules(
  configuration: RecordingRuleConfiguration,
): ToolConfigurationValidationIssue | null {
  const identifiers = new Set<string>();
  for (const [setIndex, ruleSet] of configuration.ruleSets.entries()) {
    if (ruleSet.name.trim() === "" || identifiers.has(ruleSet.id)) {
      return { field: "setName", setIndex };
    }
    identifiers.add(ruleSet.id);
    for (const [ruleIndex, rule] of ruleSet.rules.entries()) {
      if (
        identifiers.has(rule.id) ||
        (rule.kind !== "match" && rule.value.trim() === "") ||
        (rule.kind === "match" && rule.value !== "")
      ) {
        return { field: "rules", setIndex, ruleIndex };
      }
      identifiers.add(rule.id);
    }
  }
  return null;
}

const packetBytePattern = /^(?:[0-9A-Fa-f]{2}|\?\?)(?:\s+(?:[0-9A-Fa-f]{2}|\?\?))*$/;

/** 校验封包规则的独立搜索与替换序列；空替换和非法字节不得进入运行时。 */
function validatePacketFilters(
  configuration: PacketFilterConfiguration,
): ToolConfigurationValidationIssue | null {
  const identifiers = new Set<string>();
  for (const [ruleIndex, rule] of configuration.rules.entries()) {
    const pattern = rule.pattern.trim();
    const replacement = rule.replacement.trim();
    const patternTokens = pattern === "" ? [] : pattern.split(/\s+/);
    const replacementTokens = replacement === "" ? [] : replacement.split(/\s+/);
    const host = rule.host.trim();
    if (
      rule.id.trim() === "" ||
      rule.name.trim() === "" ||
      utf8ByteLength(rule.id.trim()) > 64 ||
      utf8ByteLength(rule.name.trim()) > 128 ||
      identifiers.has(rule.id) ||
      utf8ByteLength(host) > 253 ||
      (host.includes("*") && (!host.startsWith("*.") || host.slice(2).includes("*"))) ||
      (rule.minimumLength !== null &&
        rule.maximumLength !== null &&
        rule.minimumLength > rule.maximumLength) ||
      (pattern !== "" && !packetBytePattern.test(pattern)) ||
      patternTokens.length > maximumPacketFilterBytes ||
      (rule.action === "modify" &&
        (patternTokens.length === 0 ||
          !packetBytePattern.test(replacement) ||
          replacementTokens.length === 0 ||
          replacementTokens.length > maximumPacketFilterBytes)) ||
      (rule.action !== "modify" && replacement !== "") ||
      (rule.replaceAll && patternTokens.length === 0)
    ) {
      return { field: "rules", ruleIndex };
    }
    identifiers.add(rule.id);
  }
  return null;
}

/** 校验 Location 协议、主机、端口表达式和路径，保持与 Location crate 的持久化规则一致。 */
function hasValidLocation(location: LocationPattern): boolean {
  if (!locationProtocols.has(location.protocol.toLowerCase())) {
    return false;
  }
  if (
    !hasValidLocationHost(location.host) ||
    !hasValidPortExpression(location.port)
  ) {
    return false;
  }
  return (
    location.path === "" ||
    location.path === "*" ||
    location.path.startsWith("/")
  );
}

/** 拒绝 URL、查询语法和不成对 IPv6 方括号，避免将不可匹配的主机规则交给数据面。 */
function hasValidLocationHost(host: string): boolean {
  if (host === "" || host === "*") {
    return true;
  }
  if (/\s|[/#]/.test(host)) {
    return false;
  }
  const queryIndex = host.indexOf("?");
  if (queryIndex >= 0 && /[=&]/.test(host.slice(queryIndex + 1))) {
    return false;
  }
  const hasOpeningBracket = host.startsWith("[");
  const hasClosingBracket = host.endsWith("]");
  if (hasOpeningBracket !== hasClosingBracket || host === "[]") {
    return false;
  }
  const normalizedHost =
    hasOpeningBracket && hasClosingBracket ? host.slice(1, -1) : host;
  if (hasOpeningBracket || normalizedHost.includes(":")) {
    try {
      new URL(`http://[${normalizedHost}]/`);
    } catch {
      return false;
    }
  }
  return true;
}

/** 校验 Location 支持的逗号端口列表和闭区间；端口零值始终无效。 */
function hasValidPortExpression(expression: string): boolean {
  const normalizedExpression = expression.trim();
  if (normalizedExpression === "" || normalizedExpression === "*") {
    return true;
  }
  return normalizedExpression.split(",").every((segment) => {
    const bounds = segment
      .trim()
      .split("-")
      .map((bound) => bound.trim());
    if (
      bounds.length === 0 ||
      bounds.length > 2 ||
      bounds.some((bound) => bound === "")
    ) {
      return false;
    }
    if (bounds.some((bound) => !/^\d+$/.test(bound))) {
      return false;
    }
    const start = Number(bounds[0]);
    const end = bounds.length === 2 ? Number(bounds[1]) : start;
    return (
      Number.isInteger(start) &&
      Number.isInteger(end) &&
      start >= 1 &&
      end >= start &&
      end <= 65_535
    );
  });
}

/** 校验远程映射的单一目标端口；目标不支持 Location 的范围、列表或通配符语义。 */
function hasValidMapRemotePort(port: string): boolean {
  if (port === "") {
    return true;
  }
  if (!/^\d+$/.test(port)) {
    return false;
  }
  const numericPort = Number(port);
  return (
    Number.isInteger(numericPort) && numericPort >= 1 && numericPort <= 65_535
  );
}

/** 校验 HTTP 头名称和值的可线性化范围；禁止 CR/LF 与其他控制字符进入控制面。 */
function hasValidHeader(header: HeaderField): boolean {
  if (!headerNamePattern.test(header.name)) {
    return false;
  }
  return Array.from(header.value).every((character) => {
    const code = character.charCodeAt(0);
    return (
      code === 9 || (code >= 32 && code <= 126) || (code >= 128 && code <= 255)
    );
  });
}

/** 校验本地映射响应头，连接级字段和 Content-Length 由代理生成，不能由规则伪造。 */
function hasValidMapLocalHeaders(
  headers: HeaderField[],
  contentTypeOverride: string,
): boolean {
  if (!hasValidHeader({ name: "content-type", value: contentTypeOverride })) {
    return false;
  }
  return headers.every(
    (header) =>
      hasValidHeader(header) &&
      !forbiddenResponseHeaders.has(header.name.toLowerCase()),
  );
}

/** 校验远程映射的覆盖目标；空字段表示保留原值，不允许目标借用超过来源路径的星号捕获。 */
function hasValidMapRemoteTarget(
  source: LocationPattern,
  target: MapRemoteConfiguration["rules"][number]["to"],
): boolean {
  if (!targetProtocols.has(target.protocol.toLowerCase())) {
    return false;
  }
  if (
    target.host !== "" &&
    (target.host.includes("*") ||
      target.host.includes("?") ||
      !hasValidLocationHost(target.host))
  ) {
    return false;
  }
  if (!hasValidMapRemotePort(target.port)) {
    return false;
  }
  if (
    target.path !== "" &&
    (!target.path.startsWith("/") ||
      target.path.includes("?") ||
      target.path.includes("#"))
  ) {
    return false;
  }
  return countWildcards(target.path) <= countWildcards(source.path);
}

/** 统计路径模板中的星号捕获数量，远程映射只能引用来源模式已提供的捕获。 */
function countWildcards(value: string): number {
  return Array.from(value).filter((character) => character === "*").length;
}

/** 验证 Rust regex 共同支持的基础语法；前端只拦截语法错误，最终编译仍以服务端为准。 */
function hasValidRegularExpression(expression: string | null): boolean {
  if (expression === null) {
    return true;
  }
  try {
    new RegExp(expression);
    return !/\(\?<?[=!]/.test(expression) && !/\\[1-9]/.test(expression);
  } catch {
    return false;
  }
}

/** 查找 Location 数组中的首个无效项，返回其索引以供设置对话框显示精准提示。 */
function invalidLocationIndex(locations: LocationPattern[]): number | null {
  const index = locations.findIndex((location) => !hasValidLocation(location));
  return index < 0 ? null : index;
}

/** 验证 Block List、无缓存和 Cookie 工具的共享 Location 作用域。 */
function validateScopedLocations(
  locations: LocationPattern[],
): ToolConfigurationValidationIssue | null {
  const index = invalidLocationIndex(locations);
  return index === null ? null : { field: "scope", ruleIndex: index };
}

/** 验证本地映射的路径、Location、状态与响应头，避免点击应用后才收到可预防的控制面错误。 */
function validateMapLocal(
  configuration: MapLocalConfiguration,
): ToolConfigurationValidationIssue | null {
  for (const [ruleIndex, rule] of configuration.rules.entries()) {
    if (!hasValidLocation(rule.location)) {
      return { field: "scope", ruleIndex };
    }
    if (rule.localPath.trim() === "") {
      return { field: "localPath", ruleIndex };
    }
    if (utf8ByteLength(rule.localPath) > maximumLocalPathBytes) {
      return { field: "localPath", ruleIndex };
    }
    if (!(100 <= rule.statusCode && rule.statusCode <= 599)) {
      return { field: "configuration", ruleIndex };
    }
    if (
      rule.responseHeaders.length > maximumResponseHeaderCount ||
      utf8ByteLength(rule.contentTypeOverride) >
        maximumContentTypeOverrideBytes ||
      !hasValidMapLocalHeaders(rule.responseHeaders, rule.contentTypeOverride)
    ) {
      return { field: "responseHeaders", ruleIndex };
    }
  }
  return null;
}

/** 验证 DNS 规则的唯一标识、主机模式与 IP 字面量，定位到首个不可提交字段。 */
function validateDnsSpoofing(
  configuration: DnsSpoofingConfiguration,
): ToolConfigurationValidationIssue | null {
  const ruleIds = new Set<string>();
  for (const [ruleIndex, rule] of configuration.rules.entries()) {
    if (rule.id.trim() === "" || ruleIds.has(rule.id)) {
      return { field: "rules", ruleIndex };
    }
    ruleIds.add(rule.id);
    if (rule.hostPattern.trim() === "") {
      return { field: "hostPattern", ruleIndex };
    }
    if (!dnsSpoofingRuleSchema.safeParse(rule).success) {
      return { field: "ipAddress", ruleIndex };
    }
  }
  return null;
}

/** 验证远程映射的来源与目标；来源是 Location，目标是严格的单值覆盖模板。 */
function validateMapRemote(
  configuration: MapRemoteConfiguration,
): ToolConfigurationValidationIssue | null {
  for (const [ruleIndex, rule] of configuration.rules.entries()) {
    if (!hasValidLocation(rule.from)) {
      return { field: "mapFrom", ruleIndex };
    }
    if (utf8ByteLength(rule.to.path) > maximumRemotePathBytes) {
      return { field: "mapTo", ruleIndex };
    }
    if (!hasValidMapRemoteTarget(rule.from, rule.to)) {
      return { field: "mapTo", ruleIndex };
    }
  }
  return null;
}

/** 验证重写集名称、Location、正则与头规则专属字段，确保保存阶段可完成 Rust 侧预编译。 */
function validateRewrite(
  configuration: RewriteConfiguration,
): ToolConfigurationValidationIssue | null {
  // 启用却无任何重写集时，提交只会制造空操作并难排查，前端直接拦截。
  if (configuration.enabled && configuration.sets.length === 0) {
    return { field: "configuration" };
  }
  for (const [setIndex, set] of configuration.sets.entries()) {
    if (set.name.trim() === "") {
      return { field: "setName", setIndex };
    }
    const locationIndex = invalidLocationIndex(set.locations);
    if (locationIndex !== null) {
      return { field: "scope", setIndex, ruleIndex: locationIndex };
    }
    for (const [ruleIndex, rule] of set.rules.entries()) {
      if (
        !hasValidRegularExpression(rule.matchRegex) ||
        !hasValidRegularExpression(rule.matchValueRegex)
      ) {
        return { field: "rules", setIndex, ruleIndex };
      }
      const isHeaderRule =
        rule.type === "requestHeader" || rule.type === "responseHeader";
      if (
        isHeaderRule &&
        (rule.headerName === null ||
          !hasValidHeader({ name: rule.headerName, value: "" }) ||
          rule.headerAction === null ||
          (rule.headerAction === "add" &&
            !hasValidHeader({ name: rule.headerName, value: rule.replace })))
      ) {
        return { field: "rules", setIndex, ruleIndex };
      }
    }
  }
  return null;
}

/** 验证断点的边界数值、阶段与 Location，避免无效规则被保存后永远无法命中。 */
function validateBreakpoints(
  configuration: BreakpointsConfiguration,
): ToolConfigurationValidationIssue | null {
  if (
    !(
      1 <= configuration.suspendTimeoutSeconds &&
      configuration.suspendTimeoutSeconds <= 3_600
    ) ||
    !(1 <= configuration.maxSuspended && configuration.maxSuspended <= 1_024)
  ) {
    return { field: "maxSuspended" };
  }
  for (const [ruleIndex, rule] of configuration.rules.entries()) {
    if (!rule.onRequest && !rule.onResponse) {
      return { field: "rules", ruleIndex };
    }
    if (!hasValidLocation(rule.location)) {
      return { field: "scope", ruleIndex };
    }
  }
  return null;
}

/** 验证节流的自定义边界、作用域与预设 ID；预设选择存在时 custom 不参与实际生效配置。 */
function validateThrottling(
  configuration: ThrottlingConfiguration,
  presetIds: readonly string[] | undefined,
): ToolConfigurationValidationIssue | null {
  const profile = configuration.custom;
  if (
    profile.downloadBytesPerSecond < 1 ||
    profile.uploadBytesPerSecond < 1 ||
    profile.latencyMilliseconds > 300_000 ||
    profile.latencyJitterMilliseconds > 300_000 ||
    profile.reliabilityPercent > 100 ||
    profile.mtu < 64 ||
    profile.mtu > 65_535
  ) {
    return { field: "custom" };
  }
  if (
    configuration.activePresetId !== null &&
    presetIds !== undefined &&
    !presetIds.includes(configuration.activePresetId)
  ) {
    return { field: "preset" };
  }
  return validateScopedLocations(configuration.locations);
}

/**
 * 返回当前配置的首个可预防提交错误。
 * 服务端仍保留完整的原子校验；此前置层只避免普通表单输入落入必然失败的请求。
 */
export function validateToolConfiguration(
  tool: EditableToolId,
  configuration: EditableToolConfiguration,
  options: ToolConfigurationValidationOptions = {},
): ToolConfigurationValidationIssue | null {
  let issue: ToolConfigurationValidationIssue | null;
  if (tool === "recordingRules") {
    issue = validateRecordingRules(configuration as RecordingRuleConfiguration);
  } else if (tool === "packetFilters") {
    issue = validatePacketFilters(configuration as PacketFilterConfiguration);
  } else if (tool === "blockList") {
    const blockListConfiguration = configuration as BlockListConfiguration;
    issue =
      utf8ByteLength(blockListConfiguration.responseBody) >
      maximumBlockListResponseBodyBytes
        ? { field: "responseBody" }
        : validateScopedLocations(blockListConfiguration.locations);
  } else if (tool === "noCaching") {
    issue = validateScopedLocations(
      (configuration as NoCachingConfiguration).locations,
    );
  } else if (tool === "blockCookies") {
    issue = validateScopedLocations(
      (configuration as BlockCookiesConfiguration).locations,
    );
  } else if (tool === "dnsSpoofing") {
    issue = validateDnsSpoofing(configuration as DnsSpoofingConfiguration);
  } else if (tool === "mapLocal") {
    issue = validateMapLocal(configuration as MapLocalConfiguration);
  } else if (tool === "mapRemote") {
    issue = validateMapRemote(configuration as MapRemoteConfiguration);
  } else if (tool === "rewrite") {
    issue = validateRewrite(configuration as RewriteConfiguration);
  } else if (tool === "breakpoints") {
    issue = validateBreakpoints(configuration as BreakpointsConfiguration);
  } else if (tool === "mirror") {
    const mirror = configuration as MirrorConfiguration;
    issue = validateScopedLocations(mirror.locations);
  } else if (tool === "autoSave") {
    const autoSave = configuration as AutoSaveConfiguration;
    issue =
      autoSave.enabled &&
      autoSave.intervalSeconds === 0 &&
      autoSave.everyNTransactions === 0
        ? { field: "configuration" }
        : null;
  } else {
    issue = validateThrottling(
      configuration as ThrottlingConfiguration,
      options.presetIds,
    );
  }
  if (issue !== null) {
    return issue;
  }
  return hasValidSchema(tool, configuration)
    ? null
    : { field: "configuration" };
}
