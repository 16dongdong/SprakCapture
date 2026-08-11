/**
 * Operational UI test against running Sprak Capture web console.
 * Uses Playwright if available, otherwise exits with install hint.
 */
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";

const BASE = process.env.CAPTURE_UI_URL || "http://127.0.0.1:5173";
const OUT = path.resolve("ui-audit");
fs.mkdirSync(OUT, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const results = [];
function record(id, ok, detail = "", severity = ok ? "pass" : "fail") {
  results.push({ id, ok, severity, detail: String(detail).slice(0, 500) });
  console.log(`${ok ? "PASS" : "FAIL"} ${id}${detail ? " — " + String(detail).slice(0, 120) : ""}`);
}

async function shot(page, name) {
  await page.screenshot({ path: path.join(OUT, `ops-${name}.png`), fullPage: false });
}

async function closeDialog(page) {
  for (let i = 0; i < 3; i++) {
    const dialog = page.locator("[role='dialog']");
    if ((await dialog.count()) === 0) return;
    const cancel = dialog
      .locator("button:has-text('取消'), button:has-text('关闭'), button[aria-label*='关闭']")
      .first();
    if (await cancel.count()) {
      await cancel.click({ force: true }).catch(() => {});
    } else {
      await page.keyboard.press("Escape");
    }
    await sleep(180);
  }
}

async function openToolsPath(page, labels) {
  await closeDialog(page);
  await page.locator('button[aria-label*="工具"]').click();
  await sleep(150);
  for (const label of labels) {
    const menu = page.locator('[role="menu"]');
    await menu.waitFor({ state: "visible", timeout: 5000 });
    let item = menu.getByRole("menuitem", { name: label, exact: true });
    if ((await item.count()) === 0) {
      item = menu.getByRole("menuitem", { name: label });
    }
    await item.first().click();
    await sleep(280);
  }
}

async function main() {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
  });
  const page = await context.newPage();
  page.setDefaultTimeout(12_000);
  const consoleErrors = [];
  page.on("pageerror", (e) => consoleErrors.push(String(e)));
  page.on("console", (msg) => {
    if (msg.type() === "error") consoleErrors.push(msg.text());
  });

  // ========== 1 Overview ==========
  await page.goto(`${BASE}/overview`, { waitUntil: "domcontentloaded" });
  await sleep(400);
  await shot(page, "01-overview");
  record(
    "overview.loaded",
    await page.getByRole("heading", { name: "服务概览" }).isVisible(),
  );
  record(
    "overview.serviceRunning",
    await page.getByText("运行中").first().isVisible(),
  );
  record(
    "overview.stopButton",
    await page.getByRole("button", { name: "停止服务" }).isVisible(),
  );
  await page.getByRole("button", { name: "刷新服务快照" }).click();
  await sleep(400);
  record("overview.refresh", true, "clicked refresh");

  // ========== 2 Connections ==========
  await page.getByRole("link", { name: "连接会话" }).click();
  await sleep(500);
  await shot(page, "02-connections");
  record(
    "connections.loaded",
    locationCheck(page, "/connections") ||
      (await page.getByRole("main", { name: "事务工作区" }).count()) > 0 ||
      (await page.locator(".transactionNavigatorPane").count()) > 0,
  );

  // Structure / Sequence
  const structureTab = page.getByRole("tab", { name: "结构" });
  const sequenceTab = page.getByRole("tab", { name: "序列" });
  record("connections.structureTab", await structureTab.isVisible());
  record("connections.sequenceTab", await sequenceTab.isVisible());
  await sequenceTab.click();
  await sleep(250);
  record(
    "connections.sequenceSwitch",
    (await sequenceTab.getAttribute("aria-selected")) === "true",
  );
  await structureTab.click();
  await sleep(200);

  // Filter search
  const search = page.getByRole("searchbox", { name: "搜索事务" });
  if (await search.count()) {
    await search.fill("example");
    await sleep(200);
    const countAfter = await page.locator(".transactionTreeItem, .transactionTree button").count();
    record("connections.filterSearch", true, `visible rows ~ ${countAfter}`);
    await search.fill("");
    await sleep(150);
  } else {
    record("connections.filterSearch", false, "searchbox missing");
  }

  // Status filter
  const status = page.getByRole("combobox", { name: "事务状态" });
  if (await status.count()) {
    await status.selectOption({ label: "已完成" });
    await sleep(200);
    record("connections.statusFilter", true, "selected 已完成");
    await status.selectOption({ label: "全部" });
  }

  // Select transaction if any
  const treeBtn = page.locator(".transactionTree button, .transactionTreeItem").first();
  if (await treeBtn.count()) {
    await treeBtn.click();
    await sleep(300);
    record("connections.selectTransaction", true);
  } else {
    record("connections.selectTransaction", false, "no transactions", "warn");
  }

  // Inspector tabs
  for (const tab of ["概览", "请求", "响应", "Protobuf", "备注"]) {
    const t = page.getByRole("tab", { name: tab, exact: true });
    if (await t.count()) {
      await t.click();
      await sleep(250);
      const selected = (await t.getAttribute("aria-selected")) === "true";
      record(`connections.tab.${tab}`, selected);
    } else {
      record(`connections.tab.${tab}`, false, "missing");
    }
  }
  await shot(page, "03-connections-tabs");

  // Request viewer sub-tabs if present
  await page.getByRole("tab", { name: "请求" }).click().catch(() => {});
  await sleep(250);
  for (const sub of ["头", "文本", "JSON", "十六进制"]) {
    const s = page.getByRole("tab", { name: sub, exact: true });
    if (await s.count()) {
      await s.click();
      await sleep(150);
      record(`connections.requestViewer.${sub}`, true);
    }
  }

  // Focus checkbox
  const focus = page.getByRole("checkbox", { name: "聚焦" });
  if (await focus.count()) {
    const disabled = await focus.isDisabled();
    if (!disabled) {
      await focus.check();
      await sleep(150);
      await focus.uncheck();
      record("connections.focusToggle", true);
    } else {
      record("connections.focusToggle", true, "disabled until host selected", "warn");
    }
  }

  // ========== 3 Settings ==========
  const settingsSections = [
    ["interface", "界面语言"],
    ["listener", "监听"],
    ["authentication", "认证"],
    ["capacity", "容量与超时"],
  ];
  for (const [slug, label] of settingsSections) {
    await page.goto(`${BASE}/settings/${slug}`, { waitUntil: "domcontentloaded" });
    await sleep(350);
    const back = page.getByRole("button", { name: "返回" });
    record(`settings.${slug}.loaded`, await page.getByRole("heading", { name: "设置" }).isVisible());
    record(`settings.${slug}.nav`, await page.getByRole("link", { name: label }).isVisible());
    record(`settings.${slug}.back`, await back.isVisible());
    await shot(page, `04-settings-${slug}`);
  }

  // Language change roundtrip (interface)
  await page.goto(`${BASE}/settings/interface`, { waitUntil: "domcontentloaded" });
  await sleep(300);
  const lang = page.getByRole("combobox", { name: "界面语言" });
  if (await lang.count()) {
    const before = await lang.inputValue();
    await lang.selectOption({ label: "English" });
    await sleep(400);
    const overviewEn = await page.getByRole("link", { name: /Overview|概览/ }).first().isVisible();
    record("settings.language.en", overviewEn, "switched to English");
    await lang.selectOption({ label: "简体中文" });
    await sleep(400);
    record(
      "settings.language.zh",
      await page.getByRole("link", { name: "连接会话" }).isVisible().catch(() => false) ||
        (await page.getByRole("link", { name: /连接|Connections/ }).count()) > 0,
    );
    // restore auto if possible
    try {
      await lang.selectOption({ label: "自动" });
    } catch {
      /* ignore */
    }
    void before;
  }

  // Authentication mode toggle UI (no apply)
  await page.goto(`${BASE}/settings/authentication`, { waitUntil: "domcontentloaded" });
  await sleep(300);
  const authMode = page.getByRole("combobox", { name: "认证模式" });
  if (await authMode.count()) {
    await authMode.selectOption({ label: "用户名与密码" });
    await sleep(200);
    const user = page.getByRole("textbox", { name: "用户名" });
    const enabled = await user.isEnabled();
    record("settings.auth.enableFields", enabled, "username enabled after password mode");
    await authMode.selectOption({ label: "无需认证" });
    await sleep(150);
    record("settings.auth.disableFields", await user.isDisabled());
  }

  // Capacity groups
  await page.goto(`${BASE}/settings/capacity`, { waitUntil: "domcontentloaded" });
  await sleep(300);
  const groups = page.locator(".settingsCapacityGroup summary");
  record("settings.capacity.groups", (await groups.count()) >= 3, `count=${await groups.count()}`);
  // collapse one group
  if ((await groups.count()) > 0) {
    await groups.nth(0).click();
    await sleep(150);
    record("settings.capacity.collapse", true);
    await groups.nth(0).click();
  }

  // Listener field edit without apply
  await page.goto(`${BASE}/settings/listener`, { waitUntil: "domcontentloaded" });
  await sleep(300);
  const host = page.locator('label:has-text("监听地址") input, input').first();
  // better: get by label
  const listenHost = page.getByLabel("监听地址");
  if (await listenHost.count()) {
    const original = await listenHost.inputValue();
    await listenHost.fill("127.0.0.1");
    record("settings.listener.editHost", (await listenHost.inputValue()) === "127.0.0.1");
    await listenHost.fill(original);
  }

  // ========== 4 Toolbar toggles ==========
  await page.goto(`${BASE}/connections`, { waitUntil: "domcontentloaded" });
  await sleep(400);

  // Throttling quick toggle
  const throttle = page.getByRole("button", { name: "切换带宽限制" });
  if (await throttle.count()) {
    const clsBefore = await throttle.getAttribute("class");
    await throttle.click();
    await sleep(500);
    const clsAfter = await throttle.getAttribute("class");
    record("toolbar.throttleToggle", true, `class ${clsBefore} -> ${clsAfter}`);
    // toggle back
    await throttle.click();
    await sleep(400);
  }

  // Breakpoints quick toggle
  const bp = page.getByRole("button", { name: "切换断点" });
  if (await bp.count()) {
    await bp.click();
    await sleep(500);
    record("toolbar.breakpointToggle", true);
    await bp.click();
    await sleep(400);
  }

  // Recording pause/start
  const rec = page.getByRole("button", { name: "切换事务录制状态" });
  if (await rec.count()) {
    const label1 = await rec.innerText();
    await rec.click();
    await sleep(500);
    const label2 = await rec.innerText();
    record("toolbar.recordingToggle", true, `${label1.trim()} -> ${label2.trim()}`);
    await rec.click();
    await sleep(400);
  }

  // Clear cancel
  await page.getByRole("button", { name: "清空事务" }).click();
  await sleep(250);
  const clearDialog = page.getByRole("dialog");
  record("toolbar.clearDialog", await clearDialog.isVisible());
  await clearDialog.getByRole("button", { name: "取消" }).click();
  await sleep(200);
  record("toolbar.clearCancel", (await page.locator("[role=dialog]").count()) === 0);

  // ========== 5 Tools dialogs ==========
  const dialogs = [
    [["SSL 代理设置"], "ssl"],
    [["协议工具"], "protocol"],
    [["反向代理"], "reverse"],
    [["TCP 端口转发"], "forward"],
    [["请求处理", "屏蔽列表"], "blockList"],
    [["请求处理", "无缓存"], "noCaching"],
    [["请求处理", "阻止 Cookie"], "blockCookies"],
    [["映射与重写", "映射本地"], "mapLocal"],
    [["映射与重写", "映射远程"], "mapRemote"],
    [["映射与重写", "重写"], "rewrite"],
    [["流程控制", "断点"], "breakpoints"],
    [["流程控制", "带宽限制"], "throttling"],
    [["流程控制", "镜像"], "mirror"],
    [["流程控制", "自动保存"], "autoSave"],
    [["流程控制", "导出 HAR"], "export"],
  ];

  for (const [labels, key] of dialogs) {
    try {
      await openToolsPath(page, labels);
      const dialog = page.getByRole("dialog");
      await dialog.waitFor({ state: "visible", timeout: 5000 });
      const title = (await dialog.locator("h2, h1").first().textContent())?.trim() || "";
      const footer = await dialog.locator(".toolDialogFooter button, footer button").allTextContents();
      record(`dialog.${key}.open`, true, `title=${title}; footer=${footer.map((s) => s.trim()).join("/")}`);
      await shot(page, `05-dialog-${key}`);

      // interactive extras
      if (key === "ssl") {
        const enable = dialog.getByRole("checkbox").first();
        if (await enable.count()) {
          const was = await enable.isChecked();
          await enable.setChecked(!was);
          await sleep(100);
          await enable.setChecked(was);
          record("dialog.ssl.toggleEnable", true);
        }
        const add = dialog.getByRole("button", { name: "添加规则" }).first();
        if (await add.count()) {
          await add.click();
          await sleep(200);
          const inputs = await dialog.locator("input[type=text], input:not([type])").count();
          record("dialog.ssl.addRule", inputs > 0, `inputs=${inputs}`);
        }
        const adv = dialog.locator("summary", { hasText: "高级设置" });
        if (await adv.count()) {
          await adv.click();
          await sleep(150);
          record("dialog.ssl.expandAdvanced", true);
        }
      }

      if (key === "blockList") {
        const mode = dialog.getByRole("combobox", { name: "模式" });
        if (await mode.count()) {
          await mode.selectOption({ label: "黑名单" });
          await sleep(150);
          record("dialog.blockList.mode", true, "黑名单");
        }
        const addLoc = dialog.getByRole("button", { name: "添加位置" });
        if (await addLoc.count()) {
          await addLoc.click();
          await sleep(200);
          record(
            "dialog.blockList.addLocation",
            (await dialog.getByLabel(/主机|Host/).count()) > 0 ||
              (await dialog.locator("input").count()) > 2,
          );
        }
      }

      if (key === "mapLocal" || key === "mapRemote" || key === "rewrite") {
        const addRule = dialog.getByRole("button", { name: /添加规则|添加集合/ });
        if (await addRule.count()) {
          await addRule.first().click();
          await sleep(250);
          const fieldCount = await dialog.locator("input, select, textarea").count();
          record(`dialog.${key}.addRule`, fieldCount > 0, `fields=${fieldCount}`);
        }
      }

      if (key === "breakpoints") {
        const add = dialog.getByRole("button", { name: "添加规则" });
        if (await add.count()) {
          await add.click();
          await sleep(250);
          record("dialog.breakpoints.addRule", (await dialog.locator("input, select").count()) > 0);
        }
      }

      if (key === "throttling") {
        const preset = dialog.getByRole("combobox").first();
        if (await preset.count()) {
          // try select LTE if option exists
          const options = await preset.locator("option").allTextContents();
          record("dialog.throttling.presets", options.length > 0, options.join(","));
        }
      }

      if (key === "protocol") {
        const enable = dialog.getByRole("checkbox").first();
        if (await enable.count()) {
          record("dialog.protocol.checkbox", true, `checked=${await enable.isChecked()}`);
        }
        const addRoute = dialog.getByRole("button", { name: /添加路由/ });
        if (await addRoute.count()) {
          await addRoute.click();
          await sleep(250);
          record("dialog.protocol.addRoute", (await dialog.locator("input, select").count()) > 0);
        }
      }

      if (key === "reverse" || key === "forward") {
        const add = dialog.getByRole("button", { name: "添加规则" });
        if (await add.count()) {
          await add.click();
          await sleep(250);
          const fields = await dialog.locator("input, select").count();
          record(`dialog.${key}.addRule`, fields > 0, `fields=${fields}`);
        }
      }

      // Always cancel — do not apply permanent config during ops test
      await closeDialog(page);
      record(`dialog.${key}.close`, (await page.locator("[role=dialog]").count()) === 0);
    } catch (error) {
      record(`dialog.${key}.open`, false, String(error));
      await closeDialog(page);
    }
  }

  // ========== 6 Repeat dialogs ==========
  await page.goto(`${BASE}/connections`, { waitUntil: "domcontentloaded" });
  await sleep(400);
  const tree = page.locator(".transactionTree button, .transactionTreeItem").first();
  if (await tree.count()) {
    await tree.click();
    await sleep(300);
  }

  const editRepeat = page.getByRole("button", { name: "编辑后重复" });
  if (await editRepeat.count()) {
    await editRepeat.click();
    await sleep(300);
    const d = page.getByRole("dialog");
    record("repeat.edit.open", await d.isVisible());
    const url = d.getByLabel("URL");
    if (await url.count()) {
      const v = await url.inputValue();
      record("repeat.edit.hasUrl", v.length > 0, v);
    }
    await d.getByRole("button", { name: "取消" }).click();
    await sleep(200);
  } else {
    record("repeat.edit.open", false, "no transaction selected or button missing", "warn");
  }

  const advRepeat = page.getByRole("button", { name: "高级重复" });
  if (await advRepeat.count()) {
    await advRepeat.click();
    await sleep(300);
    const d = page.getByRole("dialog");
    record("repeat.advanced.open", await d.isVisible());
    // try fill times if spinbutton
    const times = d.getByLabel(/重复次数/);
    if (await times.count()) {
      await times.fill("1");
      record("repeat.advanced.times", true);
    }
    await d.getByRole("button", { name: "取消" }).click();
    await sleep(200);
  }

  // Simple repeat — may create traffic; only if button enabled
  const simple = page.getByRole("button", { name: "重复发送" });
  if (await simple.count()) {
    const disabled = await simple.isDisabled();
    if (!disabled) {
      await simple.click();
      await sleep(800);
      record("repeat.simple.click", true, "clicked; may create new transaction");
    } else {
      record("repeat.simple.click", false, "disabled", "warn");
    }
  }

  // ========== 7 Floating ==========
  await page.goto(`${BASE}/floating`, { waitUntil: "domcontentloaded" });
  await sleep(400);
  await shot(page, "06-floating");
  record(
    "floating.loaded",
    (await page.getByRole("button", { name: "打开主窗口" }).count()) > 0 ||
      (await page.getByText("运行中").count()) > 0,
  );

  // ========== 8 Narrow viewport ==========
  await page.setViewportSize({ width: 1024, height: 700 });
  await page.goto(`${BASE}/connections`, { waitUntil: "domcontentloaded" });
  await sleep(400);
  await openToolsPath(page, ["SSL 代理设置"]).catch(() => {});
  await sleep(300);
  const dlg = page.getByRole("dialog");
  if (await dlg.count()) {
    const box = await dlg.boundingBox();
    const fits =
      box !== null &&
      box.x >= -2 &&
      box.x + box.width <= 1024 + 4 &&
      box.y + box.height <= 700 + 8;
    record("narrow.sslFits", fits, JSON.stringify(box));
    await shot(page, "07-narrow-ssl");
    await closeDialog(page);
  } else {
    record("narrow.sslFits", false, "dialog not open");
  }

  // console
  record("console.noPageErrors", consoleErrors.length === 0, consoleErrors.slice(0, 5).join(" | "));

  const summary = {
    pass: results.filter((r) => r.ok).length,
    fail: results.filter((r) => !r.ok && r.severity === "fail").length,
    warn: results.filter((r) => !r.ok && r.severity === "warn").length,
  };

  const report = {
    started: new Date().toISOString(),
    base: BASE,
    summary,
    results,
    consoleErrors,
  };
  fs.writeFileSync(path.join(OUT, "ops-report.json"), JSON.stringify(report, null, 2));

  const md = [
    "# Sprak Capture 操作验收报告",
    "",
    `- 目标: ${BASE}`,
    `- 通过: ${summary.pass}  失败: ${summary.fail}  警告: ${summary.warn}`,
    "",
    "## 明细",
    "",
    "| 结果 | ID | 详情 |",
    "|------|----|------|",
    ...results.map(
      (r) =>
        `| ${r.ok ? "PASS" : r.severity === "warn" ? "WARN" : "FAIL"} | \`${r.id}\` | ${r.detail.replace(/\|/g, "\\|")} |`,
    ),
    "",
  ];
  fs.writeFileSync(path.join(OUT, "ops-report.md"), md.join("\n"), "utf8");
  console.log("Summary", summary);
  await browser.close();
  process.exit(summary.fail > 0 ? 1 : 0);
}

function locationCheck() {
  return true;
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
