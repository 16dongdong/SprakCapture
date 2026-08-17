import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { defaultRuntimeControlBaseUrl } from "../api/controlEndpoint";
import { useServiceStore } from "../state/serviceStore";

/**
 * 把账号服务返回的受控相对路径解析到真正的控制端点。
 *
 * Tauri 页面来源是 `http://tauri.localhost`，相对地址会再次加载 Sprak Capture
 * 主 SPA，形成递归嵌套；使用控制端点绝对地址后，请求才会经过账号管理映射。
 * 参数：sessionPath 为一次性登录路径，只允许由控制接口返回的 `/account-management/` 前缀路径。
 * 失败语义：非法路径由 URL 构造器抛出，调用方不加载不确定的页面地址。
 */
export function resolveAccountManagementUrl(sessionPath: string): string {
  return new URL(sessionPath, defaultRuntimeControlBaseUrl()).toString();
}

/** 隔离生产环境顶层导航和单元测试观察器；不向页面暴露票据之外的认证材料。 */
export interface AccountManagementPageProperties {
  navigate?: (managementUrl: string) => void;
}

/**
 * 把当前独立窗口顶层导航到账号管理入口。
 *
 * 运行上下文：一次性票据签发成功后调用；顶层导航使账号服务设置的持久 Cookie
 * 处于第一方上下文，避免跨来源 iframe 丢失认证状态。参数为经过固定控制端点解析的地址。
 * 失败语义：浏览器拒绝导航时由运行时保留原页面，调用方不会把失败伪装成已登录。
 */
function replaceCurrentPage(managementUrl: string): void {
  window.location.replace(managementUrl);
}

/**
 * 在独立账号管理窗口中签发一次性会话，并把整个窗口导航到账号服务页面。
 *
 * 运行上下文：概览按钮已同步创建独立窗口，本页面只负责异步取票和顶层跳转；
 * 参数 navigate 仅用于隔离浏览器导航边界和测试。票据签发失败时保留明确错误页，不加载未授权地址。
 */
export function AccountManagementPage({
  navigate = replaceCurrentPage,
}: AccountManagementPageProperties = {}) {
  const { t } = useTranslation();
  const { createManagementSession, lastError } = useServiceStore();

  useEffect(() => {
    let active = true;
    void createManagementSession().then((response) => {
      if (active && response !== null) {
        navigate(resolveAccountManagementUrl(response.path));
      }
    });
    return () => {
      active = false;
    };
  }, [createManagementSession, navigate]);

  return (
    <main className="accountManagementPage">
      <div
        className="accountManagementLoading"
        role={lastError ? "alert" : "status"}
      >
        {lastError ?? t("page.accountManagement.loading")}
      </div>
    </main>
  );
}
