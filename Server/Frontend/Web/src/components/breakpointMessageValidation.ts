import {
  maximumEncodedBodyCharacters,
  type EditableHttpMessage,
  type HeaderField,
} from "../api/protocol";
import type { BreakpointMessagePhase } from "./breakpointMessageEditor";

/** 断点报文表单中可定位的首个不可继续字段。 */
export type BreakpointMessageValidationField =
  | "method"
  | "url"
  | "statusCode"
  | "headers"
  | "body";

/** HTTP token 语法同时覆盖标准与扩展方法，具体方法可由上游按 HTTP 语义处理。 */
const methodTokenPattern = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
const headerNamePattern = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
const standardBase64Pattern = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;

/** 校验单个头字段可以由 HTTP 控制面线性化，不允许控制字符注入报文边界。 */
function hasValidHeader(header: HeaderField): boolean {
  if (!headerNamePattern.test(header.name)) {
    return false;
  }
  return Array.from(header.value).every((character) => {
    const code = character.charCodeAt(0);
    return code === 9 || (code >= 32 && code <= 126) || (code >= 128 && code <= 255);
  });
}

/** 校验可继续回写的 Base64 正文形状和上限，正文解码仍由后端进行最终字节校验。 */
function hasValidBodyBase64(bodyBase64: string): boolean {
  return (
    bodyBase64.length <= maximumEncodedBodyCharacters &&
    standardBase64Pattern.test(bodyBase64)
  );
}

/** 校验请求 URL 为后端可转发的绝对 HTTP/HTTPS URI，拒绝相对 URL 和其他协议。 */
function hasValidRequestUrl(url: string): boolean {
  if (url.length > 8_192) {
    return false;
  }
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

/**
 * 验证断点草稿可以安全继续到后端。
 * 请求与响应各自只暴露其可写字段；该函数先拦截普通表单错误，后端仍负责原子回写和最终 HTTP 解析。
 */
export function validateBreakpointMessage(
  payload: EditableHttpMessage,
  phase: BreakpointMessagePhase,
): BreakpointMessageValidationField | null {
  if (payload.headers.length > 256 || !payload.headers.every(hasValidHeader)) {
    return "headers";
  }
  if (!hasValidBodyBase64(payload.bodyBase64)) {
    return "body";
  }
  if (phase === "request") {
    if (payload.statusCode !== null || (payload.reason ?? "") !== "") {
      return "method";
    }
    if (
      payload.method !== null &&
      (payload.method.length > 32 || !methodTokenPattern.test(payload.method))
    ) {
      return "method";
    }
    if (payload.url !== null && !hasValidRequestUrl(payload.url)) {
      return "url";
    }
    return null;
  }
  if (payload.method !== null || payload.url !== null || (payload.reason ?? "") !== "") {
    return "statusCode";
  }
  if (
    payload.statusCode !== null &&
    (!Number.isInteger(payload.statusCode) ||
      payload.statusCode < 100 ||
      payload.statusCode > 599)
  ) {
    return "statusCode";
  }
  return null;
}
