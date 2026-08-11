import assert from "node:assert/strict";
import {
  access,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

import {
  resolveBackendPath,
  resolveTargetDirectory,
  resolveTauriCliPath,
  withStagedBackend,
} from "./desktopLifecycle.mjs";

/** 验证 Windows 开发构建产物路径包含 debug 配置与 `.exe` 后缀。 */
test("解析 Windows 开发构建产物", () => {
  const targetDirectory = path.resolve("D:\\workspace\\target");
  assert.equal(
    resolveBackendPath(targetDirectory, false, "win32"),
    path.join(targetDirectory, "debug", "proxyService.exe"),
  );
});

/** 验证 Windows 发布构建产物路径包含 release 配置与固定资源文件名。 */
test("解析 Windows 发布构建产物", () => {
  const targetDirectory = path.resolve("D:\\workspace\\target");
  assert.equal(
    resolveBackendPath(targetDirectory, true, "win32"),
    path.join(targetDirectory, "release", "proxyService.exe"),
  );
});

/** 验证相对 `CARGO_TARGET_DIR` 按工作区根目录解析并转换为绝对路径。 */
test("解析 Cargo 自定义目标目录", async () => {
  const targetDirectory = await resolveTargetDirectory({
    CARGO_TARGET_DIR: "build-output",
  });
  assert.equal(path.basename(targetDirectory), "build-output");
  assert.equal(path.isAbsolute(targetDirectory), true);
});

/** 验证 Tauri 入口固定来自 Desktop 本地依赖，防止误用机器上的全局 CLI 版本。 */
test("解析项目本地 Tauri CLI", () => {
  const desktopDirectory = path.resolve("D:\\workspace\\Desktop");
  assert.equal(
    resolveTauriCliPath(desktopDirectory),
    path.join(desktopDirectory, "node_modules", "@tauri-apps", "cli", "tauri.js"),
  );
});

/** 验证 Tauri 悬浮窗直接加载 BrowserRouter 路径，禁止重新引入会被忽略的 hash 路由。 */
test("悬浮窗配置使用 BrowserRouter 路径", async () => {
  const configText = await readFile(
    new URL("../src-tauri/tauri.conf.json", import.meta.url),
    "utf8",
  );
  const config = JSON.parse(configText);
  const floatingWindow = config.app.windows.find(
    (windowConfig) => windowConfig.label === "floating",
  );
  assert.equal(floatingWindow?.url, "/floating");
  assert.equal(floatingWindow.url.includes("#"), false);
});

/** 验证 Tauri 阶段失败时仍清除暂存后端，避免可执行构建产物残留在源码目录。 */
test("失败后清理暂存后端", async () => {
  // 修复测试目录泄漏：此用例只验证临时资源清理，不应在工作区创建会触发发布前检查的 tmp 目录。
  const testDirectory = await mkdtemp(path.join(tmpdir(), "capture-desktopLifecycle-"));
  const backendPath = path.join(testDirectory, "builtProxyService.exe");
  const stagingDirectory = path.join(testDirectory, "resources");
  const stagedBackendPath = path.join(stagingDirectory, "proxyService.exe");
  await writeFile(backendPath, "测试后端产物", "utf8");

  try {
    await assert.rejects(
      withStagedBackend(
        backendPath,
        async () => {
          await access(stagedBackendPath);
          throw new Error("模拟 Tauri 构建失败");
        },
        stagingDirectory,
      ),
      /模拟 Tauri 构建失败/u,
    );
    await assert.rejects(access(stagedBackendPath));
  } finally {
    await rm(testDirectory, { recursive: true, force: true });
  }
});
