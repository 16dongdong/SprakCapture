/**
 * 将示例清单复制到编译输出目录。
 *
 * 运行上下文：TypeScript 编译完成后由 npm build 调用。tsc 只生成 JavaScript，
 * Host 又要求清单与相对入口位于同一插件目录，因此这里必须同步非代码资产。
 * 复制失败会让构建直接失败，避免发布一个表面成功但无法加载的示例。
 */
import { copyFile, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const sourceManifest = resolve(packageDirectory, "examples/binaryProtocol/plugin.json");
const outputDirectory = resolve(packageDirectory, "dist/examples/binaryProtocol");

await mkdir(outputDirectory, { recursive: true });
await copyFile(sourceManifest, resolve(outputDirectory, "plugin.json"));
