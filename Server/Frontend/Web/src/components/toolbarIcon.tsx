import type { CSSProperties, ImgHTMLAttributes } from "react";

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

interface ToolbarIconProps
  extends Omit<
    ImgHTMLAttributes<HTMLImageElement>,
    "alt" | "height" | "src" | "width"
  > {
  name: ToolbarIconName;
  size?: number;
}

// 工具栏位图由桌面安装器复制到固定路径，文件名不会随 Web 构建生成内容哈希。
// 版本查询参数用于隔离旧安装留下的 Chromium 失败缓存；修改任一位图时必须同步推进此版本。
const toolbarIconAssetRevision = "20260814-1";
const toolbarIconAssetSuffix = `?v=${toolbarIconAssetRevision}`;

const iconSources: Record<ToolbarIconName, string> = {
  overviewInactive: `/assets/toolbar/overviewInactive.png${toolbarIconAssetSuffix}`,
  overviewActive: `/assets/toolbar/overviewActive.png${toolbarIconAssetSuffix}`,
  connectionsInactive: `/assets/toolbar/connectionsInactive.png${toolbarIconAssetSuffix}`,
  connectionsActive: `/assets/toolbar/connectionsActive.png${toolbarIconAssetSuffix}`,
  toolsClosed: `/assets/toolbar/toolsClosed.png${toolbarIconAssetSuffix}`,
  toolsOpen: `/assets/toolbar/toolsOpen.png${toolbarIconAssetSuffix}`,
  throttlingOff: `/assets/toolbar/throttlingOff.png${toolbarIconAssetSuffix}`,
  throttlingOn: `/assets/toolbar/throttlingOn.png${toolbarIconAssetSuffix}`,
  breakpointsOff: `/assets/toolbar/breakpointsOff.png${toolbarIconAssetSuffix}`,
  breakpointsOn: `/assets/toolbar/breakpointsOn.png${toolbarIconAssetSuffix}`,
  recordingIdle: `/assets/toolbar/recordingIdle.png${toolbarIconAssetSuffix}`,
  recordingActive: `/assets/toolbar/recordingActive.png${toolbarIconAssetSuffix}`,
  refreshIdle: `/assets/toolbar/refreshIdle.png${toolbarIconAssetSuffix}`,
  refreshBusy: `/assets/toolbar/refreshBusy.png${toolbarIconAssetSuffix}`,
  clearEnabled: `/assets/toolbar/clearEnabled.png${toolbarIconAssetSuffix}`,
  clearDisabled: `/assets/toolbar/clearDisabled.png${toolbarIconAssetSuffix}`,
  settings: `/assets/toolbar/settings.png${toolbarIconAssetSuffix}`,
  reorderOff: `/assets/toolbar/reorderOff.png${toolbarIconAssetSuffix}`,
  reorderOn: `/assets/toolbar/reorderOn.png${toolbarIconAssetSuffix}`,
};

/**
 * 渲染项目自有的单控件、单状态位图图标。
 *
 * 运行上下文：高频操作根据真实开关、忙碌和可用状态选择不同文件，避免用滤镜伪装状态。
 * `name` 必须来自已生成并纳入构建的资源清单；未知名称由 TypeScript 在编译期拒绝，
 * 不在运行时回退到另一张图，以免错误状态误导用户。图标使用真实 img 节点而不是 CSS
 * 背景层，确保远程 Chrome 和桌面 WebView2 都执行同一套图片解码与绘制流程。固定文件名附带
 * 资源修订参数，避免浏览器继续复用旧安装产生的失败缓存。
 * 失败语义：静态资源请求失败时由浏览器呈现空图像并保留 data-toolbar-icon 诊断，不伪造其它状态。
 */
export function ToolbarIcon({
  name,
  size = 18,
  className,
  style,
  ...imageProperties
}: ToolbarIconProps) {
  const iconStyle = {
    width: `${size}px`,
    height: `${size}px`,
    ...style,
  } as CSSProperties;

  return (
    <img
      {...imageProperties}
      alt=""
      aria-hidden="true"
      className={`toolbarBitmapIcon${className ? ` ${className}` : ""}`}
      data-toolbar-icon={name}
      draggable={false}
      height={size}
      src={iconSources[name]}
      style={iconStyle}
      width={size}
    />
  );
}
