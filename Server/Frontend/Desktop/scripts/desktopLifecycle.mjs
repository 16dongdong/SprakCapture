import { spawn } from "node:child_process";
import { copyFile, mkdir, rm, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const desktopDirectory = path.resolve(scriptDirectory, "..");
const workspaceRoot = path.resolve(desktopDirectory, "../../..");
const cargoManifestPath = path.join(workspaceRoot, "Cargo.toml");
const resourceDirectory = path.join(desktopDirectory, "src-tauri", "resources");
const backendPackageName = "proxy-backend";
const backendBinaryName = "proxyService";

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

/** 返回项目本地 Tauri CLI 入口；直接交给当前 Node 运行时执行，避免 Windows 无法直接 spawn `.cmd`。 */
export function resolveTauriCliPath(baseDirectory = desktopDirectory) {
  return path.join(baseDirectory, "node_modules", "@tauri-apps", "cli", "tauri.js");
}

/** 执行继承终端输入输出的命令；非零退出码直接结束当前阶段，不继续启动缺少依赖的桌面程序。 */
async function runCommand(executablePath, commandArguments, commandOptions) {
  await new Promise((resolve, reject) => {
    const childProcess = spawn(executablePath, commandArguments, {
      cwd: commandOptions.cwd,
      env: commandOptions.env,
      stdio: "inherit",
      windowsHide: true,
    });
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

/** 构建后端工作区包并校验产物确为普通文件；路径异常在启动 Tauri 前暴露。 */
async function buildBackend(releaseBuild, environment) {
  const cargoArguments = ["build", "-p", backendPackageName];
  if (releaseBuild) {
    cargoArguments.push("--release");
  }
  await runCommand(cargoExecutable(), cargoArguments, {
    cwd: workspaceRoot,
    env: environment,
  });

  const targetDirectory = await resolveTargetDirectory(environment);
  const backendPath = resolveBackendPath(targetDirectory, releaseBuild);
  const backendStats = await stat(backendPath);
  if (!backendStats.isFile()) {
    throw new Error(`后端构建产物不是普通文件：${backendPath}`);
  }
  return backendPath;
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
  const backendPath = await buildBackend(releaseBuild, environment);
  await withStagedBackend(backendPath, async (resourcePath) => {
    const desktopEnvironment = {
      ...environment,
      PROXY_SERVICE_PATH: releaseBuild ? resourcePath : backendPath,
    };
    await runCommand(
      process.execPath,
      [tauriCliPath, commandName, ...forwardedArguments],
      { cwd: desktopDirectory, env: desktopEnvironment },
    );
  });
}

/** 判断当前模块是否由 Node 直接执行；单元测试导入路径函数时不触发真实构建。 */
function isEntrypoint() {
  return process.argv[1] !== undefined
    && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
}

if (isEntrypoint()) {
  runDesktop(process.argv[2], process.argv.slice(3)).catch((error) => {
    console.error(`Desktop 生命周期执行失败：${error.message}`);
    process.exitCode = 1;
  });
}
