/**
 * Sprak Capture UI acceptance audit against http://127.0.0.1:5173/
 * Screenshots + layout probes for every page/dialog reachable from the shell.
 */
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";

const BASE = process.env.CAPTURE_UI_URL || "http://127.0.0.1:5173";
const OUT = path.resolve("ui-audit");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

fs.mkdirSync(OUT, { recursive: true });

function layoutProbe(pageLabel) {
  return ({ pageLabel }) => {
    const issues = [];
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const candidates = document.querySelectorAll(
      "button, input, select, textarea, a, label, [role='dialog'], [role='menu'], [role='menuitem'], .dialogBackdrop, .toolSettingsDialog, .confirmDialog, .toolMenu, main, .toolbarActions, .settingsNav, .settingsForm",
    );
    for (const el of candidates) {
      const style = getComputedStyle(el);
      if (
        style.display === "none" ||
        style.visibility === "hidden" ||
        Number(style.opacity) === 0
      ) {
        continue;
      }
      const r = el.getBoundingClientRect();
      if (r.width <= 0 && r.height <= 0) continue;
      const text = (
        el.getAttribute("aria-label") ||
        el.getAttribute("title") ||
        el.textContent ||
        el.tagName
      )
        .replace(/\s+/g, " ")
        .trim()
        .slice(0, 48);
      if (r.right > vw + 4) {
        issues.push({
          kind: "overflow-x",
          text,
          right: Math.round(r.right),
          vw,
        });
      }
      if (r.left < -4) {
        issues.push({ kind: "negative-x", text, left: Math.round(r.left) });
      }
      if (r.top < -4 && el.closest("[role='dialog']")) {
        issues.push({ kind: "dialog-negative-y", text, top: Math.round(r.top) });
      }
      // tiny hit targets under 20x20 for interactive controls
      if (
        (el.tagName === "BUTTON" || el.getAttribute("role") === "menuitem") &&
        r.width > 0 &&
        r.height > 0 &&
        (r.width < 20 || r.height < 20)
      ) {
        issues.push({
          kind: "tiny-hit-target",
          text,
          w: Math.round(r.width),
          h: Math.round(r.height),
        });
      }
    }
    const dialogs = document.querySelectorAll("[role='dialog']");
    if (dialogs.length > 1) {
      issues.push({ kind: "multiple-dialogs", count: dialogs.length });
    }
    const dialog = document.querySelector("[role='dialog']");
    let dialogBox = null;
    if (dialog) {
      const r = dialog.getBoundingClientRect();
      dialogBox = {
        x: Math.round(r.x),
        y: Math.round(r.y),
        w: Math.round(r.width),
        h: Math.round(r.height),
        centeredX: Math.abs(r.x + r.width / 2 - vw / 2) < 40,
        fitsHeight: r.bottom <= vh + 8,
        fitsWidth: r.right <= vw + 8,
      };
      if (!dialogBox.fitsHeight) {
        issues.push({ kind: "dialog-taller-than-viewport", dialogBox });
      }
      if (!dialogBox.fitsWidth) {
        issues.push({ kind: "dialog-wider-than-viewport", dialogBox });
      }
    }
    // overlapping absolute toolbar icons text
    const toolbar = document.querySelector(".toolbarActions");
    let toolbarOverflow = false;
    if (toolbar) {
      const tr = toolbar.getBoundingClientRect();
      const mark = document.querySelector(".windowMark");
      const nav = document.querySelector(".mainNavigation");
      if (mark && nav) {
        const mr = mark.getBoundingClientRect();
        const nr = nav.getBoundingClientRect();
        if (nr.right > tr.left - 4) {
          toolbarOverflow = true;
          issues.push({
            kind: "toolbar-collision",
            navRight: Math.round(nr.right),
            actionsLeft: Math.round(tr.left),
          });
        }
        void mr;
      }
    }
    return {
      pageLabel,
      title: document.title,
      url: location.href,
      vw,
      vh,
      dialogBox,
      toolbarOverflow,
      dialogTitle:
        document
          .querySelector("[role='dialog'] h2, [role='dialog'] h1, [role='dialog'] header")
          ?.textContent?.replace(/\s+/g, " ")
          .trim() || null,
      h1:
        document.querySelector("main h1")?.textContent?.replace(/\s+/g, " ").trim() ||
        null,
      issues: issues.slice(0, 40),
      bodyTextSample: document.body?.innerText?.slice(0, 200) || "",
    };
  };
}

