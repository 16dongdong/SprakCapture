import { describe, expect, it } from "vitest";

import {
  defaultControlBaseUrl,
  deriveEventStreamUrl,
  deriveServerSentEventsUrl,
  isTauriRuntimeOrigin,
  resolveControlBaseUrl,
} from "@/api/controlEndpoint";

describe("控制服务端点派生", () => {
  it("不把 Tauri 静态资源来源误判为远程 Web 控制端", () => {
    expect(isTauriRuntimeOrigin({ hostname: "tauri.localhost" } as Location)).toBe(
      true,
    );
    expect(
      isTauriRuntimeOrigin({ hostname: "window-123.tauri.localhost" } as Location),
    ).toBe(true);
    expect(isTauriRuntimeOrigin({ hostname: "capture.example.com" } as Location)).toBe(
      false,
    );
  });

  it("REST 与事件流默认使用同一个本机控制服务", () => {
    expect(resolveControlBaseUrl()).toBe(defaultControlBaseUrl);
    expect(deriveEventStreamUrl()).toBe(
      "ws://127.0.0.1:17890/api/v1/events",
    );
    expect(deriveServerSentEventsUrl()).toBe(
      "http://127.0.0.1:17890/api/v1/events/sse",
    );
  });

  it("从显式 HTTP 控制地址保留主机、端口和路径前缀生成事件流", () => {
    const controlBaseUrl = "http://127.0.0.1:17991/control";

    expect(resolveControlBaseUrl(controlBaseUrl)).toBe(controlBaseUrl);
    expect(deriveEventStreamUrl(controlBaseUrl)).toBe(
      "ws://127.0.0.1:17991/control/api/v1/events",
    );
    expect(deriveServerSentEventsUrl(controlBaseUrl)).toBe(
      "http://127.0.0.1:17991/control/api/v1/events/sse",
    );
  });

  it("HTTPS 控制地址生成 WSS 事件流", () => {
    expect(deriveEventStreamUrl("https://localhost:18443")).toBe(
      "wss://localhost:18443/api/v1/events",
    );
    expect(deriveServerSentEventsUrl("https://localhost:18443")).toBe(
      "https://localhost:18443/api/v1/events/sse",
    );
  });

  it.each([
    "",
    " http://127.0.0.1:17991",
    "http://127.0.0.1:17991 ",
    "ws://127.0.0.1:17991",
    "http://user:secret@127.0.0.1:17991",
    "http://127.0.0.1:17991?locale=zh-Hans",
    "http://127.0.0.1:17991#events",
  ])("拒绝不确定的控制基础地址：%s", (controlBaseUrl) => {
    expect(() => resolveControlBaseUrl(controlBaseUrl)).toThrow(TypeError);
  });
});
