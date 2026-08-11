import i18n, { currentRequestLocale } from "../i18n";
import { eventMessageSchema, type EventMessage } from "./protocol";
import { deriveServerSentEventsUrl } from "./controlEndpoint";
import type {
  EventClientCallbacks,
  EventStreamClient,
} from "./eventClient";

export type EventSourceFactory = (url: string) => EventSource;

const reconnectInitialDelayMilliseconds = 500;
const reconnectMaximumDelayMilliseconds = 10_000;
const reconnectMaximumAttempt = Math.ceil(
  Math.log2(reconnectMaximumDelayMilliseconds / reconnectInitialDelayMilliseconds),
);

/**
 * 使用浏览器原生 SSE 维护控制面单向事件流；所有易变视图共享一条连接，避免页面各自轮询。
 *
 * 运行上下文：ServiceProvider 生命周期内创建一次。服务端每次连接先发送权威快照，再发送带 revision
 * 的增量，因此重连不依赖客户端猜测丢失区间。协议坏帧会关闭当前源并重新建连，防止错误数据污染状态。
 */
export class ServerSentEventClient implements EventStreamClient {
  private readonly url: string;
  private readonly createEventSource: EventSourceFactory;
  private source: EventSource | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private callbacks: EventClientCallbacks | null = null;
  private closedByOwner = false;
  private reconnectAttempt = 0;

  /**
   * 创建 SSE 客户端；地址和工厂可注入用于桌面宿主与确定性测试。
   */
  constructor(
    url = deriveServerSentEventsUrl(),
    createEventSource: EventSourceFactory = (targetUrl) =>
      new EventSource(targetUrl),
  ) {
    this.url = url;
    this.createEventSource = createEventSource;
  }

  /**
   * 启动唯一事件源；重复启动先释放旧连接，确保不会重复应用同一实时帧。
   */
  start(callbacks: EventClientCallbacks): void {
    this.stop();
    this.callbacks = callbacks;
    this.closedByOwner = false;
    this.reconnectAttempt = 0;
    this.connect();
  }

  /**
   * 停止事件源和重连计时器；组件卸载后不再触发任何连接状态或消息回调。
   */
  stop(): void {
    this.closedByOwner = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.source?.close();
    this.source = null;
    this.callbacks = null;
    this.reconnectAttempt = 0;
  }

  /**
   * 建立一次带界面语言的 SSE 连接；构造失败会进入同一有界重连路径。
   */
  private connect(): void {
    if (this.closedByOwner || this.callbacks === null) {
      return;
    }
    this.callbacks.onConnectionState(
      "connecting",
      i18n.t("app.control.eventConnecting"),
    );

    let source: EventSource;
    try {
      const localizedUrl = new URL(this.url);
      localizedUrl.searchParams.set("locale", currentRequestLocale());
      source = this.createEventSource(localizedUrl.toString());
    } catch (error) {
      this.scheduleReconnect(
        error instanceof Error ? error.message : String(error),
      );
      return;
    }

    this.source = source;
    source.addEventListener("open", () => {
      if (this.source !== source) {
        return;
      }
      // 只有真实 open 才证明链路恢复；此处清零退避可让下一次偶发断线仍以低延迟重连。
      this.reconnectAttempt = 0;
      this.callbacks?.onConnectionState(
        "connected",
        i18n.t("app.control.eventConnected"),
      );
    });
    source.addEventListener("control", (event) => {
      if (this.source !== source) {
        return;
      }
      this.acceptMessage((event as MessageEvent<string>).data);
    });
    source.addEventListener("error", () => {
      if (this.source !== source) {
        return;
      }
      source.close();
      this.source = null;
      this.scheduleReconnect(i18n.t("app.control.eventDisconnected"));
    });
  }

  /**
   * 解析并校验单条 SSE 数据；未知或损坏事件会重建快照流，而不是继续使用部分状态。
   */
  private acceptMessage(rawMessage: unknown): void {
    if (typeof rawMessage !== "string") {
      this.rejectMessage(i18n.t("error.web.eventNonText"));
      return;
    }

    let payload: unknown;
    try {
      payload = JSON.parse(rawMessage);
    } catch {
      this.rejectMessage(i18n.t("error.web.eventInvalidJson"));
      return;
    }
    const parsedMessage = eventMessageSchema.safeParse(payload);
    if (!parsedMessage.success) {
      this.rejectMessage(i18n.t("error.web.eventInvalidProtocol"));
      return;
    }
    this.callbacks?.onMessage(parsedMessage.data as EventMessage);
  }

  /**
   * 拒绝协议坏帧并关闭其所属事件源；下一连接首帧为完整快照，可恢复权威状态。
   */
  private rejectMessage(reason: string): void {
    this.source?.close();
    this.source = null;
    this.scheduleReconnect(reason);
  }

  /**
   * 以有上限指数退避合并并发错误；同一时刻只允许一个重连任务，避免后台不可用时形成连接风暴。
   * 延迟采用确定性序列便于桌面环境和测试复现，成功 open 后从初始延迟重新开始。
   */
  private scheduleReconnect(reason: string): void {
    if (
      this.closedByOwner ||
      this.callbacks === null ||
      this.reconnectTimer !== null
    ) {
      return;
    }
    this.callbacks.onConnectionState("disconnected", reason);
    const reconnectDelayMilliseconds = Math.min(
      reconnectInitialDelayMilliseconds * 2 ** this.reconnectAttempt,
      reconnectMaximumDelayMilliseconds,
    );
    this.reconnectAttempt = Math.min(
      this.reconnectAttempt + 1,
      reconnectMaximumAttempt,
    );
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, reconnectDelayMilliseconds);
  }
}
