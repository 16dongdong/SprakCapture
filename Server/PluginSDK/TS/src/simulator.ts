/** 实现确定性本地 Host 模拟器和最小完整夹具构造器。 */

import type { ExtensionAction, JsonValue, RuntimeInvocation, Stage } from "./model.js";
import { Plugin } from "./plugin.js";

export class Simulator {
  /** 绑定待测插件；实例状态在多个调用之间保留，以覆盖真实连接。 */
  public constructor(private readonly plugin: Plugin) {}
  /** 调用普通插件函数并返回标准动作；错误直接传播给测试框架。 */
  public invoke(invocation: RuntimeInvocation): Promise<ExtensionAction> { return this.plugin.invoke(invocation); }
}

export interface InvocationOptions { readonly connectionId?: string | null; readonly direction?: string; }

/** 构造 SDK 测试使用的最小 RuntimeInvocation；不代表生产 Host 的动态代际。 */
export function createInvocation(eventId: string, stage: Stage, payload: JsonValue, options: InvocationOptions = {}): RuntimeInvocation {
  const connectionId = options.connectionId === undefined ? "connection-1" : options.connectionId;
  const direction = options.direction ?? "up";
  return {
    pluginId: "example.binary", moduleId: "transformer", moduleKind: "streamTransformer",
    envelope: {
      apiVersion: "2.0.0", eventId, stage, serviceGeneration: 1, recordingGeneration: 1,
      pluginInstanceId: "example.binary@1.0.0#1", connectionId, transactionId: null,
      deadlineUnixMs: Number.MAX_SAFE_INTEGER,
      context: { direction, interceptionMode: "intercept" }, payload,
    },
  };
}
