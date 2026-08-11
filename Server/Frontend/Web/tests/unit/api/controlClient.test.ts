import { describe, expect, it, vi } from "vitest";

import {
  defaultControlBaseUrl,
  HttpControlClient,
} from "@/api/controlClient";
import {
  defaultEventsUrl,
  EventClient,
  type EventClientCallbacks,
  type WebSocketFactory,
} from "@/api/eventClient";
import i18n from "@/i18n";
import {
  createServiceSnapshot,
  createTransactionSummary,
} from "#tests/testFixtures";

/**
 * 创建携带严格快照的 JSON 响应，避免各用例复制响应头和序列化逻辑。
 */
function createSnapshotResponse(): Response {
  return new Response(JSON.stringify(createServiceSnapshot()), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

/**
 * 创建严格录制响应；用例只覆盖传输契约，不复制录制快照的全部默认字段。
 */
function createRecordingResponse(state: "recording" | "paused" = "recording"): Response {
  const snapshot = createServiceSnapshot();
  return new Response(
    JSON.stringify({
      serverInstanceId: snapshot.serverInstanceId,
      revision: snapshot.revision,
      recording: {
        ...snapshot.recording,
        state,
      },
    }),
    {
      status: 200,
      headers: { "Content-Type": "application/json" },
    },
  );
}

/**
 * 创建公开 SSL 状态响应；固定夹具刻意不包含任何私钥相关字段。
 */
function createSslResponse(): Response {
  return new Response(JSON.stringify(createServiceSnapshot().ssl), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

/**
 * 创建事务详情响应；正文元信息保持为空，正文端点行为由独立用例覆盖。
 */
function createTransactionDetailResponse(transactionId: string): Response {
  return new Response(
    JSON.stringify({
      revision: 3,
      transaction: createTransactionSummary({ transactionId }),
      requestHeaders: [{ name: "content-type", value: "text/plain" }],
      responseHeaders: [],
      requestBody: null,
      responseBody: null,
    }),
    {
      status: 200,
      headers: { "Content-Type": "application/json" },
    },
  );
}

/**
 * 创建指定方向的正文响应；标准 base64 保持原字符串，测试客户端不会提前解码或复制。
 */
function createBodyResponse(
  transactionId: string,
  side: "request" | "response",
  base64 = "AAEC",
  decoded: {
    algorithm: string;
    contentType: string;
    decodedBytes: number;
    base64: string;
  } | null = null,
): Response {
  return new Response(
    JSON.stringify({
      revision: 4,
      meta: {
        transactionId,
        side,
        contentType: "application/octet-stream",
        encoding: "identity",
        storedBytes: 3,
        originalBytes: 3,
        truncated: false,
      },
      base64,
      decoded,
    }),
    {
      status: 200,
      headers: { "Content-Type": "application/json" },
    },
  );
}

/**
 * 创建不主动发事件的 WebSocket 替身；地址断言不依赖真实网络。
 */
function createSocketStub(): WebSocket {
  return {
    addEventListener: vi.fn(),
    close: vi.fn(),
  } as unknown as WebSocket;
}

const inactiveCallbacks: EventClientCallbacks = {
  onConnectionState: vi.fn(),
  onMessage: vi.fn(),
};

describe("默认控制地址", () => {
  it("默认 fetch 保留浏览器全局调用上下文", async () => {
    const originalFetch = globalThis.fetch;
    const requestFetch = vi.fn(function (this: typeof globalThis) {
      expect(this).toBe(globalThis);
      return Promise.resolve(createSnapshotResponse());
    });
    globalThis.fetch = requestFetch as typeof fetch;

    try {
      await new HttpControlClient().getSnapshot();
    } finally {
      globalThis.fetch = originalFetch;
    }

    expect(requestFetch).toHaveBeenCalledOnce();
  });

  it("HTTP 客户端默认访问本机守护进程", async () => {
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValue(createSnapshotResponse());
    const client = new HttpControlClient(undefined, requestFetch);

    await client.getSnapshot();

    expect(requestFetch).toHaveBeenCalledWith(
      `${defaultControlBaseUrl}/api/v1/snapshot`,
      expect.objectContaining({ method: "GET" }),
    );
  });

  it("事件客户端默认访问本机守护进程", () => {
    const socketFactory = vi.fn<WebSocketFactory>(() => createSocketStub());
    const client = new EventClient(undefined, socketFactory);

    client.start(inactiveCallbacks);

    expect(socketFactory).toHaveBeenCalledWith(
      `${defaultEventsUrl}?locale=zh-Hans`,
    );
    client.stop();
  });

  it("语言切换后 HTTP 头与新建 WebSocket 使用同一具体 locale", async () => {
    await i18n.changeLanguage("ja");
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValue(createSnapshotResponse());
    await new HttpControlClient(undefined, requestFetch).getSnapshot();
    expect(requestFetch).toHaveBeenCalledWith(
      `${defaultControlBaseUrl}/api/v1/snapshot`,
      expect.objectContaining({
        headers: expect.objectContaining({ "Accept-Language": "ja" }),
      }),
    );

    const socketFactory = vi.fn<WebSocketFactory>(() => createSocketStub());
    const eventClient = new EventClient(undefined, socketFactory);
    eventClient.start(inactiveCallbacks);
    expect(socketFactory).toHaveBeenCalledWith(`${defaultEventsUrl}?locale=ja`);
    eventClient.stop();
  });
});

describe("HTTP 控制失败语义", () => {
  it("提取受控 JSON 中的中文错误信息", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          code: "serviceNotStartable",
          message: "服务正在停止",
          messageKey: "error.serviceNotStartable",
          params: {},
        }),
        { status: 409 },
      ),
    );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await expect(client.startService()).rejects.toMatchObject({
      statusCode: 409,
      message: "服务正在停止",
    });
  });

  it("拒绝把非协议错误正文直接展示到界面", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockResolvedValue(
      new Response("<html>upstream failure</html>", { status: 502 }),
    );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await expect(client.getSnapshot()).rejects.toMatchObject({
      statusCode: 502,
      message: "控制请求失败（HTTP 502）",
    });
  });

  it("拒绝只包含旧错误文案字段的响应", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(JSON.stringify({ errorMessage: "旧错误结构" }), {
        status: 409,
      }),
    );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await expect(client.startService()).rejects.toMatchObject({
      statusCode: 409,
      message: "控制请求失败（HTTP 409）",
    });
  });

  it("拒绝不符合协议的成功响应", async () => {
    const { serverInstanceId } = createServiceSnapshot();
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValue(
        new Response(
          JSON.stringify({ serverInstanceId, serviceState: "running" }),
          {
            status: 200,
          },
        ),
      );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await expect(client.getSnapshot()).rejects.toMatchObject({
      statusCode: 200,
      message: "控制服务响应字段无效：revision",
    });
  });

  it("把网络失败保留为无 HTTP 状态码的未连接错误", async () => {
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockRejectedValue(new TypeError("连接被拒绝"));
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await expect(client.getSnapshot()).rejects.toMatchObject({
      statusCode: null,
      message: "控制服务未连接：网络请求失败",
    });
  });
});

