import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type {
  BlockCookiesConfiguration,
  BlockListConfiguration,
  BreakpointsConfiguration,
  DnsSpoofingConfiguration,
  EventMessage,
  MapLocalConfiguration,
  MapRemoteConfiguration,
  NoCachingConfiguration,
  RewriteConfiguration,
  ServiceSnapshot,
  ThrottlingConfiguration,
} from "@/api/protocol";
import i18n from "@/i18n";
import { ServiceProvider } from "@/state/serviceStore";
import {
  createControlClientStub,
  createServiceSnapshot,
} from "#tests/testFixtures";
import { ToolSettingsDialog, type ToolDialogId } from "@/components/toolSettingsDialog";
import type { TransactionToolSeed } from "@/components/transactionToolSeed";

/** 创建已连接的事件流替身，使表单测试只覆盖对话框和控制请求语义。 */
function createConnectedEventClient() {
  return {
    start(callbacks: {
      onConnectionState(state: "connected", message: string): void;
      onMessage?(message: EventMessage): void;
    }) {
      callbacks.onConnectionState("connected", "事件流已连接");
    },
    stop() {},
  };
}

/** 以最新快照渲染一个工具对话框，确保 Apply 后的重读行为与运行时一致。 */
function renderToolDialog(
  open: Exclude<ToolDialogId, "export">,
  currentSnapshot: () => ServiceSnapshot,
  overrides: Parameters<typeof createControlClientStub>[1],
  initialSeed: TransactionToolSeed | null = null,
) {
  const controlClient = createControlClientStub(currentSnapshot(), {
    getSnapshot: async () => currentSnapshot(),
    ...overrides,
  });
  return render(
    <ServiceProvider
      controlClient={controlClient}
      eventClient={createConnectedEventClient()}
    >
      <ToolSettingsDialog
        initialSeed={initialSeed}
        open={open}
        onClose={() => undefined}
      />
    </ServiceProvider>,
  );
}

/** 返回当前测试语言下的表单文案，避免测试把某一种翻译当作协议字段。 */
function formLabel(key: string): string {
  return i18n.t(`tools.form.${key}`);
}

