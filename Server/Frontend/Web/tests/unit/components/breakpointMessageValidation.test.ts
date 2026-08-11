import { describe, expect, it } from "vitest";

import type { EditableHttpMessage } from "@/api/protocol";
import { validateBreakpointMessage } from "@/components/breakpointMessageValidation";

const requestDraft: EditableHttpMessage = {
  method: "GET",
  url: "https://origin.example.test/resource",
  statusCode: null,
  reason: null,
  headers: [],
  bodyBase64: "",
};

describe("断点报文前置校验", () => {
  /** 请求继续只接受绝对 HTTP/HTTPS URL、合法方法与 HTTP 头，避免错误在挂起后才变成通用失败。 */
  it("阻止不支持的 URL 协议和非法头字段", () => {
    expect(
      validateBreakpointMessage(
        { ...requestDraft, url: "ftp://origin.example.test/file" },
        "request",
      ),
    ).toBe("url");
    expect(
      validateBreakpointMessage(
        { ...requestDraft, headers: [{ name: "Bad Header", value: "value" }] },
        "request",
      ),
    ).toBe("headers");
  });

  /** 响应状态和 Base64 正文必须保留后端可回写范围，二进制编辑不接受不完整编码。 */
  it("阻止无效响应状态和非标准 Base64", () => {
    const responseDraft: EditableHttpMessage = {
      ...requestDraft,
      method: null,
      url: null,
      statusCode: 99,
    };
    expect(validateBreakpointMessage(responseDraft, "response")).toBe("statusCode");
    expect(
      validateBreakpointMessage(
        { ...responseDraft, statusCode: 200, bodyBase64: "not-base64" },
        "response",
      ),
    ).toBe("body");
  });
});
