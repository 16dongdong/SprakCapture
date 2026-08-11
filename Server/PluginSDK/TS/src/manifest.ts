/** 提供 Sidecar manifest 的类型化构造器。 */

import type { Stage } from "./model.js";

export class ManifestBuilder {
  private readonly modules: Record<string, unknown>[] = [];
  private description = "TypeScript 插件";

  /** 保存插件身份；无效身份仍会由 Host 权威清单校验报告。 */
  public constructor(private readonly pluginId: string, private readonly name: string, private readonly version: string, private readonly publisher: string) {}

  /** 设置清单描述并返回构造器；空描述交给 Host 权威校验拒绝。 */
  public describe(description: string): this { this.description = description; return this; }

  /** 添加模块订阅并返回构造器，以函数链完成清单。 */
  public module(moduleId: string, kind: string, ...subscribedStages: Stage[]): this {
    this.modules.push({ id: moduleId, kind, subscriptions: subscribedStages.map((stage) => ({ stage, order: 0, match: {} })), contributes: [] });
    return this;
  }

  /** 生成 Node Sidecar 清单；入口必须是已经编译的相对 .js 文件。 */
  public build(entry: string): Record<string, unknown> {
    if (!entry.endsWith(".js") && !entry.endsWith(".mjs")) throw new Error("TypeScript Sidecar 入口必须编译为 .js 或 .mjs");
    return {
      manifestVersion: 2, id: this.pluginId, name: this.name, description: this.description,
      version: this.version, publisher: this.publisher,
      engines: { host: ">=2.0.0 <3.0.0", api: "2.x" },
      runtime: { kind: "sidecar", entry, protocolVersion: "2.0", arguments: [] },
      modules: [...this.modules], capabilities: [], dependencies: {},
      limits: { timeoutMs: 0, maxPendingEvents: 0, maxOutputBytes: 0, maxStorageBytes: 0 },
      failurePolicy: "failClosed", extensions: { typeScriptSdk: { protocol: "jsonl-v2" } },
    };
  }
}
