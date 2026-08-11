/** 为真实 Node 子进程测试提供确定性乱序和停止探针。 */

import { writeFile } from "node:fs/promises";
import { continueEvent, definePlugin } from "../src/index.js";

export const plugin = definePlugin();

plugin.on("udpDatagram", async (event) => {
  /** 按 payload 延迟完成，验证作者显式并发时结果按完成顺序返回。 */
  const delayMs = Number(event.payloadObject().delayMs ?? 0);
  await new Promise<void>((resolveDelay) => setTimeout(resolveDelay, delayMs));
  return continueEvent(event);
});

plugin.onStop(async () => {
  /** 写入系统临时探针；测试在子进程退出后验证并由临时目录清理。 */
  const probePath = process.env.PLUGIN_STOP_PROBE;
  if (probePath) await writeFile(probePath, "stopped", "utf8");
});

export default plugin;
