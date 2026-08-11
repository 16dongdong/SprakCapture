import {
  Activity,
  ArrowDown,
  ArrowUp,
  CircleCheck,
  CircleX,
  Network,
  Server,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  formatByteCount,
  presentConnectionState,
  presentProxyEntryPoints,
  presentServiceState,
} from "../components/presentation";
import { StatusActionButton } from "../components/statusActionButton";
import { useServiceStore } from "../state/serviceStore";

/**
 * 渲染一个扁平指标单元；未知值保持短横线而不是伪造零。
 */
function MetricCell({
  label,
  value,
  icon: MetricIcon,
}: {
  label: string;
  value: string;
  icon: typeof Activity;
}) {
  return (
    <div className="metricCell">
      <MetricIcon aria-hidden="true" size={16} />
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

/**
 * 展示统一流量捕获服务、全部代理入口以及代理与 WinDivert 合并后的实时指标。
 *
 * 运行上下文：主窗口概览路由消费共享服务快照，不直接管理任一监听器生命周期。
 * 参数：无；状态与动作均来自 ServiceStore。
 * 失败语义：快照暂不可用时显示连接状态和不可用入口；透明捕获只合并同语义字段，不把数据包数冒充连接数。
 */
export function OverviewPage() {
  const { t } = useTranslation();
  const {
    snapshot,
    controlConnection,
    eventConnection,
    connectionMessage,
    lastError,
  } = useServiceStore();
  const connected = controlConnection === "connected";
  const metrics = snapshot?.metrics;
  const processCapture = snapshot?.processCapture;
  const activeConnections =
    metrics && processCapture
      ? metrics.activeConnections + processCapture.trackedFlows
      : null;
  const acceptedConnections =
    metrics && processCapture
      ? metrics.acceptedConnections + processCapture.acceptedConnections
      : null;
  const bytesUp =
    metrics && processCapture ? metrics.bytesUp + processCapture.bytesUp : null;
  const bytesDown =
    metrics && processCapture
      ? metrics.bytesDown + processCapture.bytesDown
      : null;
  const sessionCount = snapshot
    ? snapshot.sessions.length + snapshot.processCapture.acceptedConnections
    : null;
  const servicePresentation =
    snapshot === null ? null : presentServiceState(snapshot.serviceState);
  const proxyEntryPoints =
    snapshot === null
      ? t("app.service.endpointUnavailable")
      : presentProxyEntryPoints(
          snapshot.listeners,
          t("app.service.endpointUnavailable"),
        );
  const listenerErrorCodes = snapshot
    ? [snapshot.listeners.httpProxy.error?.code, snapshot.listeners.socks5.error?.code]
    : [];
  const diagnosticDetail = listenerErrorCodes.includes(
    "processCaptureStartFailed",
  )
    ? t("page.overview.processCaptureStartFailed")
    : lastError || connectionMessage;

  return (
    <main className="pageShell overviewPage">
      <header className="pageHeader">
        <div>
          <h1>{t("page.overview.title")}</h1>
          <p>{t("page.overview.subtitle")}</p>
        </div>
      </header>

      <section className="serviceSummary" aria-label={t("app.service.name")}>
        <div className="serviceIdentity">
          <span
            className={`largeStatusDot largeStatusDot--${servicePresentation?.tone ?? "neutral"}`}
          />
          <div>
            <small>{t("app.service.name")}</small>
            <strong>
              {servicePresentation?.label ?? t("page.overview.disconnected")}
            </strong>
            <span>
              {proxyEntryPoints}
            </span>
          </div>
        </div>
        <div className="serviceControl">
          <div className="connectionReadiness">
            {connected ? (
              <CircleCheck aria-hidden="true" size={18} />
            ) : (
              <CircleX aria-hidden="true" size={18} />
            )}
            <div>
              <strong>
                {connected
                  ? t("page.overview.controlConnected")
                  : t("page.overview.controlDisconnected")}
              </strong>
              <span>
                {t("page.overview.eventStream")}
                {eventConnection === "connected"
                  ? t("app.connectionState.connected")
                  : eventConnection === "connecting"
                    ? t("app.connectionState.connecting")
                    : t("app.connectionState.disconnected")}
              </span>
            </div>
          </div>
          <StatusActionButton />
        </div>
      </section>

      <section
        className="metricsGrid"
        aria-label={t("page.overview.metricsLabel")}
      >
        <MetricCell
          label={t("page.overview.activeConnections")}
          value={activeConnections === null ? "—" : String(activeConnections)}
          icon={Activity}
        />
        <MetricCell
          label={t("page.overview.acceptedConnections")}
          value={
            acceptedConnections === null ? "—" : String(acceptedConnections)
          }
          icon={Network}
        />
        <MetricCell
          label={t("page.overview.failedConnections")}
          value={metrics ? String(metrics.failedConnections) : "—"}
          icon={CircleX}
        />
        <MetricCell
          label={t("page.overview.bytesUp")}
          value={bytesUp === null ? "—" : formatByteCount(bytesUp)}
          icon={ArrowUp}
        />
        <MetricCell
          label={t("page.overview.bytesDown")}
          value={bytesDown === null ? "—" : formatByteCount(bytesDown)}
          icon={ArrowDown}
        />
        <MetricCell
          label={t("page.overview.sessionCount")}
          value={sessionCount === null ? "—" : String(sessionCount)}
          icon={Server}
        />
      </section>

      <section className="diagnosticPanel">
        <h2>{t("page.overview.connectionStatus")}</h2>
        <dl>
          <div>
            <dt>{t("page.overview.controlApi")}</dt>
            <dd>{presentConnectionState(controlConnection)}</dd>
          </div>
          <div>
            <dt>{t("page.overview.eventApi")}</dt>
            <dd>{presentConnectionState(eventConnection)}</dd>
          </div>
          <div>
            <dt>{t("page.overview.detail")}</dt>
            <dd>{diagnosticDetail}</dd>
          </div>
        </dl>
      </section>
    </main>
  );
}
