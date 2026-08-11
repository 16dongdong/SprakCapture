import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import type { EditableHttpMessage } from "@/api/protocol";
import i18n from "@/i18n";
import { BreakpointMessageEditor } from "@/components/breakpointMessageEditor";

const requestPayload: EditableHttpMessage = {
  method: "GET",
  url: "https://origin.example.com/resource",
  statusCode: null,
  reason: null,
  headers: [],
  bodyBase64: "aGVsbG8=",
};

/** 将断点编辑器包装为受控组件，使测试覆盖草稿回写和再次渲染的完整链路。 */
function ControlledBreakpointEditor({
  payload = requestPayload,
  onPayloadChange,
}: {
  payload?: EditableHttpMessage;
  onPayloadChange(next: EditableHttpMessage): void;
}) {
  const [draft, setDraft] = useState(payload);

  /** 更新本地草稿并暴露给断言，模拟命中断点对话框持有的事务草稿。 */
  const updateDraft = (next: EditableHttpMessage) => {
    setDraft(next);
    onPayloadChange(next);
  };

  return (
    <BreakpointMessageEditor
      payload={draft}
      phase="request"
      onChange={updateDraft}
    />
  );
}

describe("断点消息字段编辑器", () => {
  /** 请求消息以方法、URL、头和正文独立编辑，提交草稿始终保持后端要求的 Base64 正文表示。 */
  it("以普通字段编辑请求草稿而不暴露配置对象文本", async () => {
    const user = userEvent.setup();
    const onPayloadChange = vi.fn();
    render(<ControlledBreakpointEditor onPayloadChange={onPayloadChange} />);

    expect(screen.queryByLabelText(i18n.t("tools.configuration"))).not.toBeInTheDocument();
    await user.clear(screen.getByRole("textbox", { name: i18n.t("tools.form.method") }));
    await user.type(screen.getByRole("textbox", { name: i18n.t("tools.form.method") }), "POST");
    await user.clear(screen.getByRole("textbox", { name: i18n.t("tools.form.url") }));
    await user.type(
      screen.getByRole("textbox", { name: i18n.t("tools.form.url") }),
      "https://origin.example.com/updated",
    );
    await user.click(
      screen.getByRole("button", { name: i18n.t("tools.form.addHeader") }),
    );
    await user.type(
      screen.getByRole("textbox", { name: `${i18n.t("tools.form.headerName")} 1` }),
      "X-Trace",
    );
    await user.type(
      screen.getByRole("textbox", { name: `${i18n.t("tools.form.headerValue")} 1` }),
      "edited",
    );
    await user.clear(screen.getByRole("textbox", { name: i18n.t("tools.form.body") }));
    await user.type(screen.getByRole("textbox", { name: i18n.t("tools.form.body") }), "payload");

    expect(onPayloadChange).toHaveBeenLastCalledWith({
      method: "POST",
      url: "https://origin.example.com/updated",
      statusCode: null,
      reason: null,
      headers: [{ name: "X-Trace", value: "edited" }],
      bodyBase64: "cGF5bG9hZA==",
    });
  });

  /** 非 UTF-8 正文使用 Base64 编辑，避免文本转码破坏需要继续转发的原始字节。 */
  it("将二进制正文展示为可编辑 Base64", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <ControlledBreakpointEditor
        payload={{ ...requestPayload, bodyBase64: "/w==" }}
        onPayloadChange={onChange}
      />,
    );

    const binaryBody = screen.getByRole("textbox", {
      name: i18n.t("tools.form.binaryBody"),
    });
    expect(binaryBody).toHaveValue("/w==");
    await user.clear(binaryBody);
    await user.type(binaryBody, "AQI=");
    expect(onChange).toHaveBeenLastCalledWith({
      ...requestPayload,
      bodyBase64: "AQI=",
    });
    expect(
      screen.queryByRole("textbox", { name: i18n.t("tools.form.body") }),
    ).not.toBeInTheDocument();
  });

  /** 响应草稿只暴露后端可以稳定回写的状态、头和正文，不提供会被控制面拒绝的原因短语输入。 */
  it("以可回写字段编辑响应草稿而不暴露原因短语", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <BreakpointMessageEditor
        payload={{
          ...requestPayload,
          method: null,
          url: null,
          statusCode: 200,
        }}
        phase="response"
        onChange={onChange}
      />,
    );

    expect(screen.queryByRole("textbox", { name: i18n.t("tools.form.reason") })).not.toBeInTheDocument();
    await user.clear(
      screen.getByRole("spinbutton", { name: i18n.t("tools.form.statusCode") }),
    );
    await user.type(
      screen.getByRole("spinbutton", { name: i18n.t("tools.form.statusCode") }),
      "201",
    );

    expect(onChange).toHaveBeenLastCalledWith({
      ...requestPayload,
      method: null,
      url: null,
      statusCode: 201,
    });
  });
});
