import type { LocationPattern, ValidatorId } from "../api/protocol";
import type { ListenerDialogId } from "../components/listenerSettingsDialog";
import type { ToolDialogId } from "../components/toolSettingsDialog";
import type { TransactionToolSeed } from "../components/transactionToolSeed";
import type { SettingsSection } from "../pages/settingsPage";
import {
  showManagedRouteWindow,
  type ManagedRouteWindow,
  type ManagedWindowPlatform,
} from "./managedWindow";

/** 独立窗口支持的完整业务集合；每一种类型都必须映射到真实可操作页面。 */
export type IndependentWindowRequest =
  | { kind: "settings"; section: SettingsSection }
  | { kind: "processManager" }
  | {
      kind: "ssl";
      seed?: TransactionToolSeed | null;
      focusClientCertificate?: boolean;
    }
  | { kind: "protocol" }
  | { kind: "listener"; listener: ListenerDialogId }
  | { kind: "tool"; tool: ToolDialogId; seed?: TransactionToolSeed | null }
  | { kind: "breakpointHit" }
  | {
      kind: "onlineValidation";
      transactionId: string;
      validatorId: ValidatorId;
    }
  | { kind: "pluginUninstall"; pluginId: string; pluginName: string }
  | { kind: "repeat"; transactionId: string; mode: "edit" | "advanced" }
  | { kind: "clearRecording"; transactionCount: number };

const toolDialogIds: readonly ToolDialogId[] = [
  "recordingRules",
  "packetFilters",
  "blockList",
  "noCaching",
  "blockCookies",
  "dnsSpoofing",
  "mapLocal",
  "mapRemote",
  "rewrite",
  "breakpoints",
  "throttling",
  "mirror",
  "autoSave",
  "export",
];

const settingsSections: readonly SettingsSection[] = [
  "interface",
  "listener",
  "upstreamProxy",
  "capacity",
  "mcp",
];

const seedHashOffset = 2_166_136_261;
const seedHashPrime = 16_777_619;

/** 计算适合 Tauri label 的短稳定哈希；它只区分窗口上下文，不承担数据完整性校验。 */
function createWindowContextHash(value: string): string {
  let hash = seedHashOffset;
  for (let index = 0; index < value.length; index += 1) {
    hash = Math.imul(hash ^ value.charCodeAt(index), seedHashPrime);
  }
  return (hash >>> 0).toString(16);
}

/** 把事务位置写入 URL 参数；正文不会跨窗口复制，避免大对象阻塞窗口创建。 */
function appendTransactionSeed(
  parameters: URLSearchParams,
  seed: TransactionToolSeed | null | undefined,
): void {
  if (seed === null || seed === undefined) {
    return;
  }
  parameters.set("transactionId", seed.transactionId);
  parameters.set("contentType", seed.contentType);
  parameters.set("protocol", seed.location.protocol);
  parameters.set("host", seed.location.host);
  parameters.set("port", seed.location.port);
  parameters.set("path", seed.location.path);
  if (seed.location.query !== null) {
    parameters.set("query", seed.location.query);
  }
}

/** 从 URL 参数恢复最小事务上下文；缺少事务标识时视为无种子，而不是生成半有效规则。 */
export function readTransactionSeed(
  parameters: URLSearchParams,
): TransactionToolSeed | null {
  const transactionId = parameters.get("transactionId");
  if (transactionId === null) {
    return null;
  }
  const location: LocationPattern = {
    protocol: parameters.get("protocol") ?? "",
    host: parameters.get("host") ?? "",
    port: parameters.get("port") ?? "",
    path: parameters.get("path") ?? "",
    query: parameters.has("query") ? parameters.get("query") : null,
  };
  return {
    transactionId,
    contentType: parameters.get("contentType") ?? "",
    location,
  };
}

/** 校验设置区域参数；未知值不会进入导航状态。 */
export function readSettingsSection(
  value: string | undefined,
): SettingsSection {
  return settingsSections.includes(value as SettingsSection)
    ? (value as SettingsSection)
    : "interface";
}

/** 校验工具类型参数；独立窗口缺少有效工具时拒绝渲染错误编辑器。 */
export function readToolDialogId(value: string | null): ToolDialogId | null {
  return toolDialogIds.includes(value as ToolDialogId)
    ? (value as ToolDialogId)
    : null;
}

/** 校验监听器类型参数；该边界阻止未知路由误写其它监听配置。 */
export function readListenerDialogId(
  value: string | null,
): ListenerDialogId | null {
  return value === "reverseProxies" || value === "portForwards" ? value : null;
}

