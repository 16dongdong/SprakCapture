import { render, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  AccountManagementPage,
  resolveAccountManagementUrl,
} from "@/pages/accountManagementPage";

const createManagementSession = vi.fn(async () => ({
  path: "/account-management/api/v1/auth/local?ticket=test-ticket",
}));

vi.mock("@/state/serviceStore", () => ({
  useServiceStore: () => ({
    createManagementSession,
    lastError: null,
  }),
}));

describe("账号管理页面入口", () => {
  /** 会话路径必须进入控制端点映射，不能相对解析到桌面静态资源来源。 */
  it("把一次性路径解析到本机控制端点，避免 Tauri 主 SPA 递归嵌套", () => {
    expect(
      resolveAccountManagementUrl(
        "/account-management/api/v1/auth/local?ticket=test-ticket",
      ),
    ).toBe(
      "http://127.0.0.1:17890/account-management/api/v1/auth/local?ticket=test-ticket",
    );
  });

  /** 独立窗口必须执行顶层导航，禁止重新引入导致第一方 Cookie 丢失的 iframe。 */
  it("签发票据后顶层进入账号管理且不渲染嵌套页面", async () => {
    const navigate = vi.fn();
    const { container } = render(<AccountManagementPage navigate={navigate} />);

    await waitFor(() => {
      expect(navigate).toHaveBeenCalledWith(
        "http://127.0.0.1:17890/account-management/api/v1/auth/local?ticket=test-ticket",
      );
    });
    expect(container.querySelector("iframe")).toBeNull();
  });
});
