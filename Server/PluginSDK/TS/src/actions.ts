/** 提供函数调用式动作构造器，避免作者手写 JSON Patch。 */

import type { ExtensionAction, JsonValue } from "./model.js";
import { Event } from "./model.js";

/** 构造标准动作并统一填充空字段；内部使用避免不同构造器产生结构漂移。 */
function createAction(event: Event, action: ExtensionAction["action"], options: Partial<ExtensionAction> = {}): ExtensionAction {
  return { eventId: event.id, action, patch: options.patch ?? [], annotations: options.annotations ?? [], output: options.output ?? null };
}

/** 校验终止原因和目标主机包含可见字符；纯空白输入会在进入 Host 前报告作者错误。 */
function requireNonBlank(value: string, message: string): string {
  if (value.trim().length === 0) throw new Error(message);
  return value;
}

/** 保持事件不变；适用于全部阶段。 */
export function continueEvent(event: Event): ExtensionAction { return createAction(event, "continue"); }

/** 原子替换 payload；Host 会在写入线路前复验阶段结构。 */
export function modifyPayload(event: Event, payload: JsonValue): ExtensionAction {
  return createAction(event, "modify", { patch: [{ op: "replace", path: "", value: payload }] });
}

/** 替换当前线上字节并保留其他 payload 字段。 */
export function modifyBytes(event: Event, bytes: Uint8Array): ExtensionAction {
  return modifyPayload(event, { ...event.payloadObject(), bytes: [...bytes] });
}

/** 暂存当前流块以等待后续半帧。 */
export function hold(event: Event): ExtensionAction { return createAction(event, "hold"); }
/** 丢弃当前块或事务。 */
export function drop(event: Event): ExtensionAction { return createAction(event, "drop"); }
/** 拒绝当前操作并附带稳定原因。 */
export function reject(event: Event, reason = "pluginRejected"): ExtensionAction {
  return createAction(event, "reject", { output: { reason: requireNonBlank(reason, "拒绝原因不能为空") } });
}
/** 请求关闭当前连接。 */
export function close(event: Event, reason = "pluginClosed"): ExtensionAction {
  return createAction(event, "close", { output: { reason: requireNonBlank(reason, "关闭原因不能为空") } });
}
/** 附加解码树、标签或显示字段，不改变线上数据。 */
export function annotate(event: Event, ...annotations: Record<string, JsonValue>[]): ExtensionAction { return createAction(event, "annotate", { annotations }); }

/** 改写最终目标；非法端口在进入 Host 前失败。 */
export function redirect(event: Event, host: string, port: number): ExtensionAction {
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("重定向目标必须包含有效主机和端口");
  return createAction(event, "redirect", { output: { host: requireNonBlank(host, "重定向主机不能为空"), port } });
}

/** 生成 HTTP、DNS 或命令阶段的完整合成响应。 */
export function respond(event: Event, response: JsonValue): ExtensionAction { return createAction(event, "respond", { output: response }); }