describe("M2 SSL 控制客户端", () => {
  it("使用严格路由更新设置、再生并导出公开根证书", async () => {
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(createSslResponse())
      .mockResolvedValueOnce(createSslResponse())
      .mockResolvedValueOnce(createSslResponse())
      .mockResolvedValueOnce(
        new Response("ROOT CERTIFICATE", {
          status: 200,
          headers: { "Content-Type": "application/x-pem-file" },
        }),
      );
    const client = new HttpControlClient(
      "http://127.0.0.1:17890",
      requestFetch,
    );
    const update = {
      enabled: true,
      includeLocations: [
        {
          protocol: "https",
          host: "*.example.com",
          port: "",
          path: "",
          query: null,
        },
      ],
      excludeLocations: [],
      maxCachedCertificates: 128,
      useClientSni: true,
    };

    await client.getSsl();
    await client.updateSsl(update);
    await client.regenerateSslRoot();
    const certificate = await client.exportSslRoot("pem");

    expect(certificate.size).toBe("ROOT CERTIFICATE".length);
    expect(certificate.type).toBe("application/x-pem-file");
    expect(requestFetch).toHaveBeenNthCalledWith(
      1,
      "http://127.0.0.1:17890/api/v1/ssl",
      expect.objectContaining({ method: "GET" }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      2,
      "http://127.0.0.1:17890/api/v1/ssl",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify(update),
      }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      3,
      "http://127.0.0.1:17890/api/v1/ssl/ca/generate",
      expect.objectContaining({ method: "POST" }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      4,
      "http://127.0.0.1:17890/api/v1/ssl/ca/export?format=pem",
      expect.objectContaining({ method: "GET" }),
    );
  });
});

describe("M6 Protobuf 控制客户端", () => {
  /** 描述符读取、路由更新和上传必须复用唯一控制面，上传载荷只在专用端点发送。 */
  it("使用严格路由读取、更新并上传描述符", async () => {
    const configuration = { enabled: false, schemas: [], routes: [] };
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(JSON.stringify(configuration), { status: 200 }),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify(configuration), { status: 200 }),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify(configuration), { status: 200 }),
      );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);
    const update = { enabled: true, routes: [] };
    const upload = {
      name: "fixture",
      defaultMessageType: "fixture.Envelope",
      base64: "AA==",
    };

    await client.getProtobufConfiguration();
    await client.updateProtobufConfiguration(update);
    await client.uploadProtobufDescriptor(upload);

    expect(requestFetch).toHaveBeenNthCalledWith(
      1,
      "http://127.0.0.1:17890/api/v1/tools/protobuf",
      expect.objectContaining({ method: "GET" }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      2,
      "http://127.0.0.1:17890/api/v1/tools/protobuf",
      expect.objectContaining({ method: "PUT", body: JSON.stringify(update) }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      3,
      "http://127.0.0.1:17890/api/v1/tools/protobuf/schemas",
      expect.objectContaining({ method: "POST", body: JSON.stringify(upload) }),
    );
  });
});

