import {
  useCallback,
  useEffect,
  useState,
  type FormEvent,
  type PropsWithChildren,
} from "react";
import { useTranslation } from "react-i18next";

import { shouldUseSameOriginControl } from "../api/controlEndpoint";

type AuthenticationState = "checking" | "authenticated" | "anonymous";

interface RemoteAuthenticationGateProps extends PropsWithChildren {
  authenticationRequired?: boolean;
}

/**
 * 在生产远程入口建立统一登录门禁；桌面 WebView 与 Vite 开发态明确免认证。
 *
 * 运行上下文：包裹整个 ServiceProvider，确保未登录时不会先建立控制 API 或 SSE 连接。
 * 参数：children 是完成授权后挂载的完整工作台。
 * 失败语义：会话检查失败进入登录页；登录失败展示服务端受控错误且不创建工作台状态。
 */
export function RemoteAuthenticationGate({
  children,
  authenticationRequired = shouldUseSameOriginControl(),
}: RemoteAuthenticationGateProps) {
  const { t } = useTranslation();
  const [authenticationState, setAuthenticationState] = useState<AuthenticationState>(
    authenticationRequired ? "checking" : "authenticated",
  );
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [errorMessage, setErrorMessage] = useState("");
  const [submitting, setSubmitting] = useState(false);

  /** 查询同源持久会话；任何网络或非成功响应均按未登录处理，避免远程页面短暂暴露。 */
  const checkSession = useCallback(async () => {
    if (!authenticationRequired) {
      setAuthenticationState("authenticated");
      return;
    }
    const response = await fetch("/api/v1/auth/session", {
      credentials: "same-origin",
      cache: "no-store",
    }).catch(() => null);
    setAuthenticationState(response?.ok === true ? "authenticated" : "anonymous");
  }, [authenticationRequired]);

  useEffect(() => {
    void checkSession();
  }, [checkSession]);

  /** 提交共享管理员身份并建立十年持久 Cookie；浏览器退出前不会主动清除会话。 */
  const login = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setSubmitting(true);
    setErrorMessage("");
    try {
      const response = await fetch("/api/v1/auth/login", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username, password }),
      });
      if (!response.ok) {
        const payload = await response.json().catch(() => null) as { message?: string } | null;
        setErrorMessage(payload?.message ?? t("page.remoteLogin.failed"));
        return;
      }
      setPassword("");
      setAuthenticationState("authenticated");
    } catch {
      setErrorMessage(t("page.remoteLogin.failed"));
    } finally {
      setSubmitting(false);
    }
  };

  if (authenticationState === "authenticated") {
    return children;
  }
  if (authenticationState === "checking") {
    return <main className="remoteLoginPage"><p role="status">{t("page.remoteLogin.checking")}</p></main>;
  }
  return (
    <main className="remoteLoginPage">
      <form className="remoteLoginCard" onSubmit={(event) => void login(event)}>
        <header><h1>{t("page.remoteLogin.title")}</h1><p>{t("page.remoteLogin.description")}</p></header>
        <label><span>{t("page.remoteLogin.username")}</span><input autoComplete="username" required value={username} onChange={(event) => setUsername(event.target.value)} /></label>
        <label><span>{t("page.remoteLogin.password")}</span><input autoComplete="current-password" required type="password" value={password} onChange={(event) => setPassword(event.target.value)} /></label>
        {errorMessage ? <p className="inlineError" role="alert">{errorMessage}</p> : null}
        <button className="primaryButton" disabled={submitting} type="submit">{submitting ? t("page.remoteLogin.submitting") : t("page.remoteLogin.submit")}</button>
      </form>
    </main>
  );
}
