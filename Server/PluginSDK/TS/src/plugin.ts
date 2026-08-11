/** 提供普通函数式阶段、TCP 与 UDP 注册。 */

import { continueEvent, drop, hold, modifyBytes } from "./actions.js";
import { Event, type ExtensionAction, type RuntimeInvocation, type Stage } from "./model.js";
import { type Frame, StreamPipeline } from "./stream.js";

export type EventHandler = (event: Event) => ExtensionAction | void | Promise<ExtensionAction | void>;
export type StopHandler = () => void | Promise<void>;

export class Plugin {
  private readonly handlers = new Map<Stage, EventHandler>();
  private streamPipeline?: StreamPipeline;
  private frameHandler?: (frame: Frame, event: Event) => Frame | Uint8Array | null;
  private datagramHandler?: (bytes: Uint8Array, event: Event) => Uint8Array | null;
  private stopHandler?: StopHandler;
  private stopped = false;
  public manifest: Record<string, unknown> = {};
  public configuration: Record<string, unknown> = {};

  /** 注册阶段函数；重复注册同一阶段失败，避免调用顺序隐藏。 */
  public on(stage: Stage, handler?: EventHandler): EventHandler | ((registered: EventHandler) => EventHandler) {
    const register = (registered: EventHandler): EventHandler => {
      if (this.handlers.has(stage)) throw new Error(`阶段已注册：${stage}`);
      this.handlers.set(stage, registered);
      return registered;
    };
    return handler ? register(handler) : register;
  }

  /** 注册 TCP 明文帧函数；SDK 管理半包、密码器与重封包。 */
  public tcp(pipeline: StreamPipeline, handler: (frame: Frame, event: Event) => Frame | Uint8Array | null): void {
    this.streamPipeline = pipeline;
    this.frameHandler = handler;
  }

  /** 注册 UDP 数据报函数；null 表示丢弃，Uint8Array 表示转发内容。 */
  public udp(handler: (bytes: Uint8Array, event: Event) => Uint8Array | null): void { this.datagramHandler = handler; }

  /** 注册一次停止生命周期；重复注册会失败，避免资源释放顺序含糊。 */
  public onStop(handler: StopHandler): StopHandler {
    if (this.stopHandler) throw new Error("停止函数已注册");
    this.stopHandler = handler;
    return handler;
  }

  /** 在全部作者任务结束后执行一次清理；重复调用保持幂等。 */
  public async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;
    await this.stopHandler?.();
  }

  /** 执行一次 RuntimeInvocation；作者异常原样交给 Sidecar error 消息。 */
  public async invoke(invocation: RuntimeInvocation): Promise<ExtensionAction> {
    const event = new Event(invocation);
    if (event.stage === "tcpChunk" && this.streamPipeline && this.frameHandler) {
      const output = this.streamPipeline.push(event.connectionId, String(event.context.direction ?? "unknown"), event.bytes(), (frame) => this.frameHandler?.(frame, event) ?? null);
      if (output === undefined) return hold(event);
      return output.length > 0 ? modifyBytes(event, output) : drop(event);
    }
    if (event.stage === "udpDatagram" && this.datagramHandler) {
      const output = this.datagramHandler(event.bytes(), event);
      return output ? modifyBytes(event, output) : drop(event);
    }
    return (await this.handlers.get(event.stage)?.(event)) ?? continueEvent(event);
  }
}

/** 创建插件注册对象；作者无需继承基类或实现 JSON 分派。 */
export function definePlugin(): Plugin { return new Plugin(); }
