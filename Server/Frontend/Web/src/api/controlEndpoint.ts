/**
 * 控制服务的默认 HTTP 基础地址；桌面端与开发 Web 都从这一单一来源派生接口端点。
 */
export const defaultControlBaseUrl = "http://127.0.0.1:17890";

/**
 * 判断页面是否运行在 Tauri 的静态资源来源上。
 *
 * Tauri 2 生产构建默认使用 `http://tauri.localhost` 提供前端资源；该来源虽然是
 * HTTP，却不是控制 API。若把它误判成远程 Web 页面，REST 请求会落到静态资源服务，
 * 得到 HTML 而不是 JSON，桌面端就会显示“控制服务返回了无效 JSON”。
 */
export function isTauriRuntimeOrigin(location: Location): boolean {
  const hostname = location.hostname.toLowerCase();
  return hostname === "tauri.localhost" || hostname.endsWith(".tauri.localhost");
}

/**
 * 判断当前页面是否由远程 HTTP 入口托管；Tauri 静态来源和 Vite 开发态继续直连本机控制端口。
 *
 * 运行上下文：默认控制客户端和事件流创建时调用；远程生产页面与控制 API 必须保持同源 Cookie。
 * 失败语义：服务端渲染或缺少 window 时返回 false，不猜测部署地址。
 */
export function shouldUseSameOriginControl(): boolean {
  return !import.meta.env.DEV
    && typeof window !== "undefined"
    && (window.location.protocol === "http:" || window.location.protocol === "https:")
    && !isTauriRuntimeOrigin(window.location);
}

/// 选择当前运行环境的默认控制地址；远程生产页面使用同源，桌面和开发环境使用固定回环端口。
///
/// 运行上下文：未显式设置 `VITE_CONTROL_BASE_URL` 时使用。
/// 失败语义：远程 origin 仍由后续规范化校验，非法浏览器地址不会静默回退本机。
export function defaultRuntimeControlBaseUrl(): string {
  return shouldUseSameOriginControl() ? window.location.origin : defaultControlBaseUrl;
}

/**
 * 规范化控制服务基础地址。
 *
 * 运行上下文：构造 REST 与 WebSocket 客户端前执行，确保二者使用同一协议、主机、端口和可选路径前缀。
 * 参数：controlBaseUrl 为构建期注入的绝对 HTTP(S) 地址；未提供时使用本机守护进程默认地址。
 * 失败语义：地址为空、包含空白边界、携带凭据或查询片段、或不是 HTTP(S) 绝对地址时抛出 TypeError，阻止前端连接到不确定端点。
 */
export function resolveControlBaseUrl(controlBaseUrl?: string): string {
  const candidate = controlBaseUrl ?? defaultRuntimeControlBaseUrl();
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
