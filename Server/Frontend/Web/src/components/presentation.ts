import type {
  ListenerSnapshots,
  ServiceMetrics,
  ServiceSnapshot,
  ServiceState,
} from "../api/protocol";
import i18n from "../i18n";

export interface ServicePresentation {
  label: string;
  actionText: string;
  actionKind: "start" | "stop" | "wait";
  tone: "neutral" | "success" | "warning" | "danger";
}

export type UnifiedTrafficMetrics = ServiceMetrics;

/**
 * 合并代理监听器与 WinDivert 进程捕获的实时指标；两个数据面由后端分别计数，所有概览界面必须使用同一聚合口径。
 * 运行上下文：工作台概览和悬浮窗收到同一份服务快照后同步调用，避免只展示代理账本而把进程捕获流量显示为零。
 * 参数：snapshot 为当前控制服务的权威快照；调用方仅在快照存在时传入。
 * 失败语义：该函数执行纯数值聚合且不会失败；协议层已保证所有计数均为非负安全整数。
 */
export function combineTrafficMetrics(
  snapshot: ServiceSnapshot,
): UnifiedTrafficMetrics {
  return {
    ...snapshot.metrics,
    acceptedConnections:
      snapshot.metrics.acceptedConnections +
      snapshot.processCapture.acceptedConnections,
    activeConnections:
      snapshot.metrics.activeConnections + snapshot.processCapture.trackedFlows,
    bytesUp: snapshot.metrics.bytesUp + snapshot.processCapture.bytesUp,
    bytesDown: snapshot.metrics.bytesDown + snapshot.processCapture.bytesDown,
  };
}

type ServicePresentationMetadata = Pick<
  ServicePresentation,
  "actionKind" | "tone"
>;

const servicePresentationMetadata: Record<
  ServiceState,
  ServicePresentationMetadata
> = {
  stopped: {
    actionKind: "start",
    tone: "neutral",
  },
  starting: {
    actionKind: "wait",
    tone: "warning",
  },
  running: {
    actionKind: "stop",
    tone: "success",
  },
  stopping: {
    actionKind: "wait",
    tone: "warning",
  },
  faulted: {
    actionKind: "start",
    tone: "danger",
  },
};

/**
 * 返回服务状态的本地化展示；动作类型和视觉语气来自稳定状态表，文案读取当前界面语言。
 */
export function presentServiceState(
  serviceState: ServiceState,
): ServicePresentation {
  return {
    ...servicePresentationMetadata[serviceState],
    label: i18n.t(`app.service.${serviceState}.label`),
    actionText: i18n.t(`app.service.${serviceState}.action`),
  };
}

/**
 * 把字节计数格式化为稳定二进制单位；非负约束由严格控制协议保证。
 */
export function formatByteCount(byteCount: number): string {
  if (byteCount < 1024) {
    return `${byteCount} B`;
  }
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = byteCount / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value >= 100 ? value.toFixed(0) : value.toFixed(2)} ${units[unitIndex]}`;
}

/**
 * 把控制面和事件流连接状态转换为当前界面语言，避免诊断区域泄漏协议枚举值。
 */
export function presentConnectionState(
  state: "connecting" | "connected" | "disconnected",
): string {
  return i18n.t(`app.connectionState.${state}`);
}

/**
 * 汇总进入统一捕获管线的代理监听入口；HTTP(S) 是分析主入口，SOCKS5 仅作为可选接入协议展示。
 *
 * 运行上下文：概览页、悬浮窗和其他紧凑状态面共享同一份权威监听快照。
 * 参数：listeners 为控制面返回的监听器状态，unavailableText 为没有可用入口时的本地化文案。
 * 失败语义：只展示已绑定的运行中入口；快照尚未绑定任何地址时返回 unavailableText，不推断配置地址。
 */
export function presentProxyEntryPoints(
  listeners: ListenerSnapshots,
  unavailableText: string,
): string {
  const entryPoints = [
    ["HTTP(S)", listeners.httpProxy],
    ["SOCKS5", listeners.socks5],
  ] as const;
  const activeEntryPoints = entryPoints.flatMap(([protocol, listener]) =>
    listener.state === "running" && listener.boundEndpoint !== null
      ? [`${protocol} ${listener.boundEndpoint}`]
      : [],
  );
  return activeEntryPoints.join(" · ") || unavailableText;
}
