/** 验证 TypeScript SDK 的动作、流式协议与 JSONL 运行器。 */

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { access, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { Event, Frame, LengthPrefixedCodec, ManifestBuilder, Simulator, StreamPipeline, createInvocation, definePlugin, modifyBytes, modifyPayload, redirect, reject } from "../src/index.js";

test("普通阶段函数直接构造修改动作", async () => {
  /** 注册 UDP 函数并验证 Host 期望的完整动作结构。 */
  const plugin = definePlugin();
  plugin.on("udpDatagram", (event) => modifyBytes(event, event.bytes().reverse()));
  const action = await new Simulator(plugin).invoke(createInvocation("event-1", "udpDatagram", { bytes: [1, 2, 3] }));
  assert.equal(action.action, "modify");
  assert.deepEqual(action.patch[0]?.value, { bytes: [3, 2, 1] });
});

test("终止动作拒绝纯空白参数", () => {
  /** 保持五种 SDK 的 reason/host 校验一致，避免同一作者输入产生不同动作。 */
  const event = new Event(createInvocation("event-validation", "beforeConnect", { bytes: [] }));
  assert.throws(() => reject(event, " \t\r\n"), /拒绝原因/);
  assert.throws(() => redirect(event, " \t", 443), /重定向主机/);
});

test("任意 JSON payload 保持原始类型", () => {
  /** 标量和数组可用于命令等阶段，只有 bytes 便捷函数要求对象 payload。 */
  const event = new Event(createInvocation("event-scalar", "commandInvoked", 7));
  assert.equal(event.payload, 7);
  assert.deepEqual(modifyPayload(event, ["ok"]).patch[0]?.value, ["ok"]);
  assert.throws(() => modifyBytes(event, Uint8Array.of(1)), /payload 必须是对象/);
});

test("TCP 管线保留半帧并重封包", async () => {
  /** 分两次推送一帧，确保第一块 hold 且第二块只发布完整线帧。 */
  const plugin = definePlugin();
  plugin.tcp(new StreamPipeline(() => new LengthPrefixedCodec(2)), (frame) => ({ ...frame, payload: Uint8Array.from(frame.payload, (value) => value - 32) }));
  const first = await plugin.invoke(createInvocation("event-1", "tcpChunk", { bytes: [0, 3, 97] }));
  const second = await plugin.invoke(createInvocation("event-2", "tcpChunk", { bytes: [98, 99] }));
  assert.equal(first.action, "hold");
  assert.deepEqual(second.patch[0]?.value, { bytes: [0, 3, 65, 66, 67] });
});

test("manifest 使用 sidecar 和 JavaScript 入口", () => {
  /** 确保 TypeScript 构建结果不会被错误声明为生产 Native 动态库。 */
  const manifest = new ManifestBuilder("example.ts", "示例", "1.0.0", "example").module("traffic", "trafficHandler", "udpDatagram").build("plugin.js");
  assert.deepEqual(manifest.runtime, { kind: "sidecar", entry: "plugin.js", protocolVersion: "2.0", arguments: [] });
});

test("构建输出包含可直接加载的完整示例", async () => {
  /**
   * Host 按 manifest 所在目录解析相对 entry；同时检查二者可访问，防止 tsc 只生成
   * JavaScript 却漏掉清单，导致发布包中的示例无法安装。
   */
  const currentDirectory = dirname(fileURLToPath(import.meta.url));
  const exampleDirectory = resolve(currentDirectory, "../examples/binaryProtocol");
  const manifest = JSON.parse(await readFile(join(exampleDirectory, "plugin.json"), "utf8")) as {
    runtime: { entry: string };
  };
  await access(join(exampleDirectory, manifest.runtime.entry));
  assert.equal(manifest.runtime.entry, "plugin.js");
});

test("runner 严格实现 JSONL", async () => {
  /** 启动真实 Node 子进程，验证逐行 ready/result 与 requestId 关联。 */
  const currentDirectory = dirname(fileURLToPath(import.meta.url));
  const runner = resolve(currentDirectory, "../src/runner.js");
  const example = resolve(currentDirectory, "../examples/binaryProtocol/plugin.js");
  const child = spawn(process.execPath, [runner, example], { stdio: ["pipe", "pipe", "pipe"] });
  const lines = createInterface({ input: child.stdout });
  const responses: Record<string, unknown>[] = [];
  lines.on("line", (line) => responses.push(JSON.parse(line) as Record<string, unknown>));
  child.stdin.write(`${JSON.stringify({ type: "initialize", apiVersion: 2, manifest: {}, configuration: {} })}\n`);
  child.stdin.write(`${JSON.stringify({ type: "invoke", requestId: 1, invocation: createInvocation("event-1", "tcpChunk", { bytes: [0, 1, 97] }) })}\n`);
  child.stdin.end(`${JSON.stringify({ type: "stop" })}\n`);
  const [exitCode] = await once(child, "exit");
  assert.equal(exitCode, 0);
  assert.deepEqual(responses[0], { type: "ready", apiVersion: 2 });
  assert.equal(responses[1]?.type, "result");
  assert.equal(responses[1]?.requestId, 1);
  assert.equal(typeof responses[1]?.requestId, "number");
  const result = responses[1] as { action: { patch: { value: unknown }[] } };
  assert.deepEqual(result.action.patch[0]?.value, { bytes: [0, 1, 65] });
});

test("显式并发允许乱序结果并在 stop 后释放一次", async () => {
  /** 真实子进程发送慢、快两个请求，验证原子帧、requestId 和停止等待。 */
  const currentDirectory = dirname(fileURLToPath(import.meta.url));
  const runner = resolve(currentDirectory, "../src/runner.js");
  const fixture = resolve(currentDirectory, "concurrentPlugin.js");
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "capture-ts-sdk-"));
  const probePath = join(temporaryDirectory, "stopped.txt");
  try {
    const child = spawn(process.execPath, [runner, fixture, "--concurrent"], { stdio: ["pipe", "pipe", "pipe"], env: { ...process.env, PLUGIN_STOP_PROBE: probePath } });
    const lines = createInterface({ input: child.stdout });
    const responses: Record<string, unknown>[] = [];
    lines.on("line", (line) => responses.push(JSON.parse(line) as Record<string, unknown>));
    const messages = [
      { type: "initialize", apiVersion: 2, manifest: {}, configuration: {} },
      { type: "invoke", requestId: 1, invocation: createInvocation("slow-event", "udpDatagram", { bytes: [1], delayMs: 150 }) },
      { type: "invoke", requestId: 2, invocation: createInvocation("fast-event", "udpDatagram", { bytes: [2], delayMs: 5 }) },
      { type: "stop" },
    ];
    child.stdin.end(messages.map((message) => JSON.stringify(message)).join("\n") + "\n");
    const [exitCode] = await once(child, "exit");
    assert.equal(exitCode, 0);
    assert.deepEqual(responses.slice(1).map((response) => response.requestId), [2, 1]);
    assert.ok(responses.slice(1).every((response) => typeof response.requestId === "number"));
    assert.equal(await readFile(probePath, "utf8"), "stopped");
  } finally {
    await rm(temporaryDirectory, { recursive: true, force: true });
  }
});
