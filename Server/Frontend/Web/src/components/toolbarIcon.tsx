import type { CSSProperties, HTMLAttributes } from "react";

import "./toolbarIcon.css";

export type ToolbarIconName =
  | "overviewInactive"
  | "overviewActive"
  | "connectionsInactive"
  | "connectionsActive"
  | "toolsClosed"
  | "toolsOpen"
  | "throttlingOff"
  | "throttlingOn"
  | "breakpointsOff"
  | "breakpointsOn"
  | "recordingIdle"
  | "recordingActive"
  | "refreshIdle"
  | "refreshBusy"
  | "clearEnabled"
  | "clearDisabled"
  | "settings"
  | "reorderOff"
  | "reorderOn";

interface ToolbarIconProps extends HTMLAttributes<HTMLSpanElement> {
  name: ToolbarIconName;
  size?: number;
}

const iconSources: Record<ToolbarIconName, string> = {
  overviewInactive: "/assets/toolbar/overviewInactive.png",
  overviewActive: "/assets/toolbar/overviewActive.png",
  connectionsInactive: "/assets/toolbar/connectionsInactive.png",
  connectionsActive: "/assets/toolbar/connectionsActive.png",
  toolsClosed: "/assets/toolbar/toolsClosed.png",
  toolsOpen: "/assets/toolbar/toolsOpen.png",
  throttlingOff: "/assets/toolbar/throttlingOff.png",
  throttlingOn: "/assets/toolbar/throttlingOn.png",
  breakpointsOff: "/assets/toolbar/breakpointsOff.png",
  breakpointsOn: "/assets/toolbar/breakpointsOn.png",
  recordingIdle: "/assets/toolbar/recordingIdle.png",
  recordingActive: "/assets/toolbar/recordingActive.png",
  refreshIdle: "/assets/toolbar/refreshIdle.png",
  refreshBusy: "/assets/toolbar/refreshBusy.png",
  clearEnabled: "/assets/toolbar/clearEnabled.png",
  clearDisabled: "/assets/toolbar/clearDisabled.png",
  settings: "/assets/toolbar/settings.png",
  reorderOff: "/assets/toolbar/reorderOff.png",
  reorderOn: "/assets/toolbar/reorderOn.png",
};

/**
 * 渲染项目自有的单控件、单状态位图图标。
 *
 * 运行上下文：高频操作根据真实开关、忙碌和可用状态选择不同文件，避免用滤镜伪装状态。
 * `name` 必须来自已生成并纳入构建的资源清单；未知名称由 TypeScript 在编译期拒绝，
 * 不在运行时回退到另一张图，以免错误状态误导用户。
 */
export function ToolbarIcon({
  name,
  size = 18,
  className,
  style,
  ...spanProps
}: ToolbarIconProps) {
  const iconStyle = {
    "--toolbar-icon-size": `${size}px`,
    "--toolbar-icon-source": `url("${iconSources[name]}")`,
    ...style,
  } as CSSProperties;

  return (
    <span
      {...spanProps}
      aria-hidden="true"
      className={`toolbarBitmapIcon${className ? ` ${className}` : ""}`}
      data-toolbar-icon={name}
      style={iconStyle}
    />
  );
}
