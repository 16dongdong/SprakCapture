import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  defaultEventsUrl,
  EventClient,
  type EventClientCallbacks,
  type WebSocketFactory,
} from "@/api/eventClient";
import { deriveEventStreamUrl } from "@/api/controlEndpoint";
import {
  createServiceSnapshot,
  createTransactionSummary,
} from "#tests/testFixtures";

interface SocketHarness {
  socket: WebSocket;
  closeSocket: ReturnType<typeof vi.fn>;
  emit(type: string, event: Event): void;
}

/**
 * 创建可主动派发浏览器事件的 WebSocket 替身，用于验证协议错误后的生命周期。
 */
function createSocketHarness(
  initialReadyState: number = WebSocket.OPEN,
): SocketHarness {
  const listeners = new Map<string, EventListener[]>();
  let readyState: number = initialReadyState;
  const closeSocket = vi.fn(() => {
    readyState = WebSocket.CLOSING;
  });
  const socket = {
    addEventListener: vi.fn(
      (type: string, listener: EventListenerOrEventListenerObject) => {
        if (typeof listener !== "function") {
          return;
        }
        const eventListeners = listeners.get(type) ?? [];
        eventListeners.push(listener);
        listeners.set(type, eventListeners);
      },
    ),
    close: closeSocket,
    get readyState() {
      return readyState;
    },
  } as unknown as WebSocket;

  return {
    socket,
    closeSocket,
    /**
     * 按注册顺序派发一次事件，模拟浏览器 WebSocket 回调。
     */
    emit(type: string, event: Event) {
      if (type === "open") {
        readyState = WebSocket.OPEN;
      } else if (type === "close") {
        readyState = WebSocket.CLOSED;
      }
      for (const listener of listeners.get(type) ?? []) {
        listener(event);
      }
    },
  };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("事件协议错误恢复", () => {
  it("默认事件地址由控制基础地址派生", () => {
    expect(defaultEventsUrl).toBe(deriveEventStreamUrl());
  });

  it("接受监听器、录制与事务事件且不触发协议重连", () => {
    const socketHarness = createSocketHarness();
    const onMessage = vi.fn();
    const client = new EventClient(
      "ws://127.0.0.1:17890/api/v1/events",
      () => socketHarness.socket,
    );
    const snapshot = createServiceSnapshot();
    const transaction = createTransactionSummary();

    client.start({
      onConnectionState: vi.fn(),
      onMessage,
    });
    socketHarness.emit(
      "message",
      {
        data: JSON.stringify({
          type: "serviceState",
          serverInstanceId: snapshot.serverInstanceId,
          revision: 2,
          serviceState: "running",
          listeners: snapshot.listeners,
        }),
      } as MessageEvent<string>,
    );
    socketHarness.emit(
      "message",
      {
        data: JSON.stringify({
          type: "recording",
          serverInstanceId: snapshot.serverInstanceId,
          revision: 3,
          recording: {
            ...snapshot.recording,
            state: "paused",
          },
        }),
      } as MessageEvent<string>,
    );
    socketHarness.emit(
      "message",
      {
        data: JSON.stringify({
          type: "transactions",
          serverInstanceId: snapshot.serverInstanceId,
          revision: 4,
          transactions: {
            ...snapshot.transactions,
            revision: 4,
            total: 1,
            items: [transaction],
          },
        }),
      } as MessageEvent<string>,
    );

    expect(onMessage).toHaveBeenCalledTimes(3);
    expect(onMessage.mock.calls[0]?.[0]).toMatchObject({
      type: "serviceState",
      listeners: snapshot.listeners,
    });
    expect(onMessage.mock.calls[1]?.[0]).toMatchObject({
      type: "recording",
      recording: { state: "paused" },
    });
    expect(onMessage.mock.calls[2]?.[0]).toMatchObject({
      type: "transactions",
      transactions: { total: 1 },
    });
    expect(socketHarness.closeSocket).not.toHaveBeenCalled();
    client.stop();
  });

  it("关闭损坏事件所在连接并在延迟后重新连接", async () => {
    const sockets: SocketHarness[] = [];
    const socketFactory = vi.fn<WebSocketFactory>(() => {
      const socketHarness = createSocketHarness();
      sockets.push(socketHarness);
      return socketHarness.socket;
    });
    const connectionStates: string[] = [];
    const callbacks: EventClientCallbacks = {
      onConnectionState(state, message) {
        connectionStates.push(`${state}:${message}`);
      },
      onMessage: vi.fn(),
    };
    const client = new EventClient("ws://127.0.0.1:17890/api/v1/events", socketFactory);

    client.start(callbacks);
    sockets[0]?.emit(
      "message",
      { data: "{" } as MessageEvent<string>,
    );

    expect(sockets[0]?.closeSocket).toHaveBeenCalledWith(
      4002,
      "事件协议错误",
    );
    expect(connectionStates.at(-1)).toBe(
      "disconnected:事件流返回了无效 JSON",
    );

    await vi.advanceTimersByTimeAsync(2000);
    expect(socketFactory).toHaveBeenCalledTimes(2);
    expect(connectionStates.at(-1)).toBe("connecting:正在连接事件流");

    client.stop();
  });

  it("组件在握手期间卸载时等待连接建立后再关闭", () => {
    const socketHarness = createSocketHarness(WebSocket.CONNECTING);
    const client = new EventClient(
      "ws://127.0.0.1:17890/api/v1/events",
      () => socketHarness.socket,
    );

    client.start({
      onConnectionState: vi.fn(),
      onMessage: vi.fn(),
    });
    client.stop();

    expect(socketHarness.closeSocket).not.toHaveBeenCalled();
    socketHarness.emit("open", new Event("open"));
    expect(socketHarness.closeSocket).toHaveBeenCalledOnce();
  });

  it("旧握手迟到完成时不覆盖新连接状态", () => {
    const sockets = [
      createSocketHarness(WebSocket.CONNECTING),
      createSocketHarness(WebSocket.CONNECTING),
    ];
    let socketIndex = 0;
    const socketFactory = vi.fn<WebSocketFactory>(() => {
      const socketHarness = sockets[socketIndex];
      if (socketHarness === undefined) {
        throw new Error("测试 WebSocket 已耗尽");
      }
      socketIndex += 1;
      return socketHarness.socket;
    });
    const connectionStates: string[] = [];
    const client = new EventClient(
      "ws://127.0.0.1:17890/api/v1/events",
      socketFactory,
    );
    const callbacks: EventClientCallbacks = {
      onConnectionState(state) {
        connectionStates.push(state);
      },
      onMessage: vi.fn(),
    };

    client.start(callbacks);
    const oldSocket = sockets[0];
    client.start(callbacks);
    const newSocket = sockets[1];
    oldSocket?.emit("open", new Event("open"));
    expect(connectionStates).toEqual(["connecting", "connecting"]);

    newSocket?.emit("open", new Event("open"));
    expect(connectionStates).toEqual([
      "connecting",
      "connecting",
      "connected",
    ]);
    client.stop();
  });
});
