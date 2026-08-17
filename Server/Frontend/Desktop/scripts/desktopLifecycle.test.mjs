import assert from "node:assert/strict";
import {
  access,
  mkdir,
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
  resolveAccountServicePath,
  resolveClientPackagerPath,
  resolveGradleExecutable,
  resolveTargetDirectory,
  resolveTauriCliPath,
  withStagedBackend,
  withStagedServices,
  withStagedValidationResources,
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

/** 验证桌面发布优先使用显式持久 Gradle，避免客户端模板构建重复下载 Wrapper 发行版。 */
test("解析持久 Gradle 可执行文件", async () => {
  const testDirectory = await mkdtemp(path.join(tmpdir(), "capture-gradleExecutable-"));
  const executablePath = path.join(testDirectory, "gradle.bat");
  await writeFile(executablePath, "@echo off\r\n", "utf8");
  try {
    assert.equal(
      await resolveGradleExecutable(
        { CAPTURE_GRADLE_EXECUTABLE: executablePath },
        "win32",
      ),
      executablePath,
    );
  } finally {
    await rm(testDirectory, { recursive: true, force: true });
  }
});

/** 验证账号服务与代理服务使用同一个发布 profile 和安装资源目录。 */
test("解析 Windows 账号服务发布产物", () => {
  const targetDirectory = path.resolve("D:\\workspace\\target");
  assert.equal(
    resolveAccountServicePath(targetDirectory, true, "win32"),
    path.join(targetDirectory, "release", "accountService.exe"),
  );
});

/** 验证独立客户端打包器与服务使用同一发布 profile，安装时无需额外工具链。 */
test("解析 Windows 客户端打包器发布产物", () => {
  const targetDirectory = path.resolve("D:\\workspace\\target");
  assert.equal(
    resolveClientPackagerPath(targetDirectory, true, "win32"),
    path.join(targetDirectory, "release", "clientPackager.exe"),
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

/** 验证安装清单显式收录两个服务、独立打包器和预编译模板，不再分发 Client 源码。 */
test("安装资源包含服务与预编译客户端资源", async () => {
  const configText = await readFile(
    new URL("../src-tauri/tauri.conf.json", import.meta.url),
    "utf8",
  );
  const config = JSON.parse(configText);

  assert.equal(
    config.bundle.resources["resources/proxyService.exe"],
    "proxyService.exe",
  );
  assert.equal(
    config.bundle.resources["resources/accountService.exe"],
    "accountService.exe",
  );
  assert.equal(
    config.bundle.resources["resources/desktopResourceManifest.json"],
    "desktopResourceManifest.json",
  );
  assert.equal(
    config.bundle.resources["resources/clientPackager.exe"],
    "clientPackager.exe",
  );
  assert.equal(
    config.bundle.resources["resources/clientTemplate.apk"],
    "clientTemplate.apk",
  );
  assert.equal(config.bundle.resources["resources/clientProject/**/*"], undefined);
  assert.equal(config.bundle.windows.nsis.installerHooks, "./windows/installerHooks.nsh");
});

/** 验证任一桌面构建结果都会清理服务、Web、打包器与模板，资源目录不残留构建输入。 */
test("失败后清理全部暂存服务", async () => {
  const testDirectory = await mkdtemp(path.join(tmpdir(), "capture-desktopServices-"));
  const backendPath = path.join(testDirectory, "builtProxyService.exe");
  const accountServicePath = path.join(testDirectory, "builtAccountService.exe");
  const clientPackagerPath = path.join(testDirectory, "builtClientPackager.exe");
  const clientTemplatePath = path.join(testDirectory, "builtClientTemplate.apk");
  const stagingDirectory = path.join(testDirectory, "resources");
  const webAssetsDirectory = path.join(testDirectory, "web");
  const stagedBackendPath = path.join(stagingDirectory, "proxyService.exe");
  const stagedAccountServicePath = path.join(stagingDirectory, "accountService.exe");
  const stagedWebPath = path.join(stagingDirectory, "index.html");
  const stagedClientPackagerPath = path.join(stagingDirectory, "clientPackager.exe");
  const stagedClientTemplatePath = path.join(stagingDirectory, "clientTemplate.apk");
  await mkdir(webAssetsDirectory);
  await Promise.all([
    writeFile(backendPath, "测试代理产物", "utf8"),
    writeFile(accountServicePath, "测试账号服务产物", "utf8"),
    writeFile(clientPackagerPath, "测试客户端打包器", "utf8"),
    writeFile(clientTemplatePath, "测试预编译模板", "utf8"),
    writeFile(path.join(webAssetsDirectory, "index.html"), "测试 Web 产物", "utf8"),
  ]);

  try {
    await assert.rejects(
      withStagedServices(
        {
          backendPath,
          accountServicePath,
          clientPackagerPath,
          clientTemplatePath,
          webAssetsDirectory,
        },
        async () => {
          await Promise.all([
            access(stagedBackendPath),
            access(stagedAccountServicePath),
            access(stagedWebPath),
            access(stagedClientPackagerPath),
            access(stagedClientTemplatePath),
          ]);
          throw new Error("模拟双服务打包失败");
        },
        stagingDirectory,
      ),
      /模拟双服务打包失败/u,
    );
    await Promise.all([
      assert.rejects(access(stagedBackendPath)),
      assert.rejects(access(stagedAccountServicePath)),
      assert.rejects(access(stagedWebPath)),
      assert.rejects(access(stagedClientPackagerPath)),
      assert.rejects(access(stagedClientTemplatePath)),
    ]);
  } finally {
    await rm(testDirectory, { recursive: true, force: true });
  }
});

/** 验证桌面编译期标记和 Web 资源只在 Cargo 验证回调期间存在，失败后不会污染安装资源。 */
test("失败后清理桌面验证资源", async () => {
  const testDirectory = await mkdtemp(path.join(tmpdir(), "capture-desktopValidation-"));
  const stagingDirectory = path.join(testDirectory, "resources");
  const webAssetsDirectory = path.join(testDirectory, "web");
  const stagedBackendPath = path.join(stagingDirectory, "proxyService.exe");
  const stagedAccountServicePath = path.join(stagingDirectory, "accountService.exe");
  const stagedClientPackagerPath = path.join(stagingDirectory, "clientPackager.exe");
  const stagedClientTemplatePath = path.join(stagingDirectory, "clientTemplate.apk");
  const stagedWebPath = path.join(stagingDirectory, "index.html");
  await mkdir(webAssetsDirectory);
  await writeFile(path.join(webAssetsDirectory, "index.html"), "测试 Web 产物", "utf8");

  try {
    await assert.rejects(
      withStagedValidationResources(
        webAssetsDirectory,
        async () => {
          await Promise.all([
            access(stagedBackendPath),
            access(stagedAccountServicePath),
            access(stagedClientPackagerPath),
            access(stagedClientTemplatePath),
            access(stagedWebPath),
          ]);
          throw new Error("模拟桌面验证失败");
        },
        stagingDirectory,
      ),
      /模拟桌面验证失败/u,
    );
    await Promise.all([
      assert.rejects(access(stagedBackendPath)),
      assert.rejects(access(stagedAccountServicePath)),
      assert.rejects(access(stagedClientPackagerPath)),
      assert.rejects(access(stagedClientTemplatePath)),
      assert.rejects(access(stagedWebPath)),
    ]);
  } finally {
    await rm(testDirectory, { recursive: true, force: true });
  }
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
