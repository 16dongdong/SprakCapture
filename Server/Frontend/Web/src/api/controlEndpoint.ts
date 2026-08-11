/**
 * 控制服务的默认 HTTP 基础地址；桌面端与开发 Web 都从这一单一来源派生接口端点。
 */
export const defaultControlBaseUrl = "http://127.0.0.1:17890";

/**
 * 规范化控制服务基础地址。
 *
 * 运行上下文：构造 REST 与 WebSocket 客户端前执行，确保二者使用同一协议、主机、端口和可选路径前缀。
 * 参数：controlBaseUrl 为构建期注入的绝对 HTTP(S) 地址；未提供时使用本机守护进程默认地址。
 * 失败语义：地址为空、包含空白边界、携带凭据或查询片段、或不是 HTTP(S) 绝对地址时抛出 TypeError，阻止前端连接到不确定端点。
 */
export function resolveControlBaseUrl(controlBaseUrl?: string): string {
  const candidate = controlBaseUrl ?? defaultControlBaseUrl;
  if (candidate.length === 0 || candidate.trim() !== candidate) {
    throw new TypeError("控制接口基础地址不能为空且不能包含首尾空白");
  }

  let parsedUrl: URL;
  try {
    parsedUrl = new URL(candidate);
  } catch (error) {
    throw new TypeError("控制接口基础地址必须是绝对 HTTP 或 HTTPS 地址", {
      cause: error,
    });
  }

  if (
    (parsedUrl.protocol !== "http:" && parsedUrl.protocol !== "https:") ||
    parsedUrl.username.length > 0 ||
    parsedUrl.password.length > 0 ||
    parsedUrl.search.length > 0 ||
    parsedUrl.hash.length > 0
  ) {
    throw new TypeError("控制接口基础地址必须是不含凭据、查询参数和片段的 HTTP 或 HTTPS 地址");
  }

  parsedUrl.pathname = parsedUrl.pathname.replace(/\/+$/, "");
  return parsedUrl.toString().replace(/\/$/, "");
}

/**
 * 由控制服务基础地址生成事件流地址。
 *
 * 运行上下文：REST 与事件流必须连接同一个控制服务实例，HTTPS 控制面对应 WSS，HTTP 控制面对应 WS。
 * 参数：controlBaseUrl 使用与 HttpControlClient 相同的基础地址规则。
 * 失败语义：基础地址不满足控制面约束时透传 resolveControlBaseUrl 的 TypeError。
 */
export function deriveEventStreamUrl(controlBaseUrl?: string): string {
  const eventUrl = new URL(resolveControlBaseUrl(controlBaseUrl));
  eventUrl.protocol = eventUrl.protocol === "https:" ? "wss:" : "ws:";
  eventUrl.pathname = `${eventUrl.pathname.replace(/\/+$/, "")}/api/v1/events`;
  return eventUrl.toString();
}

/**
 * 由控制服务地址生成 SSE 端点；单向实时视图复用 HTTP 连接语义，避免为状态展示建立双向通道。
 *
 * 运行上下文：ServiceProvider 创建默认事件客户端时调用；返回值始终与 REST 指向同一后台实例。
 * 失败语义：基础地址非法时透传 resolveControlBaseUrl 的 TypeError，不生成猜测地址。
 */
export function deriveServerSentEventsUrl(controlBaseUrl?: string): string {
  const eventUrl = new URL(resolveControlBaseUrl(controlBaseUrl));
  eventUrl.pathname = `${eventUrl.pathname.replace(/\/+$/, "")}/api/v1/events/sse`;
  return eventUrl.toString();
}
