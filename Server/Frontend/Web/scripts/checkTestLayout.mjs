import { readdirSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const webRootDirectory = resolve(fileURLToPath(new URL("..", import.meta.url)));
const sourceDirectory = join(webRootDirectory, "src");
const layoutViolations = [];

/**
 * 检查业务源码树，阻止测试文件或测试专用目录重新进入 src。
 *
 * 此检查在 Vitest 执行前运行，失败时列出准确路径并以非零状态退出。
 */
function collectLayoutViolations(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const entryPath = join(directory, entry.name);
    const entryRelativePath = relative(sourceDirectory, entryPath);
    if (entry.isDirectory()) {
      if (entry.name === "test" || entry.name === "tests") {
        layoutViolations.push(`${entryRelativePath}/`);
        continue;
      }
      collectLayoutViolations(entryPath);
      continue;
    }
    if (/\.(?:test|spec)\./u.test(entry.name)) {
      layoutViolations.push(entryRelativePath);
    }
  }
}

collectLayoutViolations(sourceDirectory);

if (layoutViolations.length > 0) {
  console.error("业务源码目录包含测试文件或测试目录：");
  for (const violation of layoutViolations) {
    console.error(`- src/${violation}`);
  }
  process.exitCode = 1;
} else {
  console.log("测试目录分层检查通过：src 未包含测试文件或测试专用目录。");
}
