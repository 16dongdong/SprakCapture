import {
  Activity,
  ArrowDown,
  ArrowUp,
  ExternalLink,
  ScanSearch,
  X,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useTranslation } from "react-i18next";

import {
  combineTrafficMetrics,
  formatByteCount,
  presentProxyEntryPoints,
  presentServiceState,
} from "../components/presentation";
import { StatusActionButton } from "../components/statusActionButton";
import i18n from "../i18n";
import {
  closeCurrentManagedWindow,
  showMainWindow,
} from "../platform/managedWindow";
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
 * 启动悬浮窗自绘标题区的拖动；按钮区域保留点击语义，非 Tauri 浏览器窗口不调用原生 API。
 * 失败语义：原生拖动失败只记录诊断，不阻断悬浮面板中的状态查看和关闭操作。
 */
async function startFloatingWindowDrag(
  event: ReactPointerEvent<HTMLElement>,
): Promise<void> {
  if (!isTauri() || event.button !== 0) {
    return;
  }
  const target = event.target;
  if (target instanceof Element && target.closest("button") !== null) {
    return;
  }
  try {
    await getCurrentWindow().startDragging();
  } catch (error) {
    console.error("启动悬浮窗拖动失败", error);
  }
}

/** 关闭当前悬浮面板；桌面端由生命周期钩子转为隐藏，浏览器弹窗直接关闭。 */
async function closeFloatingWindow(): Promise<void> {
  try {
    await closeCurrentManagedWindow();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`关闭悬浮窗失败：${message}`);
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
  // 代理监听与 WinDivert 使用独立计数器；悬浮窗必须复用工作台聚合口径，不能只读取代理侧的零值。
  const metrics = snapshot === null ? null : combineTrafficMetrics(snapshot);
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
      <header
        className="floatingDragRegion"
        onPointerDown={(event) => void startFloatingWindowDrag(event)}
      >
        <div className="floatingTitle">
          <ScanSearch aria-hidden="true" size={17} />
          <strong>Sprak Capture</strong>
        </div>
        <div className="floatingWindowActions">
          <button
            className="iconButton"
            type="button"
            onClick={() => void openMainWindow()}
            aria-label={t("floating.openMainWindow")}
            title={t("floating.openMainWindow")}
          >
            <ExternalLink aria-hidden="true" size={15} />
          </button>
          <button
            className="iconButton"
            type="button"
            onClick={() => void closeFloatingWindow()}
            aria-label={t("floating.close")}
            title={t("floating.close")}
          >
            <X aria-hidden="true" size={15} />
          </button>
        </div>
      </header>
      <section className="floatingStatus">
        <span
          className={`largeStatusDot largeStatusDot--${presentation?.tone ?? "neutral"}`}
        />
        <div>
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
