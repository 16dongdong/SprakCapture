/** 定义 TypeScript 插件与 Host API 2.x 交换的稳定类型。 */

export const stages = [
  "serviceStarting", "serviceStarted", "configurationChanged", "serviceStopping",
  "connectionAccepted", "socks5Authentication", "protocolClassified", "targetResolving", "beforeConnect", "connected", "connectionClosing",
  "clientHelloObserved", "certificateSelecting", "tlsEstablished", "tlsFailed",
  "requestHeaders", "requestBodyChunk", "requestComplete", "beforeUpstream", "responseHeaders", "responseBodyChunk", "responseComplete",
  "webSocketOpening", "webSocketFrame", "webSocketClosing", "tcpChunk", "udpDatagram", "dnsMessage",
  "beforeRecord", "transactionUpdated", "transactionCompleted", "recordingCleared", "inspectorDataRequested", "commandInvoked", "contextActionInvoked",
] as const;

export type Stage = typeof stages[number];
export type ActionKind = "continue" | "modify" | "hold" | "drop" | "reject" | "respond" | "redirect" | "annotate" | "close";
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };

export interface StageContext {
  readonly [name: string]: JsonValue | undefined;
  readonly direction?: string;
  readonly interceptionMode: "intercept" | "observeOnly";
}

export interface EventEnvelope {
  readonly apiVersion: string;
  readonly eventId: string;
  readonly stage: Stage;
  readonly serviceGeneration: number;
  readonly recordingGeneration: number;
  readonly pluginInstanceId: string;
  readonly connectionId: string | null;
  readonly transactionId: string | null;
  readonly deadlineUnixMs: number;
  readonly context: StageContext;
  readonly payload: JsonValue;
}

export interface RuntimeInvocation {
  readonly pluginId: string;
  readonly moduleId: string;
  readonly moduleKind: string;
  readonly envelope: EventEnvelope;
}

export interface ExtensionAction {
  readonly eventId: string;
  readonly action: ActionKind;
  readonly patch: readonly Record<string, JsonValue>[];
  readonly annotations: readonly Record<string, JsonValue>[];
  readonly output: JsonValue;
}

export class Event {
  /** 校验并封装一次宿主调用；字段不完整时在进入作者函数前失败。 */
  public constructor(public readonly invocation: RuntimeInvocation) {
    const envelope = invocation.envelope;
    if (!envelope?.eventId || !stages.includes(envelope.stage)) {
      throw new Error("无效的插件调用事件");
    }
  }

  public get id(): string {
    /** 返回宿主事件 ID；动作必须原样回显该值。 */
    return this.invocation.envelope.eventId;
  }

  public get stage(): Stage {
    /** 返回稳定阶段线名；编译器会限制作者使用不存在的阶段。 */
    return this.invocation.envelope.stage;
  }

  public get connectionId(): string {
    /** 返回连接 ID；连接外事件使用空串以便无状态处理器直接调用。 */
    return this.invocation.envelope.connectionId ?? "";
  }

  public get context(): StageContext {
    /** 返回只读阶段上下文；SDK 不修改宿主原始身份字段。 */
    return this.invocation.envelope.context;
  }

  public get payload(): JsonValue {
    /** 返回当前 payload 视图；动作构造器会在改写时创建独立副本。 */
    return this.invocation.envelope.payload;
  }

  public payloadObject(): Readonly<Record<string, JsonValue>> {
    /** 返回对象 payload；字节与结构化字段便捷访问在标量/数组阶段明确失败。 */
    if (this.payload === null || Array.isArray(this.payload) || typeof this.payload !== "object") {
      throw new Error("事件 payload 必须是对象");
    }
    return this.payload;
  }

  public bytes(): Uint8Array {
    /** 将 bytes 数组转换为 Uint8Array；越界或非整数输入直接报告协议错误。 */
    const values = this.payloadObject().bytes;
    if (!Array.isArray(values) || values.some((value) => !Number.isInteger(value) || (value as number) < 0 || (value as number) > 255)) {
      throw new Error("payload.bytes 必须是 0..255 的整数数组");
    }
    return Uint8Array.from(values as number[]);
  }
}
