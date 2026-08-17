import { spawn } from "node:child_process";
import { copyFile, cp, mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const desktopDirectory = path.resolve(scriptDirectory, "..");
const workspaceRoot = path.resolve(desktopDirectory, "../..");
const cargoManifestPath = path.join(workspaceRoot, "Cargo.toml");
const resourceDirectory = path.join(desktopDirectory, "src-tauri", "resources");
const backendPackageName = "proxy-backend";
const accountServicePackageName = "account-service";
const clientPackagerPackageName = "client-packager";
const backendBinaryName = "proxyService";
const accountServiceBinaryName = "accountService";
const clientPackagerBinaryName = "clientPackager";
const webDirectory = path.resolve(desktopDirectory, "../Web");
const webDistributionDirectory = path.join(webDirectory, "dist");
const clientProjectDirectory = path.resolve(workspaceRoot, "../Client");
const templateApplicationId = "a00000000.b00000000.c00000000.d00000000";
const templateApplicationName = "A000000000000000";

/** 解析 Cargo 目标目录；显式环境变量按 Cargo 命令的工作目录解释，未设置时读取 metadata 的权威路径。 */
export async function resolveTargetDirectory(environment = process.env) {
  if (environment.CARGO_TARGET_DIR) {
    return path.resolve(workspaceRoot, environment.CARGO_TARGET_DIR);
  }

  const metadataOutput = await captureCommand(
    cargoExecutable(),
    ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", cargoManifestPath],
    { cwd: workspaceRoot, env: environment },
  );
  const metadata = JSON.parse(metadataOutput);
  if (typeof metadata.target_directory !== "string" || metadata.target_directory.length === 0) {
    throw new Error("Cargo metadata 未返回有效 target_directory");
  }
  return path.resolve(metadata.target_directory);
}

/** 根据目标目录、构建模式和平台生成后端产物绝对路径，确保开发与打包使用同一个命名规则。 */
export function resolveBackendPath(targetDirectory, releaseBuild, platform = process.platform) {
  const executableSuffix = platform === "win32" ? ".exe" : "";
  const buildProfile = releaseBuild ? "release" : "debug";
  return path.resolve(targetDirectory, buildProfile, `${backendBinaryName}${executableSuffix}`);
}

/** 解析账号服务产物路径；它必须与 proxyService 位于同一构建 profile，监督器才能按同目录启动。 */
export function resolveAccountServicePath(targetDirectory, releaseBuild, platform = process.platform) {
  const executableSuffix = platform === "win32" ? ".exe" : "";
  const buildProfile = releaseBuild ? "release" : "debug";
  return path.resolve(targetDirectory, buildProfile, `${accountServiceBinaryName}${executableSuffix}`);
}

/** 解析独立客户端打包器产物；它与两个服务使用同一 Cargo profile，但运行时保持独立进程边界。 */
export function resolveClientPackagerPath(targetDirectory, releaseBuild, platform = process.platform) {
  const executableSuffix = platform === "win32" ? ".exe" : "";
  const buildProfile = releaseBuild ? "release" : "debug";
  return path.resolve(targetDirectory, buildProfile, `${clientPackagerBinaryName}${executableSuffix}`);
}

/** 返回项目本地 Tauri CLI 入口；直接交给当前 Node 运行时执行，避免 Windows 无法直接 spawn `.cmd`。 */
export function resolveTauriCliPath(baseDirectory = desktopDirectory) {
  return path.join(baseDirectory, "node_modules", "@tauri-apps", "cli", "tauri.js");
}

/**
 * 解析发布机器的 Gradle 可执行文件；优先使用显式配置和 D 盘持久工具，最后才使用项目 Wrapper。
 *
 * 运行上下文：仅桌面发布阶段编译 Android 模板。持久 Gradle 可以复用已经安装的发行版，避免 Wrapper
 * 每次重新下载；候选均不可用时返回明确错误，不会静默切换到 C 盘临时缓存。
 */
export async function resolveGradleExecutable(
  environment = process.env,
  platform = process.platform,
) {
  const executableName = platform === "win32" ? "gradle.bat" : "gradle";
  const wrapperName = platform === "win32" ? "gradlew.bat" : "gradlew";
  const candidates = [
    environment.CAPTURE_GRADLE_EXECUTABLE,
    platform === "win32"
      ? path.join("D:\\DevTools\\Gradle\\gradle-8.14.5", "bin", executableName)
      : undefined,
    path.join(clientProjectDirectory, wrapperName),
  ].filter(Boolean);
  for (const candidate of candidates) {
    const executablePath = path.resolve(candidate);
    const executableStats = await stat(executablePath).catch(() => undefined);
    if (executableStats?.isFile()) return executablePath;
  }
  throw new Error("发布机器缺少可用 Gradle；请设置 CAPTURE_GRADLE_EXECUTABLE");
}

/** 执行继承终端输入输出的命令；非零退出码直接结束当前阶段，不继续启动缺少依赖的桌面程序。 */
async function runCommand(executablePath, commandArguments, commandOptions) {
  await new Promise((resolve, reject) => {
    const commandInvocation = resolveCommandInvocation(
      executablePath,
      commandArguments,
    );
    const childProcess = spawn(
      commandInvocation.executablePath,
      commandInvocation.commandArguments,
      {
        cwd: commandOptions.cwd,
        env: commandOptions.env,
        stdio: "inherit",
        windowsHide: true,
      },
    );
    const forwardInterrupt = () => childProcess.kill("SIGINT");
    const forwardTermination = () => childProcess.kill("SIGTERM");

    /** 移除仅属于当前子进程的信号监听器，避免多阶段构建累积重复转发。 */
    const removeSignalListeners = () => {
      process.off("SIGINT", forwardInterrupt);
      process.off("SIGTERM", forwardTermination);
    };

    process.once("SIGINT", forwardInterrupt);
    process.once("SIGTERM", forwardTermination);
    childProcess.once("error", (error) => {
      removeSignalListeners();
      reject(error);
    });
    childProcess.once("exit", (exitCode, signalName) => {
      removeSignalListeners();
      if (exitCode === 0) {
        resolve();
        return;
      }
      const exitReason = signalName ? `信号 ${signalName}` : `退出码 ${exitCode}`;
      reject(new Error(`命令 ${executablePath} 执行失败：${exitReason}`));
    });
  });
}

/** 将 Windows `.cmd` 包装器交给 cmd.exe，绕过 Node 24 直接 spawn 命令脚本时的 EINVAL。 */
function resolveCommandInvocation(executablePath, commandArguments) {
  if (
    process.platform !== "win32" ||
    ![".cmd", ".bat"].includes(path.extname(executablePath).toLowerCase())
  ) {
    return { executablePath, commandArguments };
  }
  return {
    executablePath: process.env.ComSpec ?? "cmd.exe",
    commandArguments: ["/d", "/s", "/c", executablePath, ...commandArguments],
  };
}

/** 执行只读取标准输出的命令；metadata 诊断保留在标准错误，JSON 只从标准输出解析。 */
async function captureCommand(executablePath, commandArguments, commandOptions) {
  return await new Promise((resolve, reject) => {
    const outputChunks = [];
    const childProcess = spawn(executablePath, commandArguments, {
      cwd: commandOptions.cwd,
      env: commandOptions.env,
      stdio: ["ignore", "pipe", "inherit"],
      windowsHide: true,
    });
    childProcess.stdout.on("data", (chunk) => outputChunks.push(chunk));
    childProcess.once("error", reject);
    childProcess.once("exit", (exitCode, signalName) => {
      if (exitCode === 0) {
        resolve(Buffer.concat(outputChunks).toString("utf8"));
        return;
      }
      const exitReason = signalName ? `信号 ${signalName}` : `退出码 ${exitCode}`;
      reject(new Error(`命令 ${executablePath} 执行失败：${exitReason}`));
    });
  });
}

/** 返回当前平台的 Cargo 启动文件名；Windows 通过可执行包装器启动，其他平台直接使用 PATH 中的 cargo。 */
function cargoExecutable(platform = process.platform) {
  return platform === "win32" ? "cargo.exe" : "cargo";
}

/**
 * 解析发布机器的 Android SDK；仅桌面发布阶段需要，安装后的目标机器不会读取该路径。
 *
 * 运行上下文：优先使用显式环境，其次检查 Android Studio 默认目录和当前工作站统一工具目录。
 * 候选必须同时包含 platforms 与 build-tools；全部缺失时返回明确错误，不生成 local.properties。
 */
async function resolveAndroidSdkDirectory(environment) {
  const candidates = [
    environment.ANDROID_HOME,
    environment.ANDROID_SDK_ROOT,
    environment.LOCALAPPDATA && path.join(environment.LOCALAPPDATA, "Android", "Sdk"),
    process.platform === "win32" ? "D:\\Android" : undefined,
  ].filter(Boolean);
  for (const candidate of candidates) {
    const sdkDirectory = path.resolve(candidate);
    const [platforms, buildTools] = await Promise.all([
      stat(path.join(sdkDirectory, "platforms")).catch(() => undefined),
      stat(path.join(sdkDirectory, "build-tools")).catch(() => undefined),
    ]);
    if (platforms?.isDirectory() && buildTools?.isDirectory()) return sdkDirectory;
  }
  throw new Error("发布机器缺少 Android SDK；请设置 ANDROID_HOME 或 ANDROID_SDK_ROOT");
}

/** 构建两个服务与独立打包器并校验产物；任一模块缺失都在启动 Tauri 前终止发布。 */
async function buildBackend(releaseBuild, environment) {
  const cargoArguments = [
    "build",
    "-p",
    backendPackageName,
    "-p",
    accountServicePackageName,
    "-p",
    clientPackagerPackageName,
  ];
  if (releaseBuild) {
    cargoArguments.push("--release");
  }
  await runCommand(cargoExecutable(), cargoArguments, {
    cwd: workspaceRoot,
    env: environment,
  });

  const targetDirectory = await resolveTargetDirectory(environment);
  const backendPath = resolveBackendPath(targetDirectory, releaseBuild);
  const accountServicePath = resolveAccountServicePath(targetDirectory, releaseBuild);
  const clientPackagerPath = resolveClientPackagerPath(targetDirectory, releaseBuild);
  const backendStats = await stat(backendPath);
  if (!backendStats.isFile()) {
    throw new Error(`后端构建产物不是普通文件：${backendPath}`);
  }
  const accountServiceStats = await stat(accountServicePath);
  if (!accountServiceStats.isFile()) {
    throw new Error(`账号服务构建产物不是普通文件：${accountServicePath}`);
  }
  const clientPackagerStats = await stat(clientPackagerPath);
  if (!clientPackagerStats.isFile()) {
    throw new Error(`客户端打包器构建产物不是普通文件：${clientPackagerPath}`);
  }
  return { backendPath, accountServicePath, clientPackagerPath, targetDirectory };
}

/** 统计固定字节槽位出现次数；模板协议要求包名和节点各且仅出现一次。 */
function countOccurrences(contents, placeholder) {
  let count = 0;
  let offset = 0;
  while ((offset = contents.indexOf(placeholder, offset)) !== -1) {
    count += 1;
    offset += placeholder.length;
  }
  return count;
}

/**
 * 在发布机器预编译 Android Client 模板；目标机器只收到 APK 和独立打包器，不收到源码或 Gradle。
 *
 * 运行上下文：Web 与 Rust 服务构建完成后、Tauri 收集资源前执行；`targetDirectory` 是本轮任务专属
 * Cargo 目录，Gradle 项目缓存和 app 输出均放入其子目录。Gradle 失败、产物缺失或定长槽位漂移会
 * 直接终止桌面发布，防止安装包携带运行时不可用的模板。
 */
async function buildClientTemplate(targetDirectory, clientPackagerPath, environment) {
  const clientBuildDirectory = path.join(targetDirectory, "clientTemplateBuild");
  const projectCacheDirectory = path.join(targetDirectory, "clientTemplateGradleCache");
  const gradleExecutable = await resolveGradleExecutable(environment);
  const androidSdkDirectory = await resolveAndroidSdkDirectory(environment);
  const clientBuildEnvironment = {
    ...environment,
    ANDROID_HOME: androidSdkDirectory,
    ANDROID_SDK_ROOT: androidSdkDirectory,
  };
  await runCommand(
    gradleExecutable,
    [
      ":app:assembleRelease",
      "--no-daemon",
      "--no-problems-report",
      "--console=plain",
      "--project-cache-dir",
      projectCacheDirectory,
      `-PclientApplicationId=${templateApplicationId}`,
      `-PclientBuildDirectory=${clientBuildDirectory}`,
    ],
    { cwd: clientProjectDirectory, env: clientBuildEnvironment },
  );
  const compiledApkPath = path.join(clientBuildDirectory, "outputs", "apk", "release", "app-release-unsigned.apk");
  const templatePath = path.join(targetDirectory, "clientTemplate.apk");
  await runCommand(
    clientPackagerPath,
    ["prepare-template", "--source", compiledApkPath, "--output", templatePath],
    { cwd: workspaceRoot, env: environment },
  );
  const templateBytes = await readFile(templatePath);
  if (!templateBytes.subarray(0, 4).equals(Buffer.from("PK\x03\x04", "binary"))) {
    throw new Error(`客户端模板不是 APK ZIP：${templatePath}`);
  }
  for (const [name, placeholder] of [
    ["applicationId", Buffer.from(templateApplicationId, "utf16le")],
    ["软件名", Buffer.from(templateApplicationName, "ascii")],
  ]) {
    const occurrences = countOccurrences(templateBytes, placeholder);
    const validCount = name === "applicationId"
      ? occurrences >= 1 && occurrences <= 16
      : occurrences === 1;
    if (!validCount) {
      throw new Error(`客户端模板中的 ${name} 槽位数量无效：${occurrences}`);
    }
  }
  return templatePath;
}

/** 在 Tauri 运行期间暂存固定资源名，并在正常、失败或中断退出后清理，避免提交构建产物。 */
export async function withStagedBackend(
  backendPath,
  action,
  stagingDirectory = resourceDirectory,
) {
  const stagedBackendPath = path.join(stagingDirectory, "proxyService.exe");
  await mkdir(stagingDirectory, { recursive: true });
  try {
    await copyFile(backendPath, stagedBackendPath);
    return await action(stagedBackendPath);
  } finally {
    await rm(stagedBackendPath, { force: true });
  }
}

/** 暂存两个服务、独立打包器、预编译模板和唯一 Web 构建；结束后清理源码目录内的全部资源副本。 */
export async function withStagedServices(
  serviceArtifacts,
  action,
  stagingDirectory = resourceDirectory,
) {
  const {
    backendPath,
    accountServicePath,
    clientPackagerPath,
    clientTemplatePath,
    webAssetsDirectory,
  } = serviceArtifacts;
  const stagedBackendPath = path.join(stagingDirectory, "proxyService.exe");
  const stagedAccountServicePath = path.join(stagingDirectory, "accountService.exe");
  const stagedClientPackagerPath = path.join(stagingDirectory, "clientPackager.exe");
  const stagedClientTemplatePath = path.join(stagingDirectory, "clientTemplate.apk");
  await mkdir(stagingDirectory, { recursive: true });
  const webDistributionStats = await stat(webAssetsDirectory);
  if (!webDistributionStats.isDirectory()) {
    throw new Error(`Web 构建产物不是目录：${webAssetsDirectory}`);
  }
  const stagedWebFiles = [];
  try {
    await Promise.all([
      copyFile(backendPath, stagedBackendPath),
      copyFile(accountServicePath, stagedAccountServicePath),
      copyFile(clientPackagerPath, stagedClientPackagerPath),
      copyFile(clientTemplatePath, stagedClientTemplatePath),
    ]);
    await cp(webAssetsDirectory, stagingDirectory, { recursive: true });
    for (const entry of await readdir(webAssetsDirectory)) {
      stagedWebFiles.push(path.join(stagingDirectory, entry));
    }
    return await action({
      stagedBackendPath,
      stagedAccountServicePath,
      stagedClientPackagerPath,
      stagedClientTemplatePath,
    });
  } finally {
    await Promise.all([
      rm(stagedBackendPath, { force: true }),
      rm(stagedAccountServicePath, { force: true }),
      rm(stagedClientPackagerPath, { force: true }),
      rm(stagedClientTemplatePath, { force: true }),
      ...stagedWebFiles.map((stagedPath) =>
        rm(stagedPath, { force: true, recursive: true }),
      ),
    ]);
  }
}

/**
 * 为桌面静态检查暂存完整 Web 目录与非空服务标记文件。
 *
 * 运行上下文：Tauri 的构建脚本会在编译 Rust 前校验所有安装资源，但 `cargo check/test`
 * 不需要真实服务二进制。该边界只为编译期资源发现提供临时文件，正式打包仍必须调用
 * `withStagedServices` 放入真实产物。参数分别表示 Web 构建目录、待执行的 Cargo 阶段和
 * 可替换的暂存目录；复制、执行或清理失败时直接抛出，并在 `finally` 中删除全部标记与 Web 资源。
 */
export async function withStagedValidationResources(
  webAssetsDirectory,
  action,
  stagingDirectory = resourceDirectory,
) {
  const validationArtifacts = {
    backendPath: path.join(stagingDirectory, "validationProxyService.exe"),
    accountServicePath: path.join(stagingDirectory, "validationAccountService.exe"),
    clientPackagerPath: path.join(stagingDirectory, "validationClientPackager.exe"),
    clientTemplatePath: path.join(stagingDirectory, "validationClientTemplate.apk"),
    webAssetsDirectory,
  };
  await mkdir(stagingDirectory, { recursive: true });
  await Promise.all([
    writeFile(validationArtifacts.backendPath, "仅用于 Tauri 编译期资源校验", "utf8"),
    writeFile(
      validationArtifacts.accountServicePath,
      "仅用于 Tauri 编译期资源校验",
      "utf8",
    ),
    writeFile(
      validationArtifacts.clientPackagerPath,
      "仅用于 Tauri 编译期资源校验",
      "utf8",
    ),
    writeFile(validationArtifacts.clientTemplatePath, "PK\u0003\u0004", "binary"),
  ]);
  try {
    return await withStagedServices(
      validationArtifacts,
      action,
      stagingDirectory,
    );
  } finally {
    await Promise.all([
      rm(validationArtifacts.backendPath, { force: true }),
      rm(validationArtifacts.accountServicePath, { force: true }),
      rm(validationArtifacts.clientPackagerPath, { force: true }),
      rm(validationArtifacts.clientTemplatePath, { force: true }),
    ]);
  }
}

/**
 * 在完整 Tauri 资源环境中检查或测试桌面 Rust 包。
 *
 * 运行上下文：服务端工作区的通用 Cargo 阶段排除 `desktop-shell`，避免在缺少暂存安装资源时
 * 触发 Tauri 构建脚本。`commandName` 只接受 `check` 或 `test`；额外参数原样传给 Cargo。静态检查覆盖
 * 全部目标，测试阶段只执行清单允许测试的目标，因为 Windows 桌面入口需要管理员权限，非交互测试
 * 只验证承载业务逻辑的库目标。
 * Web 构建、资源暂存或 Cargo 阶段任一失败都会终止命令，退出前始终删除 `dist` 与暂存资源。
 */
async function runDesktopValidation(commandName, forwardedArguments) {
  if (commandName !== "check" && commandName !== "test") {
    throw new Error(`未知 Desktop 验证命令：${commandName}`);
  }
  const environment = { ...process.env };
  await runCommand(process.platform === "win32" ? "pnpm.cmd" : "pnpm", ["build"], {
    cwd: webDirectory,
    env: environment,
  });
  try {
    await withStagedValidationResources(webDistributionDirectory, async () => {
      const cargoArguments = commandName === "check"
        ? ["clippy", "-p", "desktop-shell", "--all-targets", "--all-features", "--", "-D", "warnings"]
        : ["test", "-p", "desktop-shell", "--all-features", ...forwardedArguments];
      await runCommand(cargoExecutable(), cargoArguments, {
        cwd: workspaceRoot,
        env: environment,
      });
    });
  } finally {
    await rm(webDistributionDirectory, { recursive: true, force: true });
  }
}

/** 构建后端并启动 Tauri；开发态注入绝对后端路径，打包态同时通过固定资源映射收录后端。 */
async function runDesktop(commandName, forwardedArguments) {
  const releaseBuild = commandName === "build";
  if (!releaseBuild && commandName !== "dev") {
    throw new Error(`未知 Desktop 生命周期命令：${commandName}`);
  }

  const environment = { ...process.env };
  const tauriCliPath = resolveTauriCliPath();
  const tauriCliStats = await stat(tauriCliPath).catch(() => undefined);
  if (!tauriCliStats?.isFile()) {
    throw new Error(`缺少项目本地 Tauri CLI，请先在工作区根目录执行 pnpm install：${tauriCliPath}`);
  }
  await runCommand(process.platform === "win32" ? "pnpm.cmd" : "pnpm", ["build"], {
    cwd: webDirectory,
    env: environment,
  });
  const {
    backendPath,
    accountServicePath,
    clientPackagerPath,
    targetDirectory,
  } = await buildBackend(releaseBuild, environment);
  const clientTemplatePath = await buildClientTemplate(
    targetDirectory,
    clientPackagerPath,
    environment,
  );
  await withStagedServices(
    {
      backendPath,
      accountServicePath,
      clientPackagerPath,
      clientTemplatePath,
      webAssetsDirectory: webDistributionDirectory,
    },
    async ({ stagedBackendPath, stagedClientPackagerPath, stagedClientTemplatePath }) => {
      const desktopEnvironment = {
        ...environment,
        PROXY_SERVICE_PATH: releaseBuild ? stagedBackendPath : backendPath,
        CAPTURE_WEB_ASSETS_DIR: releaseBuild
          ? resourceDirectory
          : webDistributionDirectory,
        CAPTURE_CLIENT_PACKAGER_EXECUTABLE: releaseBuild
          ? stagedClientPackagerPath
          : clientPackagerPath,
        CAPTURE_CLIENT_TEMPLATE_PATH: releaseBuild
          ? stagedClientTemplatePath
          : clientTemplatePath,
      };
      await runCommand(
        process.execPath,
        [tauriCliPath, commandName, ...forwardedArguments],
        { cwd: desktopDirectory, env: desktopEnvironment },
      );
    },
  );
}

/** 判断当前模块是否由 Node 直接执行；单元测试导入路径函数时不触发真实构建。 */
function isEntrypoint() {
  return process.argv[1] !== undefined
    && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
}

if (isEntrypoint()) {
  const commandName = process.argv[2];
  const commandArguments = process.argv.slice(3);
  const lifecycleOperation = commandName === "check" || commandName === "test"
    ? runDesktopValidation(commandName, commandArguments)
    : runDesktop(commandName, commandArguments);
  lifecycleOperation.catch((error) => {
    console.error(`Desktop 生命周期执行失败：${error.message}`);
    process.exitCode = 1;
  });
}