async function shot(page, name) {
  const file = path.join(OUT, `${name}.png`);
  await page.screenshot({ path: file, fullPage: true });
  return file;
}

async function closeDialogs(page) {
  for (let i = 0; i < 3; i++) {
    const dialog = page.locator("[role='dialog']");
    if ((await dialog.count()) === 0) break;
    // prefer explicit close/cancel
    const close = dialog
      .locator(
        "button:has-text('关闭'), button:has-text('取消'), button[aria-label*='关闭'], button[aria-label*='Close']",
      )
      .first();
    if (await close.count()) {
      await close.click({ force: true }).catch(() => {});
    } else {
      await page.keyboard.press("Escape");
    }
    await sleep(150);
  }
  // backdrop click last resort
  const backdrop = page.locator(".dialogBackdrop").first();
  if (await backdrop.count()) {
    await page.keyboard.press("Escape");
    await sleep(100);
  }
}

async function openToolsPath(page, labels) {
  await closeDialogs(page);
  await page.locator('button[aria-label*="工具"]').click();
  await sleep(120);
  for (const label of labels) {
    const item = page.locator('[role="menu"] [role="menuitem"]', {
      hasText: label,
    }).first();
    await item.waitFor({ state: "visible", timeout: 5000 });
    await item.click();
    await sleep(220);
  }
}

const report = {
  startedAt: new Date().toISOString(),
  base: BASE,
  surfaces: [],
  summary: { pass: 0, warn: 0, fail: 0 },
};

function pushSurface(entry) {
  const severity =
    entry.probe?.issues?.some((i) =>
      [
        "overflow-x",
        "dialog-taller-than-viewport",
        "dialog-wider-than-viewport",
        "toolbar-collision",
        "multiple-dialogs",
      ].includes(i.kind),
    )
      ? "fail"
      : entry.probe?.issues?.length
        ? "warn"
        : "pass";
  entry.severity = severity;
  report.surfaces.push(entry);
  report.summary[severity === "fail" ? "fail" : severity === "warn" ? "warn" : "pass"] +=
    1;
  console.log(`[${severity}] ${entry.id} — ${entry.title || entry.id}`);
  if (entry.probe?.issues?.length) {
    console.log("  issues:", JSON.stringify(entry.probe.issues, null, 0));
  }
}

async function captureSurface(page, id, title, extra = {}) {
  await sleep(180);
  const probe = await page.evaluate(layoutProbe(id), { pageLabel: id });
  const screenshot = await shot(page, id);
  pushSurface({ id, title, screenshot, probe, ...extra });
}

