import { describe, expect, it } from "vitest";

import type {
  BlockListConfiguration,
  MapLocalConfiguration,
  MapRemoteConfiguration,
  RewriteConfiguration,
} from "@/api/protocol";
import { createServiceSnapshot } from "#tests/testFixtures";
import { validateToolConfiguration } from "@/components/toolConfigurationValidation";

describe("工具配置前置校验", () => {
  /** 本地映射必须在应用前获得非空路径，连接级响应头也不得伪造。 */
  it("阻止空本地路径和代理管理的响应头", () => {
    const configuration: MapLocalConfiguration = {
      enabled: true,
      rules: [
        {
          id: "local-1",
          enabled: true,
          location: { protocol: "", host: "", port: "", path: "", query: null },
          localPath: "   ",
          isDirectory: false,
          statusCode: 200,
          responseHeaders: [],
          contentTypeOverride: "",
        },
      ],
    };

    expect(validateToolConfiguration("mapLocal", configuration)).toMatchObject({
      field: "localPath",
      ruleIndex: 0,
    });
    configuration.rules[0].localPath = "fixtures/index.html";
    configuration.rules[0].responseHeaders = [
      { name: "Content-Length", value: "1" },
    ];
    expect(validateToolConfiguration("mapLocal", configuration)).toMatchObject({
      field: "responseHeaders",
      ruleIndex: 0,
    });
  });

  /** 多字节文本也必须按后端 UTF-8 字节边界预先拒绝，避免配置保存后才收到泛化错误。 */
  it("按 UTF-8 字节数限制正文与映射文本字段", () => {
    const blockList: BlockListConfiguration = {
      mode: "blockList",
      locations: [],
      statusCode: 200,
      responseBody: "你".repeat(21_846),
      closeConnection: false,
    };
    expect(validateToolConfiguration("blockList", blockList)).toMatchObject({
      field: "responseBody",
    });

    const local: MapLocalConfiguration = {
      enabled: true,
      rules: [
        {
          id: "local-byte-limit",
          enabled: true,
          location: { protocol: "", host: "", port: "", path: "", query: null },
          localPath: "你".repeat(1_366),
          isDirectory: false,
          statusCode: 200,
          responseHeaders: [],
          contentTypeOverride: "",
        },
      ],
    };
    expect(validateToolConfiguration("mapLocal", local)).toMatchObject({
      field: "localPath",
      ruleIndex: 0,
    });

    const remote: MapRemoteConfiguration = {
      enabled: true,
      rules: [
        {
          id: "remote-byte-limit",
          enabled: true,
          from: { protocol: "", host: "origin.test", port: "", path: "", query: null },
          to: { protocol: "https", host: "target.test", port: "", path: `/${"你".repeat(683)}` },
        },
      ],
    };
    expect(validateToolConfiguration("mapRemote", remote)).toMatchObject({
      field: "mapTo",
      ruleIndex: 0,
    });
  });

  /** 远程映射目标只接受单一十进制端口和来源已有星号捕获，不能误用 Location 范围表达式。 */
  it("阻止无效远程目标端口和路径模板", () => {
    const configuration: MapRemoteConfiguration = {
      enabled: true,
      rules: [
        {
          id: "remote-1",
          enabled: true,
          from: { protocol: "", host: "origin.test", port: "", path: "/api/*", query: null },
          to: { protocol: "https", host: "target.test", port: "80-90", path: "/v2/*" },
        },
      ],
    };

    expect(validateToolConfiguration("mapRemote", configuration)).toMatchObject({
      field: "mapTo",
      ruleIndex: 0,
    });
    configuration.rules[0].to.port = "443";
    configuration.rules[0].to.path = "/v2/*/*";
    expect(validateToolConfiguration("mapRemote", configuration)).toMatchObject({
      field: "mapTo",
      ruleIndex: 0,
    });
  });

  /** Header 重写在切换类型后必须补齐头名称和动作，正则语法错误不会再等到控制 API 返回。 */
  it("阻止不完整 Header 重写和非法正则", () => {
    const configuration: RewriteConfiguration = {
      enabled: true,
      sets: [
        {
          id: "set-1",
          name: "请求头",
          enabled: true,
          locations: [],
          rules: [
            {
              id: "rule-1",
              enabled: true,
              type: "requestHeader" as const,
              matchRegex: "(",
              replace: "value",
              headerName: null,
              matchValueRegex: null,
              headerAction: null,
              caseSensitive: false,
              matchAllOccurrences: true,
            },
          ],
        },
      ],
    };

    expect(validateToolConfiguration("rewrite", configuration)).toMatchObject({
      field: "rules",
      setIndex: 0,
      ruleIndex: 0,
    });
    configuration.sets[0].rules[0].matchRegex = ".*";
    configuration.sets[0].rules[0].headerName = "X-Trace";
    configuration.sets[0].rules[0].headerAction = "add";
    expect(validateToolConfiguration("rewrite", configuration)).toBeNull();
  });

  /** 节流前端边界必须完整接受后端允许的 0% 可靠性、64 字节 MTU 和 300 秒延迟。 */
  it("接受后端定义的节流边界", () => {
    const snapshot = createServiceSnapshot();
    const { presets: _presets, ...configuration } = snapshot.tools.throttling;
    const boundaryConfiguration = {
      ...configuration,
      custom: {
        ...configuration.custom,
        latencyMilliseconds: 300_000,
        latencyJitterMilliseconds: 300_000,
        reliabilityPercent: 0,
        mtu: 64,
      },
    };

    expect(validateToolConfiguration("throttling", boundaryConfiguration)).toBeNull();
  });
});
