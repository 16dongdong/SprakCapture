import { Activity, Network } from "lucide-react";
import { useTranslation } from "react-i18next";

import {
  combineTrafficMetrics,
  presentProxyEntryPoints,
  presentServiceState,
} from "../components/presentation";
import { StatusActionButton } from "../components/statusActionButton";
import { MultiAccountOverview } from "../components/multiAccountOverview";
import { showIndependentWindow } from "../platform/independentWindowContract";
import { useServiceStore } from "../state/serviceStore";

/**
 * 渲染概览页统一规格的指标单元。
 *
 * 运行上下文：服务指标和账号指标共用该结构，参数提供本地化标签、已格式化值和对应图标。
 * 失败语义：未知值由调用方明确传入短横线，本函数不推断或伪造业务数据。
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
 * 失败语义：快照暂不可用时显示不可用入口；多账号开启时只保留账号连接实时口径，避免重复连接数。
 */
export function OverviewPage() {
  const { t } = useTranslation();
  const {
    snapshot,
    lastError,
    activeAction,
    getMultiAccountState,
  } = useServiceStore();
  const metrics = snapshot === null ? null : combineTrafficMetrics(snapshot);
  const activeConnections = metrics?.activeConnections ?? null;
  const acceptedConnections = metrics?.acceptedConnections ?? null;
  const servicePresentation =
    snapshot === null ? null : presentServiceState(snapshot.serviceState);
  const proxyEntryPoints =
    snapshot === null
      ? t("app.service.endpointUnavailable")
      : presentProxyEntryPoints(
          snapshot.listeners,
          t("app.service.endpointUnavailable"),
        );
  const multiAccountEnabled =
    snapshot?.configuration.multiAccount.enabled === true;

  /**
   * 在桌面端创建独立账号管理窗口；窗口内部通过一次性票据建立长期会话，
   * 不占用工作台路由，也不会再次要求管理员输入账号密码。浏览器环境复用命名窗口。
   * 创建失败仅记录错误，不伪造窗口已打开状态。
   */
  const openMultiAccountManagement = () => {
    void showIndependentWindow({ kind: "accountManagement" }).catch(
      (error: unknown) => {
        console.error("打开账号管理窗口失败", error);
      },
    );
  };

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
          <StatusActionButton />
        </div>
      </section>

      {snapshot !== null && multiAccountEnabled ? (
        <MultiAccountOverview
          configuration={snapshot.configuration.multiAccount}
          disabled={activeAction !== null}
          acceptedConnections={acceptedConnections}
          readState={getMultiAccountState}
          onOpenManagement={openMultiAccountManagement}
        />
      ) : null}

      {!multiAccountEnabled ? (
        <section
          className="metricsGrid"
          aria-label={t("page.overview.metricsLabel")}
        >
          <MetricCell
            label={t("page.overview.activeConnections")}
            value={
              activeConnections === null ? "—" : String(activeConnections)
            }
            icon={Activity}
          />
          <MetricCell
            label={t("page.overview.acceptedConnections")}
            value={
              acceptedConnections === null ? "—" : String(acceptedConnections)
            }
            icon={Network}
          />
        </section>
      ) : null}
      {lastError ? (
        <p className="inlineError" role="alert">
          {lastError}
        </p>
      ) : null}
    </main>
  );
}
