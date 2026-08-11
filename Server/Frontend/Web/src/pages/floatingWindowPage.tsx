import {
  Activity,
  ArrowDown,
  ArrowUp,
  ExternalLink,
  ScanSearch,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  formatByteCount,
  presentProxyEntryPoints,
  presentServiceState,
} from "../components/presentation";
import { StatusActionButton } from "../components/statusActionButton";
import i18n from "../i18n";
import { showMainWindow } from "../platform/managedWindow";
import { useServiceStore } from "../state/serviceStore";

/**
 * 请求显示主窗口；Tauri 恢复受管窗口，浏览器保留连接页命名窗口调试路径。
 */
async function openMainWindow(): Promise<void> {
  try {
    await showMainWindow();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(i18n.t("error.web.openMainWindow", { message }));
  }
}

/**
 * 渲染轻量悬浮面板；状态、代理入口和动作与主窗口来自同一服务事件及广播通道。
 *
 * 运行上下文：后台运行时保持最小信息密度，但产品身份仍是统一流量捕获服务而非单一 SOCKS5 监听器。
 * 参数：无；组件从 ServiceStore 读取权威快照。
 * 失败语义：控制面断开时保留明确断开状态和不可用入口，启停失败由共享错误状态反馈。
 */
export function FloatingWindowPage() {
  const { t } = useTranslation();
  const { snapshot, controlConnection, lastError } = useServiceStore();
  const metrics = snapshot?.metrics;
  const presentation =
    snapshot === null ? null : presentServiceState(snapshot.serviceState);
  const proxyEntryPoints =
    snapshot === null
      ? t("app.service.endpointUnavailable")
      : presentProxyEntryPoints(
          snapshot.listeners,
          t("app.service.endpointUnavailable"),
        );
  const controlDetail =
    lastError ||
    (controlConnection === "connected"
      ? t("floating.controlConnected")
      : t("floating.controlDisconnected"));

  return (
    <main className="floatingPanel">
      <header>
        <div className="floatingTitle">
          <ScanSearch aria-hidden="true" size={17} />
          <strong>Sprak Capture</strong>
        </div>
        <button
          className="iconButton"
          type="button"
          onClick={() => void openMainWindow()}
          aria-label={t("floating.openMainWindow")}
          title={t("floating.openMainWindow")}
        >
          <ExternalLink aria-hidden="true" size={15} />
        </button>
      </header>
      <section className="floatingStatus">
        <span
          className={`largeStatusDot largeStatusDot--${presentation?.tone ?? "neutral"}`}
        />
        <div>
          <small>{t("app.service.name")}</small>
          <strong>{presentation?.label ?? t("floating.disconnected")}</strong>
          <span>{proxyEntryPoints}</span>
        </div>
      </section>
      <section className="floatingMetrics">
        <div>
          <Activity aria-hidden="true" size={14} />
          <span>{t("floating.active")}</span>
          <strong>{metrics?.activeConnections ?? "—"}</strong>
        </div>
        <div>
          <ArrowUp aria-hidden="true" size={14} />
          <span>{t("floating.up")}</span>
          <strong>{metrics ? formatByteCount(metrics.bytesUp) : "—"}</strong>
        </div>
        <div>
          <ArrowDown aria-hidden="true" size={14} />
          <span>{t("floating.down")}</span>
          <strong>{metrics ? formatByteCount(metrics.bytesDown) : "—"}</strong>
        </div>
      </section>
      <StatusActionButton />
      <p
        className={controlConnection === "connected" ? "" : "isError"}
        title={controlDetail}
      >
        {controlDetail}
      </p>
    </main>
  );
}