describe("M3 工具可视化配置对话框", () => {
  it("设置操作区只显示取消和应用", async () => {
    const currentSnapshot = createServiceSnapshot();
    const { container } = renderToolDialog(
      "blockList",
      () => currentSnapshot,
      {},
    );

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(container.querySelector(".toolDialogHeader button")).toBeNull();
    expect(container.querySelectorAll(".toolDialogFooter > button")).toHaveLength(2);
  });

  it("从事务右键上下文创建并选中已填好位置的本地映射规则", async () => {
    const user = userEvent.setup();
    const currentSnapshot = createServiceSnapshot();
    renderToolDialog(
      "mapLocal",
      () => currentSnapshot,
      {},
      {
        transactionId: "transaction-context",
        contentType: "application/json",
        location: {
          protocol: "https",
          host: "assets.example",
          port: "8443",
          path: "/app/config.json",
          query: "build=7",
        },
      },
    );

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    await user.click(await screen.findByRole("button", { name: /assets\.example/ }));
    const matchLocation = formLabel("matchLocation");
    expect(
      await screen.findByRole("combobox", {
        name: `${matchLocation} ${formLabel("protocol")}`,
      }),
    ).toHaveValue("https");
    expect(
      screen.getByRole("textbox", {
        name: `${matchLocation} ${formLabel("host")}`,
      }),
    ).toHaveValue("assets.example");
    expect(
      screen.getByRole("textbox", {
        name: `${matchLocation} ${formLabel("port")}`,
      }),
    ).toHaveValue("8443");
    expect(
      screen.getByRole("textbox", {
        name: `${matchLocation} ${formLabel("path")}`,
      }),
    ).toHaveValue("/app/config.json");
    expect(
      screen.getByRole("textbox", {
        name: `${matchLocation} ${formLabel("query")}`,
      }),
    ).toHaveValue("build=7");
    expect(
      screen.getByRole("textbox", {
        name: formLabel("contentTypeOverride"),
      }),
    ).toHaveValue("application/json");
  });

  /** 屏蔽列表通过模式、作用域和合成响应字段提交，不再暴露 JSON 配置输入。 */
  it("以可视化字段提交屏蔽列表规则", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateBlockList = vi.fn(async (configuration: BlockListConfiguration) => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        tools: { ...currentSnapshot.tools, blockList: configuration },
      };
      return currentSnapshot.tools;
    });
    renderToolDialog("blockList", () => currentSnapshot, { updateBlockList });

    await screen.findByRole("dialog");
    expect(screen.queryByLabelText(i18n.t("tools.configuration"))).not.toBeInTheDocument();
    await user.selectOptions(
      screen.getByRole("combobox", { name: formLabel("blockMode") }),
      "blockList",
    );
    await user.click(
      screen.getByRole("button", { name: formLabel("addLocation") }),
    );
    await user.type(
      screen.getByRole("textbox", {
        name: `${formLabel("scope")} ${formLabel("host")}`,
      }),
      "*.example.com",
    );
    await user.click(screen.getByRole("button", { name: formLabel("saveRule") }));
    await user.clear(screen.getByRole("spinbutton", { name: formLabel("statusCode") }));
    await user.type(
      screen.getByRole("spinbutton", { name: formLabel("statusCode") }),
      "451",
    );
    await user.type(
      screen.getByRole("textbox", { name: formLabel("responseBody") }),
      "blocked",
    );
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));

    await waitFor(() => expect(updateBlockList).toHaveBeenCalledTimes(1));
    expect(updateBlockList).toHaveBeenCalledWith({
      mode: "blockList",
      locations: [
        {
          protocol: "",
          host: "*.example.com",
          port: "",
          path: "",
          query: null,
        },
      ],
      statusCode: 451,
      responseBody: "blocked",
      closeConnection: false,
    });
  });

  /** 屏蔽列表只通过 mode 切换关闭、黑名单和白名单，避免统一开关把 allowList 错写成 blockList。 */
  it("保留白名单模式并可显式切换关闭状态", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    currentSnapshot = {
      ...currentSnapshot,
      tools: {
        ...currentSnapshot.tools,
        blockList: {
          ...currentSnapshot.tools.blockList,
          mode: "allowList",
        },
      },
    };
    const updateBlockList = vi.fn(async (configuration: BlockListConfiguration) => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        tools: { ...currentSnapshot.tools, blockList: configuration },
      };
      return currentSnapshot.tools;
    });
    renderToolDialog("blockList", () => currentSnapshot, { updateBlockList });

    await screen.findByRole("dialog");
    const mode = screen.getByRole("combobox", { name: formLabel("blockMode") });
    expect(mode).toHaveValue("allowList");
    await user.selectOptions(mode, "off");
    await user.selectOptions(mode, "allowList");
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));

    await waitFor(() => expect(updateBlockList).toHaveBeenCalledTimes(1));
    expect(updateBlockList).toHaveBeenLastCalledWith({
      ...createServiceSnapshot().tools.blockList,
      mode: "allowList",
    });
  });

  /** 无缓存和 Cookie 两类方向开关均写回对应工具，不混入其它工具的字段。 */
  it("分别提交无缓存与 Cookie 方向开关", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateNoCaching = vi.fn(async (configuration: NoCachingConfiguration) => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        tools: { ...currentSnapshot.tools, noCaching: configuration },
      };
      return currentSnapshot.tools;
    });
    const firstRender = renderToolDialog("noCaching", () => currentSnapshot, {
      updateNoCaching,
    });

    await screen.findByRole("dialog");
    expect(screen.queryByLabelText(i18n.t("tools.configuration"))).not.toBeInTheDocument();
    await user.click(screen.getAllByRole("checkbox")[0]);
    await user.click(
      screen.getByRole("checkbox", { name: formLabel("injectNoStore") }),
    );
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));
    await waitFor(() => expect(updateNoCaching).toHaveBeenCalledTimes(1));
    expect(updateNoCaching).toHaveBeenCalledWith({
      enabled: true,
      locations: [],
      stripRequestHeaders: true,
      stripResponseHeaders: true,
      injectRequestNoCache: true,
      injectResponseNoStore: false,
    });

    firstRender.unmount();
    const updateBlockCookies = vi.fn(async (configuration: BlockCookiesConfiguration) => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        tools: { ...currentSnapshot.tools, blockCookies: configuration },
      };
      return currentSnapshot.tools;
    });
    renderToolDialog("blockCookies", () => currentSnapshot, { updateBlockCookies });

    await screen.findByRole("dialog");
    await user.click(screen.getAllByRole("checkbox")[0]);
    await user.click(
      screen.getByRole("checkbox", { name: formLabel("cookieRequest") }),
    );
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));
    await waitFor(() => expect(updateBlockCookies).toHaveBeenCalledTimes(1));
    expect(updateBlockCookies).toHaveBeenCalledWith({
      enabled: true,
      locations: [],
      stripRequestCookie: false,
      stripResponseSetCookie: true,
    });
  });

  /** DNS 映射使用有序主机模式和 IP 字段提交，不暴露协议对象文本。 */
  it("以结构化规则提交 DNS 映射并保留域名语义", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateDnsSpoofing = vi.fn(
      async (configuration: DnsSpoofingConfiguration) => {
        currentSnapshot = {
          ...currentSnapshot,
          revision: currentSnapshot.revision + 1,
          tools: { ...currentSnapshot.tools, dnsSpoofing: configuration },
        };
        return currentSnapshot.tools;
      },
    );
    renderToolDialog("dnsSpoofing", () => currentSnapshot, {
      updateDnsSpoofing,
    });

    await screen.findByRole("dialog");
    await user.click(screen.getAllByRole("checkbox")[0]);
    await user.click(screen.getByRole("button", { name: formLabel("addRule") }));
    await user.type(
      screen.getByRole("textbox", { name: formLabel("hostPattern") }),
      "*.fixture.test",
    );
    await user.type(
      screen.getByRole("textbox", { name: formLabel("ipAddress") }),
      "127.0.0.1",
    );
    await user.click(screen.getByRole("button", { name: formLabel("saveRule") }));
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));

    await waitFor(() => expect(updateDnsSpoofing).toHaveBeenCalledTimes(1));
    expect(updateDnsSpoofing).toHaveBeenCalledWith({
      enabled: true,
      rules: [
        {
          id: "dns-1",
          enabled: true,
          hostPattern: "*.fixture.test",
          ipAddress: "127.0.0.1",
        },
      ],
    });
  });

  /** Map Local 以“匹配位置 + 本地替换 + 响应头表”提交完整规则。 */
  it("以结构化规则提交本地映射和响应头", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateMapLocal = vi.fn(async (configuration: MapLocalConfiguration) => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        tools: { ...currentSnapshot.tools, mapLocal: configuration },
      };
      return currentSnapshot.tools;
    });
    renderToolDialog("mapLocal", () => currentSnapshot, { updateMapLocal });

    await screen.findByRole("dialog");
    expect(screen.queryByLabelText(i18n.t("tools.configuration"))).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: formLabel("addRule") }));
    await user.type(
      screen.getByRole("textbox", {
        name: `${formLabel("matchLocation")} ${formLabel("host")}`,
      }),
      "assets.example.com",
    );
    await user.type(
      screen.getByRole("textbox", { name: formLabel("localPath") }),
      "fixtures/app.js",
    );
    await user.click(screen.getByRole("button", { name: formLabel("addHeader") }));
    await user.type(
      screen.getByRole("textbox", { name: `${formLabel("headerName")} 1` }),
      "X-Source",
    );
    await user.type(
      screen.getByRole("textbox", { name: `${formLabel("headerValue")} 1` }),
      "local",
    );
    await user.click(screen.getByRole("button", { name: formLabel("saveRule") }));
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));

    await waitFor(() => expect(updateMapLocal).toHaveBeenCalledTimes(1));
    expect(updateMapLocal).toHaveBeenCalledWith({
      enabled: true,
      rules: [
        {
          id: "local-1",
          enabled: true,
          location: {
            protocol: "",
            host: "assets.example.com",
            port: "",
            path: "",
            query: null,
          },
          localPath: "fixtures/app.js",
          isDirectory: false,
          statusCode: 200,
          responseHeaders: [{ name: "X-Source", value: "local" }],
          contentTypeOverride: "",
        },
      ],
    });
  });

  /** 目录选择必须打开目录模式、上传完整相对层级，并把后端返回的受管路径写回当前规则。 */
  it("通过文件选择器导入本地目录并回填映射路径", async () => {
    const user = userEvent.setup();
    const currentSnapshot = createServiceSnapshot();
    const importMapLocalFiles = vi.fn(async () => ({
      localPath: "imports/import-id/site",
      fileCount: 2,
      totalBytes: 6,
    }));
    renderToolDialog("mapLocal", () => currentSnapshot, {
      importMapLocalFiles,
    });

    await screen.findByRole("dialog");
    await user.click(screen.getAllByRole("checkbox")[0]);
    await user.click(screen.getByRole("button", { name: formLabel("addRule") }));
    await user.click(screen.getByRole("checkbox", { name: formLabel("directoryMapping") }));
    const chooseDirectory = screen.getByRole("button", {
      name: formLabel("chooseDirectory"),
    });
    await user.click(chooseDirectory);
    const picker = document.querySelector<HTMLInputElement>('input[type="file"]');
    expect(picker).not.toBeNull();
    expect(picker).toHaveAttribute("webkitdirectory");
    const indexFile = new File(["index"], "index.html", { type: "text/html" });
    const scriptFile = new File(["x"], "app.js", { type: "text/javascript" });
    Object.defineProperty(indexFile, "webkitRelativePath", {
      value: "site/index.html",
    });
    Object.defineProperty(scriptFile, "webkitRelativePath", {
      value: "site/assets/app.js",
    });
    await user.upload(picker as HTMLInputElement, [indexFile, scriptFile]);

    await waitFor(() => expect(importMapLocalFiles).toHaveBeenCalledTimes(1));
    expect(importMapLocalFiles).toHaveBeenCalledWith({
      directory: true,
      files: [
        { file: indexFile, relativePath: "site/index.html" },
        { file: scriptFile, relativePath: "site/assets/app.js" },
      ],
    });
    expect(screen.getByRole("textbox", { name: formLabel("localPath") })).toHaveValue(
      "imports/import-id/site",
    );
  });

  /** 子对话框使用字段约束阻止半成品写回，取消后也不应把过期无效状态留给主表单。 */
  it("在规则对话框内阻止无效字段并支持无副作用取消", async () => {
    const user = userEvent.setup();
    const currentSnapshot = createServiceSnapshot();
    const updateMapLocal = vi.fn();
    renderToolDialog("mapLocal", () => currentSnapshot, { updateMapLocal });

    await screen.findByRole("dialog");
    await user.click(screen.getAllByRole("checkbox")[0]);
    await user.click(screen.getByRole("button", { name: formLabel("addRule") }));
    const saveButton = screen.getByRole("button", { name: formLabel("saveRule") });
    const localPath = screen.getByRole("textbox", { name: formLabel("localPath") });
    await user.click(saveButton);
    expect(localPath).toBeInvalid();

    await user.type(localPath, "fixtures/index.html");
    await user.clear(screen.getByRole("spinbutton", { name: formLabel("statusCode") }));
    await user.type(
      screen.getByRole("spinbutton", { name: formLabel("statusCode") }),
      "99",
    );
    await user.click(saveButton);
    expect(screen.getByRole("spinbutton", { name: formLabel("statusCode") })).toBeInvalid();
    await user.click(screen.getByRole("button", { name: formLabel("cancelRule") }));
    expect(screen.queryByRole("dialog", { name: formLabel("ruleDialogTitle") })).not.toBeInTheDocument();
    expect(updateMapLocal).not.toHaveBeenCalled();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: i18n.t("tools.apply") }),
    ).toBeEnabled();
  });

  /** Map Remote 将来源位置和目标协议/主机/端口/路径作为独立字段提交。 */
  it("以来源和目标字段提交远程映射", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateMapRemote = vi.fn(async (configuration: MapRemoteConfiguration) => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        tools: { ...currentSnapshot.tools, mapRemote: configuration },
      };
      return currentSnapshot.tools;
    });
    renderToolDialog("mapRemote", () => currentSnapshot, { updateMapRemote });

    await screen.findByRole("dialog");
    expect(screen.queryByLabelText(i18n.t("tools.configuration"))).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: formLabel("addRule") }));
    await user.type(
      screen.getByRole("textbox", {
        name: `${formLabel("mapFrom")} ${formLabel("host")}`,
      }),
      "origin.example.com",
    );
    await user.selectOptions(
      screen.getByRole("combobox", {
        name: `${formLabel("mapTo")} ${formLabel("protocol")}`,
      }),
      "wss",
    );
    await user.type(
      screen.getByRole("textbox", {
        name: `${formLabel("mapTo")} ${formLabel("host")}`,
      }),
      "target.example.com",
    );
    await user.type(
      screen.getByRole("textbox", {
        name: `${formLabel("mapTo")} ${formLabel("port")}`,
      }),
      "8443",
    );
    await user.type(
      screen.getByRole("textbox", {
        name: `${formLabel("mapTo")} ${formLabel("path")}`,
      }),
      "/v2",
    );
    await user.click(screen.getByRole("button", { name: formLabel("saveRule") }));
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));

    await waitFor(() => expect(updateMapRemote).toHaveBeenCalledTimes(1));
    expect(updateMapRemote).toHaveBeenCalledWith({
      enabled: true,
      rules: [
        {
          id: "remote-1",
          enabled: true,
          from: {
            protocol: "",
            host: "origin.example.com",
            port: "",
            path: "",
            query: null,
          },
          to: {
            protocol: "wss",
            host: "target.example.com",
            port: "8443",
            path: "/v2",
          },
        },
      ],
    });
  });

  /** Rewrite 通过规则集与规则明细提交，不应回退为协议对象文本编辑。 */
  it("以规则集和字段编辑提交 Rewrite", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateRewrite = vi.fn(async (configuration: RewriteConfiguration) => {
      currentSnapshot = {
        ...currentSnapshot,
        revision: currentSnapshot.revision + 1,
        tools: { ...currentSnapshot.tools, rewrite: configuration },
      };
      return currentSnapshot.tools;
    });
    renderToolDialog("rewrite", () => currentSnapshot, { updateRewrite });

    await screen.findByRole("dialog");
    expect(screen.queryByLabelText(i18n.t("tools.configuration"))).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: formLabel("addSet") }));
    await user.clear(screen.getByRole("textbox", { name: formLabel("setName") }));
    await user.type(
      screen.getByRole("textbox", { name: formLabel("setName") }),
      "路径重写",
    );
    await user.click(screen.getByRole("button", { name: formLabel("addRule") }));
    await user.type(
      screen.getByRole("textbox", { name: formLabel("matchExpression") }),
      "^/legacy",
    );
    await user.type(
      screen.getByRole("textbox", { name: formLabel("replacement") }),
      "/current",
    );
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));

    await waitFor(() => expect(updateRewrite).toHaveBeenCalledTimes(1));
    expect(updateRewrite).toHaveBeenCalledWith({
      enabled: true,
      sets: [
        {
          id: "rewrite-set-1",
          name: "路径重写",
          enabled: true,
          locations: [],
          rules: [
            {
              id: "rewrite-rule-1",
              enabled: true,
              type: "urlPath",
              matchRegex: "^/legacy",
              replace: "/current",
              headerName: null,
              matchValueRegex: null,
              headerAction: null,
              caseSensitive: false,
              matchAllOccurrences: true,
            },
          ],
        },
      ],
    });
  });

  /** 断点规则把阶段、Location 与超时边界拆成普通控件，避免用户手工构造配置对象。 */
  it("以字段编辑提交断点规则和超时边界", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateBreakpoints = vi.fn(
      async (configuration: BreakpointsConfiguration) => {
        currentSnapshot = {
          ...currentSnapshot,
          revision: currentSnapshot.revision + 1,
          tools: { ...currentSnapshot.tools, breakpoints: configuration },
        };
        return currentSnapshot.tools;
      },
    );
    renderToolDialog("breakpoints", () => currentSnapshot, { updateBreakpoints });

    await screen.findByRole("dialog");
    expect(screen.queryByLabelText(i18n.t("tools.configuration"))).not.toBeInTheDocument();
    await user.clear(
      screen.getByRole("spinbutton", { name: formLabel("suspendTimeout") }),
    );
    await user.type(
      screen.getByRole("spinbutton", { name: formLabel("suspendTimeout") }),
      "45",
    );
    await user.click(screen.getByRole("button", { name: formLabel("addRule") }));
    await user.type(
      screen.getByRole("textbox", {
        name: `${formLabel("matchLocation")} ${formLabel("host")}`,
      }),
      "pause.example.com",
    );
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));

    await waitFor(() => expect(updateBreakpoints).toHaveBeenCalledTimes(1));
    expect(updateBreakpoints).toHaveBeenCalledWith({
      enabled: true,
      rules: [
        {
          id: "breakpoint-rule-1",
          enabled: true,
          location: {
            protocol: "",
            host: "pause.example.com",
            port: "",
            path: "",
            query: null,
          },
          onRequest: true,
          onResponse: false,
        },
      ],
      suspendTimeoutSeconds: 45,
      maxSuspended: 32,
      onTimeout: "continue",
    });
  });

  /** 带宽限制将预设、自定义速率和 Location 拆分展示，提交时不携带只读预设目录。 */
  it("以预设、速率和作用域字段提交带宽限制", async () => {
    const user = userEvent.setup();
    let currentSnapshot = createServiceSnapshot();
    const updateThrottling = vi.fn(
      async (configuration: ThrottlingConfiguration) => {
        currentSnapshot = {
          ...currentSnapshot,
          revision: currentSnapshot.revision + 1,
          tools: { ...currentSnapshot.tools, throttling: {
            ...configuration,
            presets: currentSnapshot.tools.throttling.presets,
          } },
        };
        return currentSnapshot.tools;
      },
    );
    renderToolDialog("throttling", () => currentSnapshot, { updateThrottling });

    await screen.findByRole("dialog");
    expect(screen.queryByLabelText(i18n.t("tools.configuration"))).not.toBeInTheDocument();
    await user.click(screen.getAllByRole("checkbox")[0]);
    await user.selectOptions(
      screen.getByRole("combobox", { name: formLabel("preset") }),
      "lte",
    );
    await user.clear(
      screen.getByRole("spinbutton", { name: formLabel("downloadSpeed") }),
    );
    await user.type(
      screen.getByRole("spinbutton", { name: formLabel("downloadSpeed") }),
      "2048",
    );
    await user.click(
      screen.getByRole("button", { name: formLabel("addLocation") }),
    );
    await user.type(
      screen.getByRole("textbox", {
        name: `${formLabel("scope")} ${formLabel("host")}`,
      }),
      "slow.example.com",
    );
    await user.click(screen.getByRole("button", { name: formLabel("saveRule") }));
    await user.click(screen.getByRole("button", { name: i18n.t("tools.apply") }));

    await waitFor(() => expect(updateThrottling).toHaveBeenCalledTimes(1));
    expect(updateThrottling).toHaveBeenCalledWith({
      enabled: true,
      activePresetId: null,
      custom: {
        downloadBytesPerSecond: 2048,
        uploadBytesPerSecond: 3 * 1024 * 1024,
        latencyMilliseconds: 50,
        latencyJitterMilliseconds: 0,
        reliabilityPercent: 100,
        mtu: 1500,
      },
      locations: [
        {
          protocol: "",
          host: "slow.example.com",
          port: "",
          path: "",
          query: null,
        },
      ],
    });
  });

});
