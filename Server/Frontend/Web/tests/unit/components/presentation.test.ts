import { describe, expect, it } from "vitest";

import {
  presentConnectionState,
  presentProxyEntryPoints,
} from "@/components/presentation";
import {
  presentStreamTransport,
  presentTransactionStatusDetail,
} from "@/components/transactionPresentation";
import i18n from "@/i18n";
import {
  createServiceSnapshot,
  createTransactionSummary,
} from "../../support/testFixtures";

describe("连接状态展示", () => {
  it("只向界面输出中文状态", () => {
    expect(presentConnectionState("connecting")).toBe("连接中");
    expect(presentConnectionState("connected")).toBe("已连接");
    expect(presentConnectionState("disconnected")).toBe("未连接");
  });

  it("原始流展示实际 HTTPS、TCP 和 UDP 类型而不是 SOCKS5 入口", () => {
    expect(
      presentStreamTransport(
        createTransactionSummary({ urlDisplay: "https://example.com:443" }),
      ),
    ).toBe("HTTPS");
    expect(
      presentStreamTransport(
        createTransactionSummary({ urlDisplay: "tcp://example.com:9000" }),
      ),
    ).toBe("TCP");
    expect(
      presentStreamTransport(
        createTransactionSummary({
          method: "DATAGRAM",
          urlDisplay: "udp://example.com:53",
        }),
      ),
    ).toBe("UDP");
  });

  it("服务入口同时展示已运行的 HTTP(S) 与 SOCKS5 监听", () => {
    const baseListeners = createServiceSnapshot().listeners;
    const listeners = {
      ...baseListeners,
      socks5: {
        ...baseListeners.socks5,
        state: "running" as const,
        boundEndpoint: "127.0.0.1:1080",
      },
    };
    expect(presentProxyEntryPoints(listeners, "暂无入口")).toBe(
      "SOCKS5 127.0.0.1:1080",
    );
    expect(
      presentProxyEntryPoints(
        {
          ...listeners,
          httpProxy: {
            ...listeners.httpProxy,
            state: "running",
            boundEndpoint: "127.0.0.1:8888",
          },
        },
        "暂无入口",
      ),
    ).toBe("HTTP(S) 127.0.0.1:8888 · SOCKS5 127.0.0.1:1080");
  });

  it("失败事务的状态紧邻展示本地化失败原因", () => {
    const transaction = createTransactionSummary({
      status: "failed",
      error: {
        code: "UPSTREAM_UNAVAILABLE",
        messageKey: "error.httpProxy.upstreamUnavailable",
        params: {},
      },
    });

    expect(presentTransactionStatusDetail(transaction, i18n.t)).toBe(
      "失败 · 上游服务不可用。",
    );
    expect(
      presentTransactionStatusDetail(
        createTransactionSummary({ status: "pending" }),
        i18n.t,
      ),
    ).toBe("等待中");
  });
});