async function main() {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 1,
  });
  const page = await context.newPage();
  page.setDefaultTimeout(12_000);
  const consoleMessages = [];
  page.on("console", (msg) => {
    if (["error", "warning"].includes(msg.type())) {
      consoleMessages.push({ type: msg.type(), text: msg.text() });
    }
  });
  page.on("pageerror", (err) => {
    consoleMessages.push({ type: "pageerror", text: String(err) });
  });

  // --- Pages ---
  const pages = [
    ["01-overview", `${BASE}/overview`, "服务概览"],
    ["02-connections", `${BASE}/connections`, "连接会话"],
    ["03-settings-interface", `${BASE}/settings/interface`, "设置/界面语言"],
    ["04-settings-listener", `${BASE}/settings/listener`, "设置/监听"],
    ["05-settings-auth", `${BASE}/settings/authentication`, "设置/认证"],
    ["06-settings-capacity", `${BASE}/settings/capacity`, "设置/容量与超时"],
    ["08-floating", `${BASE}/floating`, "悬浮面板"],
  ];

  for (const [id, url, title] of pages) {
    await page.goto(url, { waitUntil: "networkidle" }).catch(async () => {
      await page.goto(url, { waitUntil: "domcontentloaded" });
    });
    await sleep(300);
    await captureSurface(page, id, title, { kind: "page", url });
  }

  // back to connections for dialogs
  await page.goto(`${BASE}/connections`, { waitUntil: "domcontentloaded" });
  await sleep(400);

  // tools root menu
  await page.locator('button[aria-label*="工具"]').click();
  await sleep(150);
  await captureSurface(page, "09-tools-menu-root", "工具菜单-根级", {
    kind: "menu",
  });
  await page.keyboard.press("Escape");
  await sleep(100);

  // dialogs via tools menu
  const dialogPaths = [
    ["10-dialog-ssl", ["SSL 代理设置"], "SSL 代理设置"],
    ["11-dialog-protocol", ["协议工具"], "协议工具"],
    ["12-dialog-reverse", ["反向代理"], "反向代理"],
    ["13-dialog-portforward", ["TCP 端口转发"], "TCP 端口转发"],
    ["14-dialog-blockList", ["请求处理", "屏蔽列表"], "屏蔽列表"],
    ["15-dialog-noCaching", ["请求处理", "禁用缓存"], "禁用缓存"],
    ["16-dialog-blockCookies", ["请求处理", "屏蔽 Cookie"], "屏蔽 Cookie"],
    ["17-dialog-mapLocal", ["映射与重写", "本地映射"], "本地映射"],
    ["18-dialog-mapRemote", ["映射与重写", "远程映射"], "远程映射"],
    ["19-dialog-rewrite", ["映射与重写", "重写"], "重写"],
    ["20-dialog-breakpoints", ["流程控制", "断点"], "断点"],
    ["21-dialog-throttling", ["流程控制", "带宽限制"], "带宽限制"],
    ["22-dialog-mirror", ["流程控制", "镜像"], "镜像"],
    ["23-dialog-autoSave", ["流程控制", "自动保存"], "自动保存"],
    ["24-dialog-export", ["流程控制", "导出"], "导出"],
  ];

  // Fallback English-ish labels if locale differs - collect available labels first
  await page.locator('button[aria-label*="工具"]').click();
  await sleep(120);
  const rootLabels = await page
    .locator('[role="menu"] [role="menuitem"]')
    .allTextContents();
  await page.keyboard.press("Escape");
  report.menuRootLabels = rootLabels.map((s) => s.trim());

  for (const [id, labels, title] of dialogPaths) {
    try {
      await openToolsPath(page, labels);
      // if branch only, may need second open - check dialog
      const hasDialog = (await page.locator("[role='dialog']").count()) > 0;
      if (!hasDialog) {
        // try partial match reopen with looser labels
        await closeDialogs(page);
        await page.locator('button[aria-label*="工具"]').click();
        await sleep(120);
        for (const label of labels) {
          const items = page.locator('[role="menu"] [role="menuitem"]');
          const count = await items.count();
          let clicked = false;
          for (let i = 0; i < count; i++) {
            const text = (await items.nth(i).innerText()).trim();
            if (text.includes(label) || label.includes(text)) {
              await items.nth(i).click();
              clicked = true;
              await sleep(200);
              break;
            }
          }
          if (!clicked) {
            throw new Error(`menu item not found: ${label}; available later`);
          }
        }
      }
      await captureSurface(page, id, title, { kind: "dialog", path: labels });
      await closeDialogs(page);
    } catch (error) {
      pushSurface({
        id,
        title,
        kind: "dialog",
        path: labels,
        severity: "fail",
        error: String(error),
      });
      report.summary.fail += 1;
      await closeDialogs(page);
      await page.keyboard.press("Escape");
    }
  }

  // Confirm clear dialog
  try {
    await closeDialogs(page);
    await page.locator('button[aria-label*="清空"]').click();
    await sleep(200);
    await captureSurface(page, "25-dialog-clear-confirm", "清空事务确认", {
      kind: "dialog",
    });
    await closeDialogs(page);
  } catch (error) {
    pushSurface({
      id: "25-dialog-clear-confirm",
      title: "清空事务确认",
      severity: "fail",
      error: String(error),
    });
    report.summary.fail += 1;
  }

  // Connections inspector tabs
  await page.goto(`${BASE}/connections`, { waitUntil: "domcontentloaded" });
  await sleep(400);
  const tabs = ["概览", "请求", "响应", "Protobuf", "备注"];
  for (const tab of tabs) {
    try {
      const tabEl = page.locator('[role="tab"]', { hasText: tab }).first();
      if (await tabEl.count()) {
        await tabEl.click();
        await sleep(150);
        await captureSurface(
          page,
          `26-inspector-tab-${tab}`,
          `检查器 Tab: ${tab}`,
          { kind: "tab" },
        );
      }
    } catch (error) {
      pushSurface({
        id: `26-inspector-tab-${tab}`,
        title: `检查器 Tab: ${tab}`,
        severity: "fail",
        error: String(error),
      });
      report.summary.fail += 1;
    }
  }

  // Sequence / structure views if present
  for (const label of ["结构", "序列", "Structure", "Sequence"]) {
    const btn = page.getByRole("button", { name: label }).first();
    if (await btn.count()) {
      await btn.click().catch(() => {});
      await sleep(150);
    }
    const tabLike = page.locator(`text=${label}`).first();
    if (await tabLike.count()) {
      await tabLike.click().catch(() => {});
      await sleep(150);
    }
  }
  await captureSurface(page, "27-connections-nav-modes", "连接会话导航模式", {
    kind: "page",
  });

  // Narrow viewport stress
  await page.setViewportSize({ width: 1024, height: 720 });
  await page.goto(`${BASE}/connections`, { waitUntil: "domcontentloaded" });
  await sleep(300);
  await captureSurface(page, "28-narrow-connections-1024", "窄屏 1024x720 连接会话", {
    kind: "responsive",
  });
  await openToolsPath(page, ["SSL 代理设置"]).catch(() => {});
  await captureSurface(page, "29-narrow-ssl-dialog", "窄屏 SSL 对话框", {
    kind: "responsive-dialog",
  });
  await closeDialogs(page);

  await page.setViewportSize({ width: 1440, height: 900 });

  // Submenus only screenshots
  await page.goto(`${BASE}/connections`, { waitUntil: "domcontentloaded" });
  for (const [id, label] of [
    ["30-menu-interception", "请求处理"],
    ["31-menu-mapping", "映射与重写"],
    ["32-menu-control", "流程控制"],
  ]) {
    await page.locator('button[aria-label*="工具"]').click();
    await sleep(120);
    await page
      .locator('[role="menu"] [role="menuitem"]', { hasText: label })
      .first()
      .click();
    await sleep(150);
    await captureSurface(page, id, `工具二级菜单: ${label}`, { kind: "menu" });
    await page.keyboard.press("Escape");
    await sleep(80);
  }

  report.consoleMessages = consoleMessages;
  report.finishedAt = new Date().toISOString();
  const reportPath = path.join(OUT, "report.json");
  fs.writeFileSync(reportPath, JSON.stringify(report, null, 2), "utf8");

  // Markdown summary
  const md = [];
  md.push("# Sprak Capture UI 验收报告（自动巡检）");
  md.push("");
  md.push(`- 目标: ${BASE}`);
  md.push(`- 开始: ${report.startedAt}`);
  md.push(`- 结束: ${report.finishedAt}`);
  md.push(
    `- 汇总: pass=${report.summary.pass} warn=${report.summary.warn} fail=${report.summary.fail}`,
  );
  md.push(`- 工具根菜单项: ${(report.menuRootLabels || []).join(" | ")}`);
  md.push("");
  md.push("## 各界面");
  md.push("");
  for (const s of report.surfaces) {
    md.push(`### ${s.id} — ${s.title} [${s.severity}]`);
    if (s.url) md.push(`- URL: ${s.url}`);
    if (s.path) md.push(`- 菜单路径: ${s.path.join(" → ")}`);
    if (s.screenshot) md.push(`- 截图: \`${path.relative(process.cwd(), s.screenshot)}\``);
    if (s.error) md.push(`- 错误: ${s.error}`);
    if (s.probe?.h1) md.push(`- H1: ${s.probe.h1}`);
    if (s.probe?.dialogTitle) md.push(`- 对话框标题: ${s.probe.dialogTitle}`);
    if (s.probe?.dialogBox)
      md.push(`- 对话框几何: ${JSON.stringify(s.probe.dialogBox)}`);
    if (s.probe?.issues?.length) {
      md.push("- 布局问题:");
      for (const issue of s.probe.issues) {
        md.push(`  - \`${issue.kind}\`: ${JSON.stringify(issue)}`);
      }
    } else if (!s.error) {
      md.push("- 布局探针: 无明显溢出/碰撞");
    }
    md.push("");
  }
  if (consoleMessages.length) {
    md.push("## 控制台");
    for (const m of consoleMessages) {
      md.push(`- **${m.type}**: ${m.text}`);
    }
  } else {
    md.push("## 控制台");
    md.push("- 无 error/warning");
  }
  fs.writeFileSync(path.join(OUT, "report.md"), md.join("\n"), "utf8");
  console.log("Wrote", reportPath);
  await browser.close();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