/**
 * 将业务请求转换为唯一的窗口路由、label 与尺寸。
 * 带事务种子的编辑器用上下文哈希隔离草稿；相同请求则复用并聚焦已有窗口。
 */
export function createIndependentWindowTarget(
  request: IndependentWindowRequest,
): ManagedRouteWindow {
  const parameters = new URLSearchParams();
  let routePath: string;
  let labelIdentity: string;
  let title: string;
  let width = 980;
  let height = 760;
  let minWidth = 720;
  let minHeight = 520;

  switch (request.kind) {
    case "settings":
      routePath = `/window/settings/${request.section}`;
      labelIdentity = `settings-${request.section}`;
      title = "设置";
      width = 1120;
      height = 780;
      minWidth = 880;
      minHeight = 620;
      break;
    case "processManager":
      routePath = "/window/dialog/processes";
      labelIdentity = "process-manager";
      title = "进程选择器";
      width = 1180;
      height = 760;
      minWidth = 900;
      minHeight = 620;
      break;
    case "ssl":
      appendTransactionSeed(parameters, request.seed);
      if (request.focusClientCertificate === true) {
        parameters.set("focus", "clientCertificate");
      }
      routePath = "/window/dialog/ssl";
      labelIdentity = `ssl-${createWindowContextHash(parameters.toString())}`;
      title = "SSL 代理设置";
      break;
    case "protocol":
      routePath = "/window/dialog/protocol";
      labelIdentity = "protocol";
      title = "协议工具设置";
      width = 1080;
      break;
    case "listener":
      parameters.set("listener", request.listener);
      routePath = "/window/dialog/listener";
      labelIdentity = `listener-${request.listener}`;
      title = request.listener === "reverseProxies" ? "反向代理" : "端口转发";
      width = 920;
      height = 560;
      minWidth = 720;
      minHeight = 420;
      break;
    case "tool":
      parameters.set("tool", request.tool);
      appendTransactionSeed(parameters, request.seed);
      routePath = "/window/dialog/tool";
      labelIdentity = `tool-${request.tool}-${createWindowContextHash(parameters.toString())}`;
      title = "工具设置";
      break;
    case "breakpointHit":
      routePath = "/window/dialog/breakpoint-hit";
      labelIdentity = "breakpoint-hit";
      title = "断点消息";
      width = 1080;
      height = 760;
      break;
    case "onlineValidation":
      parameters.set("transactionId", request.transactionId);
      parameters.set("validatorId", request.validatorId);
      routePath = "/window/dialog/online-validation";
      labelIdentity = `online-validation-${createWindowContextHash(parameters.toString())}`;
      title = "在线响应校验";
      width = 620;
      height = 380;
      minWidth = 520;
      minHeight = 320;
      break;
    case "pluginUninstall":
      parameters.set("pluginId", request.pluginId);
      parameters.set("pluginName", request.pluginName);
      routePath = "/window/dialog/plugin-uninstall";
      labelIdentity = `plugin-uninstall-${createWindowContextHash(request.pluginId)}`;
      title = "卸载插件";
      width = 560;
      height = 280;
      minWidth = 480;
      minHeight = 260;
      break;
    case "repeat":
      parameters.set("transactionId", request.transactionId);
      parameters.set("mode", request.mode);
      routePath = "/window/dialog/repeat";
      labelIdentity = `repeat-${request.mode}-${createWindowContextHash(request.transactionId)}`;
      title = request.mode === "edit" ? "编辑并重复" : "高级重复";
      width = 1080;
      height = 780;
      break;
    case "clearRecording":
      parameters.set("transactionCount", String(request.transactionCount));
      routePath = "/window/dialog/clear-recording";
      labelIdentity = "clear-recording";
      title = "清空事务";
      width = 560;
      height = 300;
      minWidth = 480;
      minHeight = 280;
      break;
  }

  const query = parameters.toString();
  return {
    label: `app-window-${labelIdentity}`,
    path: query === "" ? routePath : `${routePath}?${query}`,
    title,
    width,
    height,
    minWidth,
    minHeight,
  };
}

/** 打开业务独立窗口；所有入口共用同一 contract，调用方只声明业务上下文。 */
export function showIndependentWindow(
  request: IndependentWindowRequest,
  platform?: ManagedWindowPlatform,
): Promise<void> {
  return showManagedRouteWindow(
    createIndependentWindowTarget(request),
    platform,
  );
}
