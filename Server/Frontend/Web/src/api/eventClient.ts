import { eventMessageSchema, type EventMessage } from "./protocol";
import {
  defaultControlBaseUrl,
  deriveEventStreamUrl,
} from "./controlEndpoint";
import i18n, { currentRequestLocale } from "../i18n";

export type EventConnectionState = "connecting" | "connected" | "disconnected";

export interface EventClientCallbacks {
  onConnectionState(state: EventConnectionState, message: string): void;
  onMessage(message: EventMessage): void;
}

export interface EventStreamClient {
  start(callbacks: EventClientCallbacks): void;
  stop(): void;
}

export type WebSocketFactory = (url: string) => WebSocket;
export const defaultEventsUrl = deriveEventStreamUrl(defaultControlBaseUrl);
const webSocketConnectingState = 0;
const webSocketOpenState = 1;
const eventProtocolCloseCode = 4002;

export class EventClient implements EventStreamClient {
  private readonly url: string;
  private readonly createWebSocket: WebSocketFactory;
  private socket: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private callbacks: EventClientCallbacks | null = null;
  private closedByOwner = false;

  /**
   * 创建事件流客户端；默认直连本机守护进程，显式地址仅用于部署覆盖和测试注入。
   */
  constructor(
    url = defaultEventsUrl,
    createWebSocket: WebSocketFactory = (targetUrl) =>
      new WebSocket(targetUrl),
  ) {
    this.url = url;
    this.createWebSocket = createWebSocket;
  }

  /**
   * 连接事件流；重复启动会先关闭旧连接，确保只有一个消息来源。
   */
  start(callbacks: EventClientCallbacks): void {
    this.stop();
    this.callbacks = callbacks;
    this.closedByOwner = false;
    this.connect();
  }

  /**
   * 停止事件流和重连计时器；组件卸载后不再发布状态。
   */
  stop(): void {
    this.closedByOwner = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket !== null) {
      const ownedSocket = this.socket;
      this.socket = null;
      this.closeOwnedSocket(ownedSocket);
    }
    this.callbacks = null;
  }

  /**
   * 关闭当前实例拥有的连接；CONNECTING 阶段等待 open 后再关闭，避免浏览器把正常卸载报告成握手警告。
   */
  private closeOwnedSocket(socket: WebSocket): void {
    if (socket.readyState === webSocketConnectingState) {
      socket.addEventListener("open", () => socket.close(), { once: true });
      return;
    }
    if (socket.readyState === webSocketOpenState) {
      socket.close();
    }
  }

  /**
   * 建立一次 WebSocket 连接；协议错误直接显示为未连接并等待下一次重连。
   */
  private connect(): void {
    if (this.closedByOwner || this.callbacks === null) {
      return;
    }

    this.callbacks.onConnectionState(
      "connecting",
      i18n.t("app.control.eventConnecting"),
    );
    let socket: WebSocket;
    try {
      const localizedUrl = new URL(this.url);
      localizedUrl.searchParams.set("locale", currentRequestLocale());
      socket = this.createWebSocket(localizedUrl.toString());
    } catch (error) {
      this.scheduleReconnect(error instanceof Error ? error.message : String(error));
      return;
    }

    this.socket = socket;
    socket.addEventListener("open", () => {
      if (this.socket !== socket) {
        return;
      }
      this.callbacks?.onConnectionState(
        "connected",
        i18n.t("app.control.eventConnected"),
      );
    });
    socket.addEventListener("message", (event) => {
      if (this.socket !== socket) {
        return;
      }
      this.acceptMessage(event.data);
    });
    socket.addEventListener("error", () => {
      if (this.socket !== socket) {
        return;
      }
      this.callbacks?.onConnectionState(
        "disconnected",
        i18n.t("app.control.eventFaulted"),
      );
    });
    socket.addEventListener("close", () => {
      if (this.socket !== socket) {
        return;
      }
      this.socket = null;
      if (!this.closedByOwner) {
        this.scheduleReconnect(i18n.t("app.control.eventDisconnected"));
      }
    });
  }

  /**
   * 解析并校验单条事件；未知事件不会污染共享状态。
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
    this.callbacks?.onMessage(parsedMessage.data);
  }

  /**
   * 拒绝损坏的事件帧并主动重建连接；浏览器脚本只能发送 1000 或 3000—4999 的关闭码，
   * 因此使用应用私有 4002 表达协议错误，避免 close 自身抛错后留下失控连接。
   */
  private rejectMessage(reason: string): void {
    const rejectedSocket = this.socket;
    this.socket = null;
    this.scheduleReconnect(reason);
    rejectedSocket?.close(
      eventProtocolCloseCode,
      i18n.t("error.web.eventProtocolClose"),
    );
  }

  /**
   * 延迟重连并维持明确未连接状态；计时期间不伪造缓存为在线。
   */
  private scheduleReconnect(reason: string): void {
    if (this.closedByOwner || this.callbacks === null || this.reconnectTimer !== null) {
      return;
    }
    this.callbacks.onConnectionState("disconnected", reason);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, 2000);
  }

}
