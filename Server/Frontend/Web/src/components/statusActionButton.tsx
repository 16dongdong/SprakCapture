import { LoaderCircle, Play, Square } from "lucide-react";
import { useTranslation } from "react-i18next";

import { useServiceStore } from "../state/serviceStore";
import { presentServiceState } from "./presentation";
import { StableLabel } from "./stableLabel";

/** 服务操作按钮的完整状态集合，用于在状态切换前预留各语言的最大标签宽度。 */
const serviceStates = [
  "stopped",
  "starting",
  "running",
  "stopping",
  "faulted",
] as const;

/**
 * 渲染唯一启停动作；按钮文字、图标和可用性完全由权威状态决定。
 */
export function StatusActionButton({
  compact = false,
}: {
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const {
    snapshot,
    controlConnection,
    activeAction,
    toggleService,
  } = useServiceStore();
  const unavailableLabel = compact
    ? t("app.statusAction.controlUnavailableCompact")
    : t("app.statusAction.controlUnavailable");
  const serviceButtonLabels = serviceStates.map((serviceState) =>
    t(`app.service.${serviceState}.${compact ? "label" : "action"}`),
  );

  if (snapshot === null || controlConnection !== "connected") {
    return (
      <button
        className={`statusAction statusAction--neutral${compact ? " statusAction--compact" : ""}`}
        type="button"
        disabled
        title={t("app.statusAction.controlUnavailableTitle")}
      >
        <LoaderCircle aria-hidden="true" size={16} />
        <span>{unavailableLabel}</span>
      </button>
    );
  }

  const presentation = presentServiceState(snapshot.serviceState);
  const disabled =
    activeAction === "service" || presentation.actionKind === "wait";
  const ActionIcon =
    presentation.actionKind === "stop"
      ? Square
      : presentation.actionKind === "start"
        ? Play
        : LoaderCircle;

  return (
    <button
      className={`statusAction statusAction--${presentation.tone}${compact ? " statusAction--compact" : ""}`}
      type="button"
      disabled={disabled}
      onClick={() => void toggleService()}
      title={`${presentation.label} · ${presentation.actionText}`}
    >
      <ActionIcon
        aria-hidden="true"
        className={presentation.actionKind === "wait" ? "isSpinning" : undefined}
        size={16}
      />
      <StableLabel
        candidates={serviceButtonLabels}
        value={compact ? presentation.label : presentation.actionText}
      />
    </button>
  );
}
