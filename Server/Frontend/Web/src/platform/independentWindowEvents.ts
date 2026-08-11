/** 独立窗口执行成功后通知原页面刷新其局部只读数据。 */
export type IndependentWindowResult =
  | { kind: "onlineValidation"; transactionId: string }
  | { kind: "pluginUninstall"; pluginId: string }
  | { kind: "clearRecording" };

const resultChannelName = "capture.independentWindow.results";

/** 发布独立窗口结果；消息只含稳定标识，不跨窗口传播响应正文或插件配置。 */
export function publishIndependentWindowResult(
  result: IndependentWindowResult,
): void {
  const channel = new BroadcastChannel(resultChannelName);
  channel.postMessage(result);
  channel.close();
}

/**
 * 订阅独立窗口结果并返回确定性清理函数。
 * 失败语义：未知消息形状会被忽略，不会触发无关页面刷新。
 */
export function subscribeIndependentWindowResults(
  listener: (result: IndependentWindowResult) => void,
): () => void {
  const channel = new BroadcastChannel(resultChannelName);
  channel.onmessage = (event: MessageEvent<IndependentWindowResult>) => {
    if (
      event.data?.kind === "onlineValidation" ||
      event.data?.kind === "pluginUninstall" ||
      event.data?.kind === "clearRecording"
    ) {
      listener(event.data);
    }
  };
  return () => channel.close();
}
