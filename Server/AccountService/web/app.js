    "use strict";
    const overviewRefreshMilliseconds = 2_000;
    const state = { accounts: [], statistics: null, connections: [], connectionAccountId: null, editingAccount: null, passwordAccount: null, selectedAccountIds: new Set(), visibleAccountIds: [], ruleSets: [], editingRuleSet: null, selectedRuleSetIds: new Set(), overviewTimer: null, contextTimer: null, contextActive: false, activeTab: "overview", contextInstanceId: crypto.randomUUID(), contextSequence: 0, packageSnapshot: null };
  const byId = (id) => document.getElementById(id);
  const apiBasePath = new URL("api/v1/", document.baseURI).pathname.replace(/\/$/, "");
  const controlApiBasePath = new URL("../api/v1/", document.baseURI).pathname.replace(/\/$/, "");

    /**
     * 调用同源管理接口并统一解析稳定错误体；204 响应不尝试解析 JSON。
     * 运行上下文：Cookie 会话由浏览器自动附带，任何失败都交给当前操作区域展示。
     * 失败语义：网络错误或非成功状态均抛出 Error，不把失败响应当作空数据继续渲染。
     */
    async function request(path, options = {}) {
      const response = await fetch(path.startsWith("/api/v1") ? `${apiBasePath}${path.slice(7)}` : path, { credentials: "same-origin", ...options, headers: { "Content-Type": "application/json", ...(options.headers || {}) } });
      if (!response.ok) {
        const problem = await response.json().catch(() => ({}));
        throw new Error(problem.message || problem.code || `请求失败：${response.status}`);
      }
      return response.status === 204 ? null : response.json();
    }

    /** 调用父级 Sprak Capture 控制 API；账号管理与打包共享同一持久管理员会话，不引入第二套认证入口。 */
    async function requestControl(path, options = {}) {
      const response = await fetch(`${controlApiBasePath}${path}`, { credentials: "same-origin", ...options, headers: { "Content-Type": "application/json", ...(options.headers || {}) } });
      if (!response.ok) {
        const problem = await response.json().catch(() => ({}));
        throw new Error(problem.message || problem.code || `请求失败：${response.status}`);
      }
      return response.status === 204 ? null : response.json();
    }

    /**
     * 上报账号管理窗口当前页签和选中资源；只发送稳定 ID，不复制账号、规则正文或表单草稿。
     * 运行上下文：登录后的页签、选择、焦点变化与五秒心跳调用；失败仅记录一次开发诊断并等待下次心跳恢复。
     */
    async function reportUiContext() {
      state.contextSequence += 1;
      let selection = null;
      if (state.activeTab === "accounts" && state.selectedAccountIds.size > 0) selection = { kind: "account", ids: [...state.selectedAccountIds], side: null, sequence: null };
      if (state.activeTab === "ruleSets" && state.selectedRuleSetIds.size > 0) selection = { kind: "ruleSet", ids: [...state.selectedRuleSetIds], side: null, sequence: null };
      if (state.activeTab === "overview" && state.connectionAccountId) selection = { kind: "account", ids: [state.connectionAccountId], side: null, sequence: null };
      await requestControl("/ui/context", {
        method: "PUT",
        body: JSON.stringify({
          instanceId: state.contextInstanceId,
          sequence: state.contextSequence,
          windowKind: "independent",
          page: "accountManagement",
          section: state.activeTab,
          view: null,
          selection,
          focused: document.hasFocus(),
          visible: document.visibilityState === "visible",
        }),
      });
    }

    /** 触发界面上下文同步；网络失败不覆盖账号管理页面的业务错误区域。 */
    function scheduleUiContextReport() {
      if (!state.contextActive) return;
      reportUiContext().catch((error) => console.warn("同步当前界面上下文失败", error));
    }

    /** 创建只使用 textContent 的元素，避免账号和备注等业务文本进入 HTML 解释上下文。 */
    function element(tagName, text, className) {
      const node = document.createElement(tagName);
      if (text !== undefined && text !== null) node.textContent = String(text);
      if (className) node.className = className;
      return node;
    }

    /** 格式化字节数，始终使用二进制单位并保留便于阅读的有效位。 */
    function formatBytes(value) {
      const bytes = Number(value) || 0;
      const units = ["B", "KiB", "MiB", "GiB", "TiB"];
      let amount = Math.abs(bytes);
      let unitIndex = 0;
      while (amount >= 1024 && unitIndex < units.length - 1) { amount /= 1024; unitIndex += 1; }
      const signed = bytes < 0 ? -amount : amount;
      return `${signed.toLocaleString("zh-CN", { maximumFractionDigits: unitIndex === 0 ? 0 : 2 })} ${units[unitIndex]}`;
    }

    /** 为实时带宽补充时间单位，避免与同页面的累计连接流量混淆。 */
    function formatRate(value) {
      return `${formatBytes(value)}/s`;
    }

    /** 格式化 APK 文件大小；小于 1 MiB 使用 KiB，其余使用 MiB，避免历史列表显示冗长原始字节数。 */
    function formatPackageSize(value) {
      const bytes = Number(value) || 0;
      return bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)} KiB` : `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
    }

    /** 将特殊限制值呈现为业务语义，防止界面把 -1 误读为真实额度。 */
    function formatLimit(value, suffix = "") {
      if (value === -1) return "不限";
      if (value === 0) return "禁用";
      return `${Number(value).toLocaleString("zh-CN")}${suffix}`;
    }

    /** 把 UTC 毫秒时间戳转换为本机可读时间；负一和零保留账号策略语义。 */
    function formatTime(value) {
      if (value === -1) return "永不过期";
      if (value === 0) return "已禁用";
      return new Date(value).toLocaleString("zh-CN", { hour12: false });
    }

    /** 根据零值和到期时间计算稳定状态键；筛选与徽标共用该判定，避免同一账号显示和过滤结果不一致。 */
    function accountStatusKey(account) {
      const policy = account.policy;
      if ([policy.maxUploadBytesPerSecond, policy.maxDownloadBytesPerSecond, policy.maxConnections, policy.maxOnlineIps, policy.expiresAt].includes(0)) return "disabled";
      if (policy.expiresAt > 0 && policy.expiresAt < Date.now()) return "expired";
      return "available";
    }

    /** 把稳定状态键转换为展示文本和样式，服务端仍是最终认证权威。 */
    function accountStatus(account) {
      return { disabled: ["已禁用", "off"], expired: ["已过期", "warn"], available: ["可用", "ok"] }[accountStatusKey(account)];
    }

    /** 清空并显示全局提示；下一次操作会覆盖旧提示，避免同时显示互相冲突的状态。 */
    function showMessage(message = "", error = "") {
      byId("pageNotice").textContent = message;
      byId("pageError").textContent = error;
    }

    /** 渲染概览实时快照；每秒速率来自相邻数据面同步差值，不使用累计流量冒充实时值。 */
    function renderSummary() {
      const summary = state.statistics;
      if (!summary) return;
      const values = [
        ["账号总数", summary.totalAccounts], ["在线账号", summary.onlineAccounts], ["在线 IP", summary.onlineIps],
        ["连接数", summary.activeConnections], ["实时上行", formatRate(summary.uploadBytesPerSecond)], ["实时下行", formatRate(summary.downloadBytesPerSecond)],
      ];
      byId("summaryCards").replaceChildren(...values.map(([name, value]) => { const card = element("div", null, "panel card"); card.append(element("span", name), element("strong", value)); return card; }));
    }

    /** 构造账号行，选择框只保存稳定 accountId，业务值全部通过文本节点进入页面。 */
    function accountRow(account) {
      const row = document.createElement("tr");
      const selectionCell = document.createElement("td");
      selectionCell.className = "selectionCell";
      const selection = document.createElement("input");
      selection.type = "checkbox";
      selection.dataset.selectAccountId = account.accountId;
      selection.checked = state.selectedAccountIds.has(account.accountId);
      selection.setAttribute("aria-label", `选择账号 ${account.username}`);
      selectionCell.append(selection);
      const nameCell = document.createElement("td");
      nameCell.append(element("strong", account.username), element("div", account.remark || "", "muted"));
      const [statusName, statusClass] = accountStatus(account);
      const statusCell = document.createElement("td");
      statusCell.append(element("span", statusName, `badge ${statusClass}`));
      const bandwidth = `↑ ${formatLimit(account.policy.maxUploadBytesPerSecond, " B/s")} · ↓ ${formatLimit(account.policy.maxDownloadBytesPerSecond, " B/s")}`;
      const actionCell = document.createElement("td");
      actionCell.className = "rowActions";
      for (const [label, action, className] of [["编辑", "edit", "ghost"], ["密码", "password", "ghost"], ["连接", "connections", "ghost"]]) {
        const button = element("button", label, className); button.type = "button"; button.dataset.action = action; button.dataset.accountId = account.accountId; actionCell.append(button);
      }
      for (const cell of [selectionCell, nameCell, statusCell, element("td", account.passwordMode === "any" ? "任意非空" : "固定"), element("td", `${account.onlineIps} / ${formatLimit(account.policy.maxOnlineIps)}`), element("td", `${account.activeConnections} / ${formatLimit(account.policy.maxConnections)}`), element("td", bandwidth), element("td", formatTime(account.policy.expiresAt)), actionCell]) row.append(cell);
      return row;
    }

    /** 同步批量操作栏和表头选择态；按钮只在至少选择一个账号后出现。 */
    function renderSelectionActions() {
      const selectedCount = state.selectedAccountIds.size;
      byId("batchActions").hidden = selectedCount === 0;
      byId("selectionCount").textContent = `已选择 ${selectedCount} 个账号`;
      const visibleSelected = state.visibleAccountIds.filter((accountId) => state.selectedAccountIds.has(accountId)).length;
      const selectVisible = byId("selectVisibleAccounts");
      selectVisible.checked = state.visibleAccountIds.length > 0 && visibleSelected === state.visibleAccountIds.length;
      selectVisible.indeterminate = visibleSelected > 0 && visibleSelected < state.visibleAccountIds.length;
    }

    /** 判断账号是否符合到期筛选；禁用状态和到期类型保持正交，管理员可组合定位账号。 */
    function matchesExpiryFilter(account, filter) {
      if (filter === "all") return true;
      if (filter === "never") return account.policy.expiresAt === -1;
      if (filter === "expired") return account.policy.expiresAt > 0 && account.policy.expiresAt < Date.now();
      return account.policy.expiresAt > Date.now();
    }

    /** 为到期时间排序生成稳定数值；永不过期和禁用排在所有明确时间之后。 */
    function sortableExpiry(account) {
      return account.policy.expiresAt > 0 ? account.policy.expiresAt : Number.MAX_SAFE_INTEGER;
    }

    /** 按页面控件过滤并排序完整内存快照；服务端当前只提供分页，因此不会伪造远端查询语义。 */
    function renderAccounts() {
      const query = byId("accountSearch").value.trim().toLocaleLowerCase("zh-CN");
      const statusFilter = byId("accountStatusFilter").value;
      const expiryFilter = byId("accountExpiryFilter").value;
      const sort = byId("accountSort").value;
      const accounts = state.accounts
        .filter((account) => !query || `${account.username}\n${account.remark || ""}`.toLocaleLowerCase("zh-CN").includes(query))
        .filter((account) => statusFilter === "all" || accountStatusKey(account) === statusFilter)
        .filter((account) => matchesExpiryFilter(account, expiryFilter))
        .sort((left, right) => {
          if (sort === "createdAsc") return left.createdAt - right.createdAt || left.accountId.localeCompare(right.accountId);
          if (sort === "usernameAsc") return left.username.localeCompare(right.username, "zh-CN") || left.accountId.localeCompare(right.accountId);
          if (sort === "usernameDesc") return right.username.localeCompare(left.username, "zh-CN") || left.accountId.localeCompare(right.accountId);
          if (sort === "expiryAsc") return sortableExpiry(left) - sortableExpiry(right) || left.accountId.localeCompare(right.accountId);
          if (sort === "trafficDesc") return (right.uploadedBytes + right.downloadedBytes) - (left.uploadedBytes + left.downloadedBytes) || left.accountId.localeCompare(right.accountId);
          if (sort === "connectionsDesc") return right.activeConnections - left.activeConnections || left.accountId.localeCompare(right.accountId);
          return right.createdAt - left.createdAt || left.accountId.localeCompare(right.accountId);
        });
      state.visibleAccountIds = accounts.map((account) => account.accountId);
      byId("accountRows").replaceChildren(...accounts.map(accountRow));
      byId("accountEmpty").hidden = accounts.length !== 0;
      renderSelectionActions();
    }

    /** 分页读取全部账号；每页遵守服务端 200 条上限，避免账号数增长后页面静默遗漏。 */
    async function loadAllAccounts() {
      const accounts = [];
      const pageSize = 200;
      for (let offset = 0; ; offset += pageSize) {
        const page = await request(`/api/v1/accounts?offset=${offset}&limit=${pageSize}`);
        accounts.push(...page);
        if (page.length < pageSize) return accounts;
      }
    }

    /** 原子替换账号快照；请求失败时保留上一次完整列表。 */
    async function loadAccounts() {
      const accounts = await loadAllAccounts();
      state.accounts = accounts;
      const currentAccountIds = new Set(accounts.map((account) => account.accountId));
      for (const accountId of state.selectedAccountIds) if (!currentAccountIds.has(accountId)) state.selectedAccountIds.delete(accountId);
      renderAccounts();
    }

    /** 刷新概览统计和连接明细；账号筛选只收窄明细，不改变全局统计卡片。 */
    async function loadConnections() {
      const account = state.accounts.find((candidate) => candidate.accountId === state.connectionAccountId);
      const path = account ? `/api/v1/accounts/${encodeURIComponent(account.accountId)}/connections` : "/api/v1/connections";
      const [statistics, connections] = await Promise.all([request("/api/v1/statistics"), request(path)]);
      state.statistics = statistics;
      state.connections = connections;
      byId("connectionTitle").textContent = account ? `账号“${account.username}”的当前连接` : "当前连接";
      const names = new Map(state.accounts.map((account) => [account.accountId, account.username]));
      byId("connectionRows").replaceChildren(...state.connections.map((connection) => { const row = document.createElement("tr"); for (const value of [names.get(connection.accountId) || connection.accountId, connection.sourceIp, connection.connectionId, formatTime(connection.createdAt), formatTime(connection.lastHeartbeatAt), formatRate(connection.uploadBytesPerSecond), formatRate(connection.downloadBytesPerSecond), formatBytes(connection.uploadedBytes + connection.downloadedBytes), connection.revoked ? "撤销中" : "在线"]) row.append(element("td", value)); return row; }));
      byId("connectionEmpty").hidden = state.connections.length !== 0;
      renderSummary();
    }

    /** 构造规则集表格行；开关按钮只提交目标状态，服务端事务负责关闭其它启用项。 */
    function ruleSetRow(ruleSet) {
      const row = document.createElement("tr");
      const selectionCell = document.createElement("td");
      selectionCell.className = "selectionCell";
      const selection = document.createElement("input");
      selection.type = "checkbox";
      selection.dataset.selectRuleSetId = ruleSet.ruleSetId;
      selection.checked = state.selectedRuleSetIds.has(ruleSet.ruleSetId);
      selection.setAttribute("aria-label", `选择规则集 ${ruleSet.name}`);
      selectionCell.append(selection);
      const actionCell = document.createElement("td");
      actionCell.className = "rowActions";
      for (const [label, action, className] of [["编辑", "edit", "ghost"], [ruleSet.enabled ? "关闭" : "开启", "toggle", ruleSet.enabled ? "secondary" : "ghost"], ["删除", "delete", "ghost dangerText"]]) {
        const button = element("button", label, className);
        button.type = "button";
        button.dataset.ruleSetAction = action;
        button.dataset.ruleSetId = ruleSet.ruleSetId;
        if (action === "toggle") {
          button.classList.add("switchControl");
          button.setAttribute("role", "switch");
          button.setAttribute("aria-checked", String(ruleSet.enabled));
        }
        actionCell.append(button);
      }
      const statusCell = document.createElement("td");
      statusCell.append(element("span", ruleSet.enabled ? "已启用" : "未启用", `badge ${ruleSet.enabled ? "ok" : "off"}`));
      for (const cell of [selectionCell, element("td", ruleSet.name), statusCell, element("td", ruleSet.revision), element("td", formatTime(ruleSet.updatedAt)), actionCell]) row.append(cell);
      return row;
    }

    /** 渲染规则集快照和多选栏；所有正文只在编辑对话框内按 value 写入，不进入 HTML 解析上下文。 */
    function renderRuleSets() {
      byId("ruleSetRows").replaceChildren(...state.ruleSets.map(ruleSetRow));
      byId("ruleSetEmpty").hidden = state.ruleSets.length !== 0;
      const selectedCount = state.selectedRuleSetIds.size;
      byId("ruleSetBatchActions").hidden = selectedCount === 0;
      byId("ruleSetSelectionCount").textContent = `已选择 ${selectedCount} 个规则集`;
      const selectAll = byId("selectAllRuleSets");
      selectAll.checked = state.ruleSets.length > 0 && selectedCount === state.ruleSets.length;
      selectAll.indeterminate = selectedCount > 0 && selectedCount < state.ruleSets.length;
    }

    /** 读取完整规则集列表并清理已经删除的选择项；失败时保留上一份可见快照。 */
    async function loadRuleSets() {
      const ruleSets = await request("/api/v1/ruleSets");
      state.ruleSets = ruleSets;
      const currentIds = new Set(ruleSets.map((ruleSet) => ruleSet.ruleSetId));
      for (const ruleSetId of state.selectedRuleSetIds) if (!currentIds.has(ruleSetId)) state.selectedRuleSetIds.delete(ruleSetId);
      renderRuleSets();
    }

    /**
     * 打开新建或编辑对话框；参数是可选规则集快照，为空时填入可直接下发的完整模板。
     * 运行上下文：管理页本地交互，默认 DNS 明确直连指定 IP，VPN 与 Root 共用 FINAL,PROXY。
     * 失败语义：本函数不访问网络；服务端语法错误由提交流程写入 ruleSetFormError。
     */
    function openRuleSetDialog(ruleSet = null) {
      state.editingRuleSet = ruleSet;
      byId("ruleSetDialogTitle").textContent = ruleSet ? `编辑规则集：${ruleSet.name}` : "新建规则集";
      byId("ruleSetName").value = ruleSet?.name || "";
      byId("ruleSetContent").value = ruleSet?.content || "[DNS]\nPRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\n\n[RoutingRule]\n\n[GRoutingRule]\nFINAL,PROXY\n\n[proxy_app]\n";
      byId("ruleSetFormError").textContent = "";
      byId("ruleSetDialog").showModal();
    }

    /** 创建或按 revision 保存规则集；开关状态始终由列表独立操作，编辑不会意外改变线上启用项。 */
    async function saveRuleSet(event) {
      event.preventDefault();
      const body = { name: byId("ruleSetName").value, content: byId("ruleSetContent").value };
      try {
        if (state.editingRuleSet) {
          body.revision = state.editingRuleSet.revision;
          await request(`/api/v1/ruleSets/${encodeURIComponent(state.editingRuleSet.ruleSetId)}`, { method: "PUT", body: JSON.stringify(body) });
        } else {
          await request("/api/v1/ruleSets", { method: "POST", body: JSON.stringify({ ...body, enabled: false }) });
        }
        byId("ruleSetDialog").close();
        await loadRuleSets();
        showMessage("规则集已保存。");
      } catch (error) { byId("ruleSetFormError").textContent = error.message; }
    }

    /** 处理编辑、互斥开关和单项删除；每次成功后重读完整列表以接收其它项的新修订号。 */
    async function ruleSetAction(event) {
      const button = event.target.closest("button[data-rule-set-action]");
      if (!button) return;
      const ruleSet = state.ruleSets.find((candidate) => candidate.ruleSetId === button.dataset.ruleSetId);
      if (!ruleSet) return;
      try {
        if (button.dataset.ruleSetAction === "edit") return openRuleSetDialog(ruleSet);
        if (button.dataset.ruleSetAction === "toggle") {
          await request(`/api/v1/ruleSets/${encodeURIComponent(ruleSet.ruleSetId)}/enabled`, { method: "PUT", body: JSON.stringify({ revision: ruleSet.revision, enabled: !ruleSet.enabled }) });
        } else if (button.dataset.ruleSetAction === "delete") {
          if (!confirm(`确认永久删除规则集“${ruleSet.name}”？`)) return;
          await request(`/api/v1/ruleSets/${encodeURIComponent(ruleSet.ruleSetId)}`, { method: "DELETE" });
          state.selectedRuleSetIds.delete(ruleSet.ruleSetId);
        }
        await loadRuleSets();
      } catch (error) { showMessage("", error.message); }
    }

    /** 维护规则集稳定 ID 选择集合；删除按钮只有在选择非空时出现。 */
    function updateRuleSetSelection(event) {
      const selection = event.target.closest("input[data-select-rule-set-id]");
      if (!selection) return;
      if (selection.checked) state.selectedRuleSetIds.add(selection.dataset.selectRuleSetId);
      else state.selectedRuleSetIds.delete(selection.dataset.selectRuleSetId);
      renderRuleSets();
      scheduleUiContextReport();
    }

    /** 全选或清空当前规则集列表；列表不分页，因此复选框状态与完整快照一致。 */
    function toggleAllRuleSets(event) {
      state.selectedRuleSetIds.clear();
      if (event.target.checked) for (const ruleSet of state.ruleSets) state.selectedRuleSetIds.add(ruleSet.ruleSetId);
      renderRuleSets();
      scheduleUiContextReport();
    }

    /** 在服务端单事务内删除全部选中规则集；确认前不会发出任何写请求。 */
    async function deleteSelectedRuleSets() {
      const ruleSetIds = [...state.selectedRuleSetIds];
      if (ruleSetIds.length === 0 || !confirm(`确认永久删除已选择的 ${ruleSetIds.length} 个规则集？`)) return;
      try {
        const result = await request("/api/v1/ruleSets/batch", { method: "DELETE", body: JSON.stringify({ ruleSetIds }) });
        state.selectedRuleSetIds.clear();
        await loadRuleSets();
        showMessage(`已删除 ${result.deletedRuleSets} 个规则集。`);
      } catch (error) { showMessage("", error.message); }
    }

    /** 渲染安装包生成记录和独立下载 URL；记录不提供含凭据 APK 的重复下载入口。 */
    function renderClientPackages() {
      const snapshot = state.packageSnapshot;
      const downloadUrl = new URL("/client", window.location.origin).href;
      byId("clientDownloadUrl").textContent = downloadUrl;
      byId("clientDownloadLink").href = downloadUrl;
      byId("packageError").textContent = "";
      const packages = snapshot?.packages || [];
      byId("packageRows").replaceChildren(...packages.map((artifact) => {
        const row = document.createElement("tr");
        const digest = element("code", artifact.sha256, "packageDigest");
        const digestCell = document.createElement("td"); digestCell.append(digest);
        for (const cell of [element("td", artifact.applicationName), element("td", artifact.applicationId), element("td", formatPackageSize(artifact.sizeBytes)), digestCell]) row.append(cell);
        return row;
      }));
      byId("packageEmpty").hidden = packages.length !== 0;
    }

    /** 读取控制服务的客户端任务快照；失败保留上一次完整结果，不用空列表覆盖有效历史。 */
    async function loadClientPackages() {
      const snapshot = await requestControl("/clientPackages");
      state.packageSnapshot = snapshot;
      renderClientPackages();
    }

    /** 切换四块主区域；每个页面进入时只读取自身所需快照，避免隐藏页面持续产生请求。 */
    async function activateTab(name, accountId = null) {
      state.activeTab = name;
      state.connectionAccountId = name === "overview" ? accountId : null;
      document.querySelectorAll("[data-tab]").forEach((button) => button.setAttribute("aria-selected", String(button.dataset.tab === name)));
      document.querySelectorAll("[data-section]").forEach((section) => { section.hidden = section.dataset.section !== name; });
      try { if (name === "overview") await loadConnections(); if (name === "accounts") await loadAccounts(); if (name === "ruleSets") await loadRuleSets(); if (name === "packaging") await loadClientPackages(); showMessage(); } catch (error) { showMessage("", error.message); }
      scheduleUiContextReport();
    }

    /** 将账号策略填入编辑框；编辑时账号名和密码模式由各自专用端点维护。 */
    function openAccountDialog(account = null) {
      state.editingAccount = account;
      byId("accountDialogTitle").textContent = account ? `编辑账号：${account.username}` : "新建账号";
      byId("accountUsername").value = account?.username || ""; byId("accountUsername").disabled = Boolean(account);
      document.querySelectorAll('input[name="passwordMode"]').forEach((radio) => { radio.checked = radio.value === (account?.passwordMode || "any"); radio.disabled = Boolean(account); });
      byId("fixedPasswordLabel").hidden = Boolean(account) || (account?.passwordMode || "any") !== "fixed";
      byId("accountPassword").value = "";
      const policy = account?.policy || { maxUploadBytesPerSecond: -1, maxDownloadBytesPerSecond: -1, maxConnections: -1, maxOnlineIps: -1, expiresAt: -1 };
      byId("maxUpload").value = policy.maxUploadBytesPerSecond; byId("maxDownload").value = policy.maxDownloadBytesPerSecond; byId("maxConnections").value = policy.maxConnections; byId("maxOnlineIps").value = policy.maxOnlineIps;
      byId("accountDisabled").checked = [policy.maxUploadBytesPerSecond, policy.maxDownloadBytesPerSecond, policy.maxConnections, policy.maxOnlineIps, policy.expiresAt].includes(0);
      byId("expiresAt").value = policy.expiresAt > 0 ? new Date(policy.expiresAt - new Date().getTimezoneOffset() * 60000).toISOString().slice(0, 16) : "";
      byId("accountRemark").value = account?.remark || ""; byId("accountFormError").textContent = ""; byId("accountDialog").showModal();
    }

    /** 从表单生成严格三态策略；禁用统一提交全部零值，避免界面显示与认证判定不一致。 */
    function readPolicyForm() {
      if (byId("accountDisabled").checked) return { maxUploadBytesPerSecond: 0, maxDownloadBytesPerSecond: 0, maxConnections: 0, maxOnlineIps: 0, expiresAt: 0 };
      const expires = byId("expiresAt").value;
      return { maxUploadBytesPerSecond: Number(byId("maxUpload").value), maxDownloadBytesPerSecond: Number(byId("maxDownload").value), maxConnections: Number(byId("maxConnections").value), maxOnlineIps: Number(byId("maxOnlineIps").value), expiresAt: expires ? new Date(expires).getTime() : -1 };
    }

    /** 创建或更新账号；更新携带当前 policyRevision，冲突交由服务端返回 409。 */
    async function saveAccount(event) {
      event.preventDefault();
      try {
        const policy = readPolicyForm(); const remark = byId("accountRemark").value || null;
        if (state.editingAccount) await request(`/api/v1/accounts/${encodeURIComponent(state.editingAccount.accountId)}`, { method: "PATCH", body: JSON.stringify({ policyRevision: state.editingAccount.policyRevision, ...policy, remark }) });
        else { const passwordMode = document.querySelector('input[name="passwordMode"]:checked').value; const password = passwordMode === "fixed" ? byId("accountPassword").value : null; if (passwordMode === "fixed" && !password) throw new Error("固定密码不能为空。"); await request("/api/v1/accounts", { method: "POST", body: JSON.stringify({ username: byId("accountUsername").value, password, ...policy, remark }) }); }
        byId("accountDialog").close(); await loadAccounts(); showMessage("账号已保存。");
      } catch (error) { byId("accountFormError").textContent = error.message; }
    }

    /** 修改密码模式后服务端会撤销旧租约；界面不保留提交过的密码。 */
    async function savePassword(event) {
      event.preventDefault();
      try { const mode = document.querySelector('input[name="editPasswordMode"]:checked').value; const path = `/api/v1/accounts/${encodeURIComponent(state.passwordAccount.accountId)}/password`; if (mode === "any") await request(path, { method: "DELETE" }); else { const password = byId("editPassword").value; if (!password) throw new Error("固定密码不能为空。"); await request(path, { method: "PUT", body: JSON.stringify({ password }) }); } byId("editPassword").value = ""; byId("passwordDialog").close(); await loadAccounts(); showMessage("密码模式已更新，旧连接正在撤销。"); } catch (error) { byId("passwordFormError").textContent = error.message; }
    }

    /** 处理账号表格的单账号编辑入口；破坏性操作统一收敛到显式批量选择流程。 */
    async function accountAction(event) {
      const button = event.target.closest("button[data-action]"); if (!button) return;
      const account = state.accounts.find((candidate) => candidate.accountId === button.dataset.accountId); if (!account) return;
      try {
        if (button.dataset.action === "edit") return openAccountDialog(account);
        if (button.dataset.action === "password") { state.passwordAccount = account; byId("passwordAccountName").textContent = `账号：${account.username}`; byId("editPassword").value = ""; byId("passwordFormError").textContent = ""; byId("passwordDialog").showModal(); return; }
        if (button.dataset.action === "connections") { await activateTab("overview", account.accountId); return; }
      } catch (error) { showMessage("", error.message); }
    }

    /** 根据行选择框维护稳定账号集合；账号刷新后由 loadAccounts 清理已删除项。 */
    function updateAccountSelection(event) {
      const selection = event.target.closest("input[data-select-account-id]");
      if (!selection) return;
      if (selection.checked) state.selectedAccountIds.add(selection.dataset.selectAccountId);
      else state.selectedAccountIds.delete(selection.dataset.selectAccountId);
      renderSelectionActions();
      scheduleUiContextReport();
    }

    /** 全选或取消当前筛选结果，不改动其他筛选页已经选择的账号。 */
    function toggleVisibleAccounts(event) {
      for (const accountId of state.visibleAccountIds) {
        if (event.target.checked) state.selectedAccountIds.add(accountId);
        else state.selectedAccountIds.delete(accountId);
      }
      renderAccounts();
      scheduleUiContextReport();
    }

    /** 返回带最新策略修订号的选择快照，服务端据此实现批量乐观锁事务。 */
    function selectedAccountRevisions() {
      return state.accounts
        .filter((account) => state.selectedAccountIds.has(account.accountId))
        .map((account) => ({ accountId: account.accountId, policyRevision: account.policyRevision }));
    }

    /** 打开批量编辑对话框并重置所有可选字段，避免沿用上一次未提交输入。 */
    function openBatchEditDialog() {
      const accounts = selectedAccountRevisions();
      if (accounts.length === 0) return;
      for (const [toggleId, fieldIds] of [["changeOnlineIps", ["batchOnlineIps"]], ["changeConnections", ["batchConnections"]], ["changeUpload", ["batchUpload"]], ["changeDownload", ["batchDownload"]], ["changeExpiration", ["batchDuration", "batchDurationUnit"]]]) {
        byId(toggleId).checked = false;
        for (const fieldId of fieldIds) byId(fieldId).disabled = true;
      }
      byId("batchOnlineIps").value = "-1";
      byId("batchConnections").value = "-1";
      byId("batchUpload").value = "-1";
      byId("batchDownload").value = "-1";
      byId("batchDuration").value = "1";
      byId("batchDurationUnit").value = "86400000";
      byId("batchEditSummary").textContent = `将修改 ${accounts.length} 个账号；未勾选的字段保持不变。`;
      byId("batchEditError").textContent = "";
      byId("batchEditDialog").showModal();
    }

    /** 读取已勾选的批量字段；无修改项或不安全整数会立即终止提交并显示明确错误。 */
    function batchUpdateRequest() {
      const requestBody = { accounts: selectedAccountRevisions() };
      const numberFields = [["changeOnlineIps", "batchOnlineIps", "maxOnlineIps"], ["changeConnections", "batchConnections", "maxConnections"], ["changeUpload", "batchUpload", "maxUploadBytesPerSecond"], ["changeDownload", "batchDownload", "maxDownloadBytesPerSecond"]];
      for (const [toggleId, inputId, property] of numberFields) {
        if (!byId(toggleId).checked) continue;
        const value = Number(byId(inputId).value);
        if (!Number.isSafeInteger(value) || value < -1) throw new Error("批量限制必须是大于等于 -1 的整数。");
        requestBody[property] = value;
      }
      if (byId("changeExpiration").checked) {
        const duration = Number(byId("batchDuration").value);
        const unit = Number(byId("batchDurationUnit").value);
        const milliseconds = duration * unit;
        if (!Number.isSafeInteger(duration) || duration <= 0 || !Number.isSafeInteger(milliseconds)) throw new Error("加时时长必须是有效正整数。");
        requestBody.extendByMilliseconds = milliseconds;
      }
      if (Object.keys(requestBody).length === 1) throw new Error("请至少勾选一个要修改的项目。");
      return requestBody;
    }

    /** 在单个服务端事务中提交批量修改；任一修订冲突都会整体失败而不产生部分更新。 */
    async function saveBatchEdit(event) {
      event.preventDefault();
      try {
        const result = await request("/api/v1/accounts/batch", { method: "PATCH", body: JSON.stringify(batchUpdateRequest()) });
        byId("batchEditDialog").close();
        await loadAccounts();
        showMessage(`已更新 ${result.updatedAccounts} 个账号。`);
      } catch (error) { byId("batchEditError").textContent = error.message; }
    }

    /** 批量删除只接受当前选择快照；二次确认后由服务端事务保证全删或全不删。 */
    async function deleteSelectedAccounts() {
      const accounts = selectedAccountRevisions();
      if (accounts.length === 0 || !confirm(`确认永久删除已选择的 ${accounts.length} 个账号？`)) return;
      try {
        const result = await request("/api/v1/accounts/batch", { method: "DELETE", body: JSON.stringify({ accounts }) });
        state.selectedAccountIds.clear();
        await loadAccounts();
        showMessage(`已删除 ${result.deletedAccounts} 个账号。`);
      } catch (error) { showMessage("", error.message); }
    }

    /** 绑定批量字段启用开关，关闭时禁用输入以明确表示该策略不会被提交。 */
    function bindBatchField(toggleId, ...fieldIds) {
      byId(toggleId).addEventListener("change", (event) => {
        for (const fieldId of fieldIds) byId(fieldId).disabled = !event.target.checked;
      });
    }

    /** 登录成功后默认进入概览，并以固定周期刷新实时在线态和带宽。 */
    async function enterWorkspace() {
      state.contextActive = true; await loadAccounts(); await activateTab("overview"); byId("loginView").hidden = true; byId("workspace").hidden = false;
      if (state.overviewTimer === null) state.overviewTimer = window.setInterval(() => { if (!byId("overviewSection").hidden) loadConnections().catch((error) => showMessage("", error.message)); }, overviewRefreshMilliseconds);
      if (state.contextTimer === null) state.contextTimer = window.setInterval(scheduleUiContextReport, 5_000);
      scheduleUiContextReport();
    }

    byId("loginForm").addEventListener("submit", async (event) => { event.preventDefault(); byId("loginError").textContent = ""; try { await request("/api/v1/auth/login", { method: "POST", body: JSON.stringify({ username: byId("loginUsername").value, password: byId("loginPassword").value }) }); byId("loginPassword").value = ""; await enterWorkspace(); } catch (error) { byId("loginError").textContent = error.message; } });
    document.querySelector(".sideNav").addEventListener("click", (event) => { const tab = event.target.closest("button[data-tab]"); if (tab) activateTab(tab.dataset.tab); });
    byId("accountSearch").addEventListener("input", renderAccounts); byId("accountStatusFilter").addEventListener("change", renderAccounts); byId("accountExpiryFilter").addEventListener("change", renderAccounts); byId("accountSort").addEventListener("change", renderAccounts); byId("accountRows").addEventListener("click", accountAction); byId("accountRows").addEventListener("change", updateAccountSelection); byId("selectVisibleAccounts").addEventListener("change", toggleVisibleAccounts); byId("createAccountButton").addEventListener("click", () => openAccountDialog());
    byId("refreshAccounts").addEventListener("click", () => loadAccounts().catch((error) => showMessage("", error.message))); byId("refreshConnections").addEventListener("click", () => loadConnections().catch((error) => showMessage("", error.message)));
    byId("refreshPackages").addEventListener("click", () => loadClientPackages().catch((error) => { byId("packageError").textContent = error.message; }));
    byId("refreshRuleSets").addEventListener("click", () => loadRuleSets().catch((error) => showMessage("", error.message))); byId("createRuleSetButton").addEventListener("click", () => openRuleSetDialog()); byId("ruleSetRows").addEventListener("click", ruleSetAction); byId("ruleSetRows").addEventListener("change", updateRuleSetSelection); byId("selectAllRuleSets").addEventListener("change", toggleAllRuleSets); byId("deleteSelectedRuleSets").addEventListener("click", deleteSelectedRuleSets); byId("ruleSetForm").addEventListener("submit", saveRuleSet);
    byId("batchEditButton").addEventListener("click", openBatchEditDialog); byId("batchDeleteButton").addEventListener("click", deleteSelectedAccounts); byId("batchEditForm").addEventListener("submit", saveBatchEdit);
    bindBatchField("changeOnlineIps", "batchOnlineIps"); bindBatchField("changeConnections", "batchConnections"); bindBatchField("changeUpload", "batchUpload"); bindBatchField("changeDownload", "batchDownload"); bindBatchField("changeExpiration", "batchDuration", "batchDurationUnit");
    document.querySelectorAll('input[name="passwordMode"]').forEach((radio) => radio.addEventListener("change", () => { byId("fixedPasswordLabel").hidden = document.querySelector('input[name="passwordMode"]:checked').value !== "fixed"; }));
    document.querySelectorAll('input[name="editPasswordMode"]').forEach((radio) => radio.addEventListener("change", () => { byId("editFixedPasswordLabel").hidden = document.querySelector('input[name="editPasswordMode"]:checked').value !== "fixed"; }));
    byId("accountForm").addEventListener("submit", saveAccount); byId("passwordForm").addEventListener("submit", savePassword);
    document.querySelectorAll("[data-close-dialog]").forEach((button) => button.addEventListener("click", () => { const dialog = byId(button.dataset.closeDialog); dialog.close(); if (dialog.id === "passwordDialog") byId("editPassword").value = ""; }));
    window.addEventListener("focus", scheduleUiContextReport); window.addEventListener("blur", scheduleUiContextReport); document.addEventListener("visibilitychange", scheduleUiContextReport);
    request("/api/v1/auth/session").then(enterWorkspace).catch(() => {});
