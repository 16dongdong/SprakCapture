import { Circle, Wifi, WifiOff } from "lucide-react";
import { useTranslation } from "react-i18next";

import { useServiceStore } from "../state/serviceStore";
import { presentServiceState } from "./presentation";

/**
 * 渲染底部连接与录制状态；事务数和丢弃数来自同一权威 recording 快照，避免跨事件混合统计。
 */
export function ConnectionStatusBar() {
  const { t } = useTranslation();
  const {
    snapshot,
    controlConnection,
    eventConnection,
    connectionMessage,
    lastError,
  } = useServiceStore();
  const connected =
    controlConnection === "connected" && eventConnection === "connected";
  const serviceText =
    snapshot === null
      ? t("app.service.unknown")
      : `${t("app.service.name")} ${presentServiceState(snapshot.serviceState).label}`;
  const serviceDetail = lastError || serviceText;
  const recordingState =
    snapshot === null
      ? t("recording.unavailable")
      : t(`recording.${snapshot.recording.state}`);

  return (
    <footer className="connectionStatusBar">
      <span
        className={connected ? "statusOnline" : "statusOffline"}
        title={connectionMessage}
      >
        {connected ? (
          <Wifi aria-hidden="true" size={13} />
        ) : (
          <WifiOff aria-hidden="true" size={13} />
        )}
        {connectionMessage}
      </span>
      <span
        className={`recordingStatus${
          snapshot?.recording.state === "recording"
            ? " isRecording"
            : ""
        }`}
      >
        <Circle aria-hidden="true" fill="currentColor" size={8} />
        <span>{recordingState}</span>
        {snapshot !== null && (
          <span>
            {t("recording.statusSummary", {
              count: snapshot.recording.transactionCount,
              dropped: snapshot.recording.droppedCount,
            })}
          </span>
        )}
      </span>
      <span className="statusBarService" title={serviceDetail}>
        {serviceDetail}
      </span>
    </footer>
  );
}
