import {
  Activity,
  ArrowDown,
  ArrowUp,
  ExternalLink,
  Network,
  UsersRound,
} from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import type { MultiAccountPublicState, PublicConfiguration } from "../api/protocol";
import { formatByteCount } from "./presentation";

/** 描述账号服务指标区所需的控制面状态、轮询入口、管理动作和合并指标。 */
interface MultiAccountOverviewProps {
  configuration: PublicConfiguration["multiAccount"];
  disabled: boolean;
  acceptedConnections: number | null;
  readState(signal?: AbortSignal): Promise<MultiAccountPublicState | null>;
  onOpenManagement(): void | Promise<void>;
}

/** 将每秒字节数转换为紧凑速率；null 保留未知语义，零值明确显示为 0 B/s。 */
function formatTransferRate(value: number | null): string {
  return value === null ? "—" : `${formatByteCount(value)}/s`;
}

/**
 * 渲染与主概览一致的账号服务指标单元。
 *
 * 运行上下文：仅由合并指标网格调用；参数分别提供图标、当前语言标签和已格式化值。
 * 失败语义：未知值由调用方传入短横线，本组件不把缺失数据伪造成零。
 */
function AccountMetric({
  icon: MetricIcon,
  label,
  value,
}: {
  icon: typeof Activity;
  label: string;
  value: string;
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
 * 为主概览维护账号服务实时快照并展示一次性管理入口。
 *
 * 运行上下文：指标由父级合并进统一网格，本组件只负责轮询生命周期和账号服务标题栏。
 * 参数：configuration 为控制面严格快照，readState 读取实时局部状态，disabled 防止并发创建多个短期会话。
 * 失败语义：读取失败保留上一份配置；入口失败由 ServiceStore 展示，不复用旧 URL。
 */
export function MultiAccountOverview({
  configuration,
  disabled,
  acceptedConnections,
  readState,
  onOpenManagement,
}: MultiAccountOverviewProps) {
  const { t } = useTranslation();
  const [liveConfiguration, setLiveConfiguration] = useState(configuration);

  useEffect(() => {
    setLiveConfiguration(configuration);
  }, [configuration]);

  useEffect(() => {
    if (!configuration.enabled) {
      return;
    }
    const abortController = new AbortController();
    let timer = 0;

    /**
     * 串行读取局部账号快照并在完成后安排下一次读取，避免慢请求产生并发堆积。
     * 组件卸载或功能关闭时 AbortController 会终止在途请求，且不再创建计时器。
     */
    const pollState = async () => {
      const nextState = await readState(abortController.signal);
      if (abortController.signal.aborted) {
        return;
      }
      if (nextState !== null) {
        setLiveConfiguration(nextState);
      }
      timer = window.setTimeout(() => void pollState(), 1_000);
    };

    void pollState();
    return () => {
      abortController.abort();
      window.clearTimeout(timer);
    };
  }, [configuration.enabled, readState]);

  if (!configuration.enabled) {
    return null;
  }

  const summary = liveConfiguration.summary;
  return (
    <section
      className="accountOverview"
      aria-label={t("page.overview.multiAccountTitle")}
    >
      <header className="accountOverviewHeader">
        <div>
          <h2>{t("page.overview.multiAccountTitle")}</h2>
          <p>{t("page.overview.multiAccountDescription")}</p>
        </div>
        <button
          className="primaryButton accountManagementButton"
          disabled={disabled || liveConfiguration.state !== "running"}
          type="button"
          onClick={() => void onOpenManagement()}
        >
          <ExternalLink aria-hidden="true" size={16} />
          {t("page.overview.openMultiAccountManagement")}
        </button>
      </header>
      <div className="metricsGrid accountCombinedMetricsGrid">
        <AccountMetric
          icon={UsersRound}
          label={t("page.overview.onlineAccounts")}
          value={summary === null ? "—" : String(summary.onlineAccounts)}
        />
        <AccountMetric
          icon={Activity}
          label={t("page.overview.activeConnections")}
          value={summary === null ? "—" : String(summary.activeConnections)}
        />
        <AccountMetric
          icon={ArrowUp}
          label={t("page.overview.realtimeUpload")}
          value={formatTransferRate(summary?.uploadBytesPerSecond ?? null)}
        />
        <AccountMetric
          icon={ArrowDown}
          label={t("page.overview.realtimeDownload")}
          value={formatTransferRate(summary?.downloadBytesPerSecond ?? null)}
        />
        <AccountMetric
          icon={Network}
          label={t("page.overview.acceptedConnections")}
          value={
            acceptedConnections === null ? "—" : String(acceptedConnections)
          }
        />
      </div>
    </section>
  );
}
