/** 将 Node 标准输入的 Sidecar JSONL 映射到普通插件函数。 */

import { createInterface } from "node:readline";
import { stdin, stdout } from "node:process";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { Plugin } from "./plugin.js";
import type { RuntimeInvocation } from "./model.js";

type ProtocolMessage = Record<string, unknown>;

export interface RunnerOptions { readonly concurrentInvocations?: boolean; }

class JsonLineWriter {
  private tail = Promise.resolve();

  /** 把输出追加到单一 Promise 队列，保证并发结果不会交错成坏 JSON。 */
  public write(message: ProtocolMessage): Promise<void> {
    const encoded = `${JSON.stringify(message)}\n`;
    const operation = this.tail.then(() => new Promise<void>((resolveWrite, rejectWrite) => {
      stdout.write(encoded, (error) => error ? rejectWrite(error) : resolveWrite());
    }));
    this.tail = operation.catch(() => undefined);
    return operation;
  }
}

/** 严格执行 initialize→invoke*→stop；每个请求以 requestId 精确关联。 */
export async function serve(plugin: Plugin, options: RunnerOptions = {}): Promise<void> {
  let initialized = false;
  let stopped = false;
  const writer = new JsonLineWriter();
  const activeTasks = new Set<Promise<void>>();

  /** 校验当前 Host 从 1 递增的 u64 请求号；超出 JS 安全整数时拒绝而不改写精度。 */
  function parseRequestId(message: ProtocolMessage): number {
    const requestId = message.requestId;
    if (typeof requestId !== "number" || !Number.isSafeInteger(requestId) || requestId < 0) {
      throw new Error("requestId 必须是非负 JavaScript 安全整数")
    }
    return requestId;
  }

  /** 执行一个作者函数并回显请求身份；所有插件异常转换为 error 帧。 */
  async function invoke(message: ProtocolMessage): Promise<void> {
    const requestId = parseRequestId(message);
    try {
      const action = await plugin.invoke(message.invocation as RuntimeInvocation);
      await writer.write({ type: "result", requestId, action });
    } catch (error) {
      await writer.write({ type: "error", requestId, message: error instanceof Error ? `${error.name}: ${error.message}` : String(error) });
    }
  }

  /** 等待所有在途作者任务，再且仅执行一次停止生命周期。 */
  async function waitAndStop(): Promise<void> {
    if (stopped) return;
    stopped = true;
    await Promise.allSettled([...activeTasks]);
    await plugin.stop();
  }

  const lines = createInterface({ input: stdin, crlfDelay: Infinity });
  for await (const line of lines) {
    if (!line.trim()) continue;
    let message: ProtocolMessage = {};
    try {
      message = JSON.parse(line) as ProtocolMessage;
      if (message.type === "initialize") {
        if (initialized || message.apiVersion !== 2) throw new Error("初始化顺序或 API 版本无效");
        plugin.manifest = message.manifest as Record<string, unknown>;
        plugin.configuration = message.configuration as Record<string, unknown>;
        initialized = true;
        await writer.write({ type: "ready", apiVersion: 2 });
      } else if (message.type === "invoke") {
        if (!initialized) throw new Error("插件尚未初始化");
        parseRequestId(message);
        if (options.concurrentInvocations) {
          const task = invoke(message);
          activeTasks.add(task);
          void task.finally(() => activeTasks.delete(task)).catch((error: unknown) => console.error(error));
        } else {
          await invoke(message);
        }
      } else if (message.type === "stop") {
        await waitAndStop();
        return;
      } else {
        throw new Error("未知 Sidecar 消息");
      }
    } catch (error) {
      const requestId = message.requestId;
      if (typeof requestId !== "number" || !Number.isSafeInteger(requestId) || requestId < 0) {
        console.error(`Sidecar 协议错误：${error instanceof Error ? `${error.name}: ${error.message}` : String(error)}`);
        await waitAndStop();
        return;
      }
      await writer.write({ type: "error", requestId, message: error instanceof Error ? `${error.name}: ${error.message}` : String(error) });
    }
  }
  await waitAndStop();
}

/** 从模块默认导出或 plugin 命名导出载入 Plugin；入口错误时在读循环前失败。 */
async function loadPlugin(entry: string): Promise<Plugin> {
  const module = await import(pathToFileURL(resolve(entry)).href) as { default?: unknown; plugin?: unknown };
  const plugin = module.default ?? module.plugin;
  if (!(plugin instanceof Plugin)) throw new Error("入口必须导出 Plugin 实例");
  return plugin;
}

/** 作为独立 runner 调用时加载作者模块；生产入口也可直接调用 serve(plugin)。 */
async function main(): Promise<void> {
  const argumentsWithoutFlags = process.argv.slice(2).filter((argument) => argument !== "--concurrent");
  const entry = argumentsWithoutFlags[0];
  if (!entry) throw new Error("用法：node runner.js plugin.js");
  await serve(await loadPlugin(entry), { concurrentInvocations: process.argv.includes("--concurrent") });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error: unknown) => { console.error(error); process.exitCode = 1; });
}
