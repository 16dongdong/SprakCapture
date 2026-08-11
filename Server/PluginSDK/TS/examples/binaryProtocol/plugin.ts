/** 演示两字节大端长度协议的增量修改。 */

import { LengthPrefixedCodec, StreamPipeline, definePlugin, serve, type Frame } from "../../src/index.js";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

export const plugin = definePlugin();
const pipeline = new StreamPipeline(() => new LengthPrefixedCodec(2));

/** 把完整明文帧转为大写；SDK 自动解密/加密并重算长度。 */
function rewriteFrame(frame: Frame): Uint8Array {
  return Uint8Array.from(frame.payload, (value) => value >= 97 && value <= 122 ? value - 32 : value);
}

plugin.tcp(pipeline, rewriteFrame);
export default plugin;

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  serve(plugin).catch((error: unknown) => { console.error(error); process.exitCode = 1; });
}
