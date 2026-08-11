import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  ServerSentEventClient,
  type EventSourceFactory,
} from "@/api/serverSentEventClient";
import { deriveServerSentEventsUrl } from "@/api/controlEndpoint";
import { createServiceSnapshot } from "#tests/testFixtures";

interface EventSourceHarness {
  source: EventSource;
  close: ReturnType<typeof vi.fn>;
  emit(type: string, event: Event): void;
}

/**
 * 创建可主动派发 SSE 生命周期与命名事件的替身；测试只验证浏览器传输契约，不模拟网络缓冲。
 */
function createEventSourceHarness(): EventSourceHarness {
  const listeners = new Map<string, EventListener[]>();
  const close = vi.fn();
  const source = {
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
    close,
  } as unknown as EventSource;
  return {
    source,
    close,
    /** 按浏览器注册顺序派发事件，确保旧连接回调隔离可被确定性验证。 */
    emit(type: string, event: Event) {
      for (const listener of listeners.get(type) ?? []) {
        listener(event);
      }
    },
  };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("SSE 控制事件客户端", () => {
  it("连接后逐帧发布权威事件且停止时释放资源", () => {
    const harness = createEventSourceHarness();
    const factory = vi.fn<EventSourceFactory>(() => harness.source);
    const messages: unknown[] = [];
    const connectionStates: string[] = [];
    const client = new ServerSentEventClient(
      deriveServerSentEventsUrl(),
      factory,
    );
    const snapshot = createServiceSnapshot();

    client.start({
      onConnectionState(state) {
        connectionStates.push(state);
      },
      onMessage(message) {
        messages.push(message);
      },
    });
    harness.emit("open", new Event("open"));
    harness.emit(
      "control",
      new MessageEvent("control", {
        data: JSON.stringify({ type: "snapshot", serverInstanceId: snapshot.serverInstanceId, snapshot }),
      }),
    );

    expect(factory).toHaveBeenCalledWith(
      expect.stringContaining("/api/v1/events/sse?locale="),
    );
    expect(connectionStates).toEqual(["connecting", "connected"]);
    expect(messages).toEqual([
      expect.objectContaining({ type: "snapshot", snapshot }),
    ]);

    client.stop();
    expect(harness.close).toHaveBeenCalledOnce();
  });

  it("坏帧关闭当前连接并在短延迟后从权威快照重连", async () => {
    const harnesses = [createEventSourceHarness(), createEventSourceHarness()];
    let index = 0;
    const factory = vi.fn<EventSourceFactory>(() => {
      const harness = harnesses[index];
      if (harness === undefined) {
        throw new Error("测试事件源已耗尽");
      }
      index += 1;
      return harness.source;
    });
    const states: string[] = [];
    const client = new ServerSentEventClient(
      deriveServerSentEventsUrl(),
      factory,
    );

    client.start({
      onConnectionState(state) {
        states.push(state);
      },
      onMessage: vi.fn(),
    });
    harnesses[0]?.emit(
      "control",
      new MessageEvent("control", { data: "{" }),
    );

    expect(harnesses[0]?.close).toHaveBeenCalledOnce();
    expect(states.at(-1)).toBe("disconnected");
    await vi.advanceTimersByTimeAsync(500);
    expect(factory).toHaveBeenCalledTimes(2);
    expect(states.at(-1)).toBe("connecting");
    client.stop();
  });

  it("连续断线指数退避且成功打开后恢复初始延迟", async () => {
    const harnesses = Array.from({ length: 4 }, createEventSourceHarness);
    let index = 0;
    const factory = vi.fn<EventSourceFactory>(() => harnesses[index++]!.source);
    const client = new ServerSentEventClient(
      deriveServerSentEventsUrl(),
      factory,
    );
    client.start({ onConnectionState: vi.fn(), onMessage: vi.fn() });

    harnesses[0]!.emit("error", new Event("error"));
    await vi.advanceTimersByTimeAsync(500);
    expect(factory).toHaveBeenCalledTimes(2);

    harnesses[1]!.emit("error", new Event("error"));
    await vi.advanceTimersByTimeAsync(999);
    expect(factory).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(factory).toHaveBeenCalledTimes(3);

    harnesses[2]!.emit("open", new Event("open"));
    harnesses[2]!.emit("error", new Event("error"));
    await vi.advanceTimersByTimeAsync(500);
    expect(factory).toHaveBeenCalledTimes(4);
    client.stop();
  });
});
