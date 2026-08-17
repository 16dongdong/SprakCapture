const startupLoadingElementId = "startupLoading";
const startupLoadingExitClass = "isReady";
const startupLoadingExitState = "leaving";

/**
 * 在 React 首次提交并完成浏览器绘制后淡出启动层；重复调用只复用第一次退出流程。
 *
 * 运行上下文：由根组件的 Effect 调用，确保业务界面已经进入 DOM 后才允许移除首帧占位。
 * 失败语义：HTML 未包含启动层时直接返回；过渡事件负责最终移除节点，避免透明层继续拦截点击。
 */
export function dismissStartupLoading(): void {
  const startupLoading = document.getElementById(startupLoadingElementId);
  if (
    startupLoading === null ||
    startupLoading.dataset.state === startupLoadingExitState
  ) {
    return;
  }

  startupLoading.dataset.state = startupLoadingExitState;
  startupLoading.addEventListener(
    "transitionend",
    () => startupLoading.remove(),
    { once: true },
  );
  startupLoading.classList.add(startupLoadingExitClass);
}
