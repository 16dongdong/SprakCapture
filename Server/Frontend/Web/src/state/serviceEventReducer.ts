import type { EventMessage, ServiceSnapshot } from "../api/protocol";

/**
 * 将严格事件合并为完整服务快照。
 * 运行上下文：每个窗口独立消费 SSE，局部事件只替换对应权威字段；实例不一致或旧事件返回 null。
 * 事务页拥有独立 revision，因此其顺序不能被先到达的指标事件覆盖。
 */
export function mergeServiceEvent(
  currentSnapshot: ServiceSnapshot | null,
  message: EventMessage,
): ServiceSnapshot | null {
  if (message.type === "snapshot") {
    return message.snapshot;
  }
  if (
    currentSnapshot === null ||
    message.serverInstanceId !== currentSnapshot.serverInstanceId
  ) {
    return null;
  }
  if (message.type === "transactions") {
    if (message.transactions.revision < currentSnapshot.transactions.revision) {
      return null;
    }
    return {
      ...currentSnapshot,
      revision: Math.max(currentSnapshot.revision, message.revision),
      transactions: message.transactions,
    };
  }
  if (message.revision < currentSnapshot.revision) {
    return null;
  }
  if (message.type === "serviceState") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      serviceState: message.serviceState,
      listeners: message.listeners,
    };
  }
  if (message.type === "metrics") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      metrics: message.metrics,
    };
  }
  if (message.type === "processCapture") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      processCapture: message.processCapture,
    };
  }
  if (message.type === "sessions") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      sessions: message.sessions,
    };
  }
  if (message.type === "configuration") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      configuration: message.configuration,
    };
  }
  if (message.type === "ssl") {
    return { ...currentSnapshot, revision: message.revision, ssl: message.ssl };
  }
  if (message.type === "tools") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      tools: message.tools,
    };
  }
  if (message.type === "breakpoints") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      tools: {
        ...currentSnapshot.tools,
        suspendedBreakpointCount: message.suspended.length,
      },
    };
  }
  if (message.type === "advancedRepeats") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      advancedRepeats: message.jobs,
    };
  }
  if (message.type === "plugins") {
    // 宿主在 50ms 窗口内合并连接抖动；整体替换才能同时准确表达安装、卸载和运行态变化。
    return {
      ...currentSnapshot,
      revision: message.revision,
      plugins: message.plugins,
    };
  }
  if (message.type === "mcp") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      mcp: message.mcp,
    };
  }
  if (message.type === "recording") {
    return {
      ...currentSnapshot,
      revision: message.revision,
      recording: message.recording,
    };
  }
  return currentSnapshot;
}