describe("M1d 录制与事务控制客户端", () => {
  it("使用严格路由读取、更新并清空录制状态", async () => {
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(createRecordingResponse())
      .mockResolvedValueOnce(createRecordingResponse("paused"))
      .mockResolvedValueOnce(createRecordingResponse());
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await client.getRecording();
    const updated = await client.updateRecording({
      state: "paused",
    });
    await client.clearRecording();

    expect(updated.recording.state).toBe("paused");
    expect(requestFetch).toHaveBeenNthCalledWith(
      1,
      "http://127.0.0.1:17890/api/v1/recording",
      expect.objectContaining({ method: "GET" }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      2,
      "http://127.0.0.1:17890/api/v1/recording",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({
          state: "paused",
        }),
      }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      3,
      "http://127.0.0.1:17890/api/v1/recording/clear",
      expect.objectContaining({ method: "POST" }),
    );
  });

  it("只用响应 nextOffset 和 collectionToken 构造后续分页请求", async () => {
    const transaction = createTransactionSummary();
    const initialPage = {
      ...createServiceSnapshot().transactions,
      revision: 2,
      collectionToken: "recording-alpha:7&generation=1",
      total: 2,
      offset: 0,
      limit: 1,
      hasMore: true,
      nextOffset: 1,
      truncated: true,
      items: [transaction],
    };
    const finalPage = {
      ...initialPage,
      offset: 1,
      hasPrevious: true,
      hasMore: false,
      nextOffset: null,
      items: [
        createTransactionSummary({
          transactionId: "transaction-beta",
          sequence: 2,
        }),
      ],
    };
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(JSON.stringify(initialPage), { status: 200 }),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify(finalPage), { status: 200 }),
      );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);
    const cancellation = new AbortController();

    const firstPage = await client.listTransactions(
      { offset: 0, limit: 1 },
      cancellation.signal,
    );
    const secondPage = await client.listTransactions(
      {
        offset: firstPage.nextOffset ?? 0,
        limit: 1,
        collectionToken: firstPage.collectionToken,
      },
      cancellation.signal,
    );

    expect(secondPage.items[0]?.transactionId).toBe("transaction-beta");
    expect(requestFetch).toHaveBeenNthCalledWith(
      1,
      "http://127.0.0.1:17890/api/v1/transactions?offset=0&limit=1",
      expect.objectContaining({
        method: "GET",
        signal: cancellation.signal,
      }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      2,
      "http://127.0.0.1:17890/api/v1/transactions?offset=1&limit=1&collectionToken=recording-alpha%3A7%26generation%3D1",
      expect.objectContaining({
        method: "GET",
        signal: cancellation.signal,
      }),
    );
  });

  it("在发出请求前拒绝脱离集合令牌的后续偏移", async () => {
    const requestFetch = vi.fn<typeof fetch>();
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    expect(() =>
      client.listTransactions({ offset: 1, limit: 100 }),
    ).toThrow(Error);
    expect(() =>
      client.listTransactions({
        limit: 100,
        collectionToken: "stale-token",
      }),
    ).toThrow(Error);
    expect(() =>
      client.listTransactions({
        offset: 1,
        limit: 100,
        collectionToken: "x".repeat(129),
      }),
    ).toThrow(Error);
    expect(requestFetch).not.toHaveBeenCalled();
  });

  it("编码事务路径并保持两侧正文的 base64 字符串", async () => {
    const transactionId = "id/with?delimiter #";
    const requestBase64 = "AAEC";
    const responseBase64 = "AQID";
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(createTransactionDetailResponse(transactionId))
      .mockResolvedValueOnce(
        createBodyResponse(transactionId, "request", requestBase64),
      )
      .mockResolvedValueOnce(
        createBodyResponse(transactionId, "response", responseBase64),
      );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    const detail = await client.getTransactionDetail(transactionId);
    const requestBody = await client.getRequestBody(transactionId);
    const responseBody = await client.getResponseBody(transactionId);

    expect(detail.transaction.transactionId).toBe(transactionId);
    expect(requestBody.base64).toBe(requestBase64);
    expect(responseBody.base64).toBe(responseBase64);
    const encodedPath = "id%2Fwith%3Fdelimiter%20%23";
    expect(requestFetch).toHaveBeenNthCalledWith(
      1,
      `http://127.0.0.1:17890/api/v1/transactions/${encodedPath}`,
      expect.objectContaining({ method: "GET" }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      2,
      `http://127.0.0.1:17890/api/v1/transactions/${encodedPath}/request/body`,
      expect.objectContaining({ method: "GET" }),
    );
    expect(requestFetch).toHaveBeenNthCalledWith(
      3,
      `http://127.0.0.1:17890/api/v1/transactions/${encodedPath}/response/body`,
      expect.objectContaining({ method: "GET" }),
    );
  });

  it("严格解析自动识别的应用层解密视图且保留原始正文", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockResolvedValue(
      createBodyResponse("encrypted-response", "response", "cmF3", {
        algorithm: "aes128EcbPkcs7Json",
        contentType: "application/json;charset=UTF-8",
        decodedBytes: 12,
        base64: "eyJjb2RlIjoyMDB9",
      }),
    );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    const body = await client.getResponseBody("encrypted-response");

    expect(body.base64).toBe("cmF3");
    expect(body.decoded).toEqual({
      algorithm: "aes128EcbPkcs7Json",
      contentType: "application/json;charset=UTF-8",
      decodedBytes: 12,
      base64: "eyJjb2RlIjoyMDB9",
    });
  });

  it("读取媒体 Range 虚拟重组但不替换原始正文路径", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(new Uint8Array([0, 1, 2]), {
        status: 200,
        headers: {
          "Content-Type": "audio/mp4",
          "Content-Length": "3",
          "X-Media-Preview-Status": "continuousPrefix",
          "X-Media-Preview-Captured-Bytes": "3",
          "X-Media-Preview-Total-Bytes": "9",
          "X-Media-Preview-Segment-Count": "2",
        },
      }),
    );
    const client = new HttpControlClient(
      "http://127.0.0.1:17890",
      requestFetch,
    );

    const preview = await client.getResponseMediaPreview("id/range");

    expect(preview.status).toBe("continuousPrefix");
    expect(preview.capturedBytes).toBe(3);
    expect(preview.mimeType).toBe("audio/mp4");
    expect(preview.streamUrl).toBe(
      "http://127.0.0.1:17890/api/v1/transactions/id%2Frange/response/media-preview",
    );
    expect(requestFetch).toHaveBeenCalledWith(
      "http://127.0.0.1:17890/api/v1/transactions/id%2Frange/response/media-preview",
      expect.objectContaining({ method: "HEAD" }),
    );
  });

  it("媒体预览只读取 HEAD 元数据且不聚合 Blob、JSON 或 Base64", async () => {
    const atobSpy = vi.spyOn(globalThis, "atob");
    const response = new Response(new Uint8Array(1024), {
      status: 200,
      headers: {
        "Content-Type": "audio/mp4",
        "Content-Length": "1024",
        "X-Media-Preview-Status": "complete",
        "X-Media-Preview-Captured-Bytes": "1024",
        "X-Media-Preview-Total-Bytes": "1024",
        "X-Media-Preview-Segment-Count": "4",
      },
    });
    const jsonSpy = vi.spyOn(response, "json");
    const blobSpy = vi.spyOn(response, "blob");
    const client = new HttpControlClient(
      "http://127.0.0.1:17890",
      vi.fn<typeof fetch>().mockResolvedValue(response),
    );

    const preview = await client.getResponseMediaPreview("audio-large");

    expect(preview.streamUrl).toContain("/response/media-preview");
    expect(blobSpy).not.toHaveBeenCalled();
    expect(jsonSpy).not.toHaveBeenCalled();
    expect(atobSpy).not.toHaveBeenCalled();
    atobSpy.mockRestore();
  });

  it("拒绝把其他事务的详情绑定到当前选择", async () => {
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValue(createTransactionDetailResponse("transaction-other"));
    const client = new HttpControlClient(
      "http://127.0.0.1:17890",
      requestFetch,
    );

    await expect(
      client.getTransactionDetail("transaction-alpha"),
    ).rejects.toMatchObject({
      statusCode: null,
    });
  });

  it("拒绝把响应侧正文绑定到请求侧查看器", async () => {
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValue(createBodyResponse("transaction-alpha", "response"));
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await expect(
      client.getRequestBody("transaction-alpha"),
    ).rejects.toMatchObject({
      statusCode: null,
    });
  });

  it("保留 AbortError 供懒加载调用方丢弃过期请求", async () => {
    const cancellation = new AbortController();
    const abortError = new DOMException("请求已取消", "AbortError");
    const requestFetch = vi.fn<typeof fetch>().mockRejectedValue(abortError);
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);
    cancellation.abort();

    await expect(
      client.getResponseBody("transaction-alpha", cancellation.signal),
    ).rejects.toBe(abortError);
    expect(requestFetch).toHaveBeenCalledWith(
      "http://127.0.0.1:17890/api/v1/transactions/transaction-alpha/response/body",
      expect.objectContaining({ signal: cancellation.signal }),
    );
  });

  it("响应读取阶段取消时仍保留 AbortError", async () => {
    const cancellation = new AbortController();
    const abortError = new DOMException("响应读取已取消", "AbortError");
    const response = {
      ok: true,
      status: 200,
      json: vi.fn(async () => {
        cancellation.abort();
        throw abortError;
      }),
    } as unknown as Response;
    const requestFetch = vi.fn<typeof fetch>().mockResolvedValue(response);
    const client = new HttpControlClient(
      "http://127.0.0.1:17890",
      requestFetch,
    );

    await expect(
      client.getRecording(cancellation.signal),
    ).rejects.toBe(abortError);
  });

  it("拒绝非标准 base64 正文响应", async () => {
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValue(
        createBodyResponse("transaction-alpha", "response", "not_base64"),
      );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await expect(
      client.getResponseBody("transaction-alpha"),
    ).rejects.toMatchObject({
      statusCode: 200,
    });
  });

  /** 客户端身份导入必须保留浏览器 multipart 边界，并按协议字段传输密码而不写入 URL。 */
  it("以 multipart 上传按主机匹配的客户端证书身份", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockResolvedValue(createSslResponse());
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);
    const certificate = new File(["identity"], "client.p12");

    await client.importClientCertificate({
      name: "支付接口身份",
      format: "pkcs12",
      enabled: true,
      locations: [{
        protocol: "https",
        host: "api.example.com",
        port: "443",
        path: "",
        query: null,
      }],
      certificate,
      password: "container-password",
    });

    const [url, request] = requestFetch.mock.calls[0] ?? [];
    expect(url).toBe("http://127.0.0.1:17890/api/v1/ssl/client-certificates");
    expect(request?.headers).not.toHaveProperty("Content-Type");
    const body = request?.body as FormData;
    expect(body).toBeInstanceOf(FormData);
    expect(body.get("certificate")).toBe(certificate);
    expect(body.get("password")).toBe("container-password");
    expect(url).not.toContain("container-password");
  });

  /** Map Local 导入必须保留 FormData boundary 的浏览器生成权，并按 path/file 顺序提交目录层级。 */
  it("以 multipart 上传本地映射目录", async () => {
    const requestFetch = vi.fn<typeof fetch>().mockResolvedValue(
      new Response(
        JSON.stringify({
          localPath: "imports/import-id/site",
          fileCount: 2,
          totalBytes: 6,
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);
    const indexFile = new File(["index"], "index.html");
    const scriptFile = new File(["x"], "app.js");

    const result = await client.importMapLocalFiles({
      directory: true,
      files: [
        { file: indexFile, relativePath: "site/index.html" },
        { file: scriptFile, relativePath: "site/assets/app.js" },
      ],
    });

    expect(result.localPath).toBe("imports/import-id/site");
    const request = requestFetch.mock.calls[0]?.[1];
    expect(request?.body).toBeInstanceOf(FormData);
    expect(request?.headers).not.toHaveProperty("Content-Type");
    const entries = Array.from((request?.body as FormData).entries());
    expect(entries.map(([name]) => name)).toEqual([
      "directory",
      "path",
      "file",
      "path",
      "file",
    ]);
    expect(entries[1]?.[1]).toBe("site/index.html");
    expect(entries[3]?.[1]).toBe("site/assets/app.js");
  });

  /** 开放扩展配置必须原样走权威控制端点，客户端不添加任何能力授权字段。 */
  it("读写开放扩展平台配置并保留完整 Mod 意图", async () => {
    const configuration = {
      enabled: true,
      activeVersion: "2.0.0",
      moduleOrder: ["binaryProtocol"],
      subscriptionOverrides: {},
      failurePolicy: "failOpen" as const,
      limits: null,
      configurationSchemaVersion: "1.0.0",
      configuration: { framing: "lengthPrefix" },
      secretReferences: {},
      automaticRestart: true,
    };
    const responseDocument = {
      schemaVersion: 1,
      plugins: { "example.protocol": configuration },
    };
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValue(
        new Response(JSON.stringify(responseDocument), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    await expect(
      client.updateExtensionPlatformConfiguration("example.protocol", configuration),
    ).resolves.toEqual(responseDocument);
    expect(requestFetch).toHaveBeenCalledWith(
      "http://127.0.0.1:17890/api/v1/extensions/configuration/example.protocol",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify(configuration),
      }),
    );
  });

  /** 运行实例和调用追踪使用独立只读端点，诊断清理固定核对 204。 */
  it("读取并清空扩展运行诊断", async () => {
    const requestFetch = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify([
            {
              pluginId: "example.protocol",
              version: "2.0.0",
              runtimeKind: "native",
              instanceGeneration: 3,
              consecutiveFailures: 0,
              inFlightInvocations: 1,
            },
          ]),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(null, { status: 204 }),
      );
    const client = new HttpControlClient("http://127.0.0.1:17890", requestFetch);

    const snapshots = await client.getExtensionRuntimeSnapshots();
    expect(snapshots[0]?.runtimeKind).toBe("native");
    await expect(client.clearExtensionInvocationTraces()).resolves.toBeUndefined();
    expect(requestFetch.mock.calls.map(([url]) => url)).toEqual([
      "http://127.0.0.1:17890/api/v1/extensions/runtime",
      "http://127.0.0.1:17890/api/v1/extensions/traces",
    ]);
  });
});
