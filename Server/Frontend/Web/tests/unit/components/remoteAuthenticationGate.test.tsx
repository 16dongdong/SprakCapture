import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { RemoteAuthenticationGate } from "@/components/remoteAuthenticationGate";

describe("远程管理登录门禁", () => {
  /** 远程生产入口必须先校验会话；未登录时只显示共享管理员登录表单。 */
  it("在会话未授权时阻止工作台挂载", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(null, { status: 401 }));

    render(<RemoteAuthenticationGate authenticationRequired><div>受保护工作台</div></RemoteAuthenticationGate>);

    expect(await screen.findByRole("heading", { name: "Sprak Capture 远程管理" })).toBeInTheDocument();
    expect(screen.queryByText("受保护工作台")).toBeNull();
  });

  /** 登录成功必须在同一组件周期内挂载工作台，且请求只携带管理员账号与密码。 */
  it("使用共享管理员凭据登录后进入工作台", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(new Response(null, { status: 401 }))
      .mockResolvedValueOnce(new Response(null, { status: 200 }));
    render(<RemoteAuthenticationGate authenticationRequired><div>受保护工作台</div></RemoteAuthenticationGate>);
    fireEvent.change(await screen.findByLabelText("管理员账号"), { target: { value: "Admin" } });
    fireEvent.change(screen.getByLabelText("管理员密码"), { target: { value: "Admin123" } });
    fireEvent.click(screen.getByRole("button", { name: "登录" }));

    expect(await screen.findByText("受保护工作台")).toBeInTheDocument();
    const loginRequest = fetchMock.mock.calls[1];
    expect(loginRequest?.[0]).toBe("/api/v1/auth/login");
    expect(JSON.parse(String((loginRequest?.[1] as RequestInit).body))).toEqual({
      username: "Admin",
      password: "Admin123",
    });
  });

  /** 开发与桌面模式显式免认证，防止本机调试因为远程设置而出现重复登录入口。 */
  it("开发入口直接挂载工作台且不请求认证接口", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");
    render(<RemoteAuthenticationGate authenticationRequired={false}><div>开发工作台</div></RemoteAuthenticationGate>);
    await waitFor(() => expect(screen.getByText("开发工作台")).toBeInTheDocument());
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
