import { readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const workspaceDirectory = resolve(scriptDirectory, "..");
const transientDirectories = [
  ".playwright-cli",
  "tmp",
  "output",
  "Server/Frontend/Web/dist",
  "Server/Frontend/Web/coverage",
];
const allowedTargetDirectories = new Set(["debug", "release"]);

/**
 * 读取目录项；目录不存在表示该类临时产物已经被正确清理。
 *
 * 运行上下文：收尾校验需要同时覆盖首次执行和已产生构建产物的工作区。
 * 参数：directoryPath 为待检查目录的绝对路径。
 * 失败语义：非不存在错误继续抛出，避免权限问题被误判为清理完成。
 */
async function readDirectory(directoryPath) {
  try {
    return await readdir(directoryPath, { withFileTypes: true });
  } catch (error) {
    if (error && typeof error === "object" && error.code === "ENOENT") {
      return null;
    }
    throw error;
  }
}

/**
 * 收集仍然存在的临时目录，确保忽略规则不会掩盖物理残留。
 *
 * 运行上下文：浏览器、前端构建和测试夹具都必须在任务收尾前移除。
 * 参数：无。
 * 失败语义：目录读取失败由调用方终止校验，不输出错误的通过结果。
 */
async function findTransientDirectories() {
  const remainingDirectories = [];
  for (const directoryPath of transientDirectories) {
    const entries = await readDirectory(resolve(workspaceDirectory, directoryPath));
    if (entries !== null) {
      remainingDirectories.push(directoryPath);
    }
  }
  return remainingDirectories;
}

/**
 * 检查 Rust 默认构建目录是否混入一次性测试目标目录。
 *
 * 运行上下文：`debug` 与 `release` 是当前开发或发布构建的唯一允许顶层目录。
 * 参数：无。
 * 失败语义：任何其它顶层目录都会被报告，阻止验收目录和端到端缓存长期占用工作区。
 */
async function findUnexpectedTargetDirectories() {
  const targetEntries = await readDirectory(resolve(workspaceDirectory, "target"));
  if (targetEntries === null) {
    return [];
  }
  return targetEntries
    .filter(
      (entry) => entry.isDirectory() && !allowedTargetDirectories.has(entry.name),
    )
    .map((entry) => `target/${entry.name}`);
}

const remainingDirectories = [
  ...(await findTransientDirectories()),
  ...(await findUnexpectedTargetDirectories()),
];

if (remainingDirectories.length > 0) {
  console.error("工作区清理检查失败，以下目录必须在任务完成前删除：");
  for (const directoryPath of remainingDirectories) {
    console.error(`- ${directoryPath}`);
  }
  process.exitCode = 1;
} else {
  console.log("工作区清理检查通过：未保留一次性构建或测试目录。");
}
