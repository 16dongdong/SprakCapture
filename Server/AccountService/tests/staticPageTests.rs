#![allow(non_snake_case, non_upper_case_globals)]

const managementPage: &str = include_str!("../web/index.html");
const managementScript: &str = include_str!("../web/app.js");
const managementStyles: &str = include_str!("../web/styles.css");
const clientDownloadPage: &str = include_str!("../clientWeb/index.html");
const clientDownloadScript: &str = include_str!("../clientWeb/app.js");
const clientDownloadStyles: &str = include_str!("../clientWeb/styles.css");

/// 汇总页面三类资源用于跨文件静态契约检查；资源仅来自编译期嵌入，不访问运行文件系统。
fn managementResources() -> String {
    [managementPage, managementScript, managementStyles].join("\n")
}

/// 验证公共管理页只保留概览、账号管理、规则集和客户端安装包四个工作区。
///
/// 运行上下文：HTML、CSS 和 JavaScript 分文件嵌入账号服务，结构同时承担桌面端和窄屏导航。
/// 失败语义：缺少必要入口或重新出现已删除工作区时立即失败，阻止旧页面结构回归。
#[test]
fn managementPageContainsFocusedWorkspaces() {
    let resources = managementResources();
    for required in [
        "<title>SOCKS5 管理</title>",
        "class=\"sidebar\"",
        "class=\"sideNav\"",
        "data-tab=\"overview\" aria-selected=\"true\"",
        "data-tab=\"accounts\" aria-selected=\"false\"",
        "data-tab=\"ruleSets\" aria-selected=\"false\"",
        "data-tab=\"packaging\" aria-selected=\"false\"",
        "data-section=\"overview\"",
        "data-section=\"accounts\"",
        "data-section=\"ruleSets\"",
        "data-section=\"packaging\"",
        "/api/v1/accounts",
        "/api/v1/connections",
        "/api/v1/statistics",
    ] {
        assert!(
            resources.contains(required),
            "管理页面缺少必要契约：{required}"
        );
    }
    for removed in [
        "class=\"appHeader\"",
        "id=\"serviceSummary\"",
        "data-tab=\"connections\"",
        "data-tab=\"usage\"",
        "data-tab=\"audit\"",
        "data-tab=\"management\"",
        "id=\"usageSection\"",
        "id=\"auditSection\"",
        "id=\"managementSection\"",
    ] {
        assert!(
            !resources.contains(removed),
            "管理页面仍包含已删除结构：{removed}"
        );
    }
}

/// 验证客户端安装包页只展示公共生成 URL 和脱敏历史，不保留直接生成或重复下载按钮。
///
/// 运行上下文：页面位于 `/account-management/`，`../api/v1` 复用主远程会话并由账号服务转发控制面。
/// 失败语义：缺少生成 URL、历史字段或重新出现管理端生成/下载入口都会破坏单一下载流程。
#[test]
fn packagingWorkspaceUsesSameOriginControlApi() {
    let resources = managementResources();
    for required in [
        "客户端打包",
        "const controlApiBasePath = new URL(\"../api/v1/\"",
        "requestControl(\"/clientPackages\"",
        "Android 包生成 URL",
        "new URL(\"/client\", window.location.origin)",
        "安装包生成记录",
        "applicationName",
        "applicationId",
    ] {
        assert!(
            resources.contains(required),
            "客户端打包缺少契约：{required}"
        );
    }
    for removed in [
        "id=\"startPackaging\"",
        "data-package-id",
        "startClientPackaging",
        "downloadClientPackage",
    ] {
        assert!(
            !resources.contains(removed),
            "客户端安装包页仍含旧入口：{removed}"
        );
    }
}

/// 验证规则集页面具备列表、多选删除、编辑正文和互斥开关的完整可达契约。
///
/// 运行上下文：编译期合并 HTML、CSS 和 JavaScript，同时检查默认 DNS 模板与双模式说明。
/// 失败语义：任一结构、操作或 DNS 协议文案缺失都会使管理页无法创建当前客户端可执行的规则。
#[test]
fn ruleSetWorkspaceSupportsEditingSelectionAndExclusiveEnable() {
    let resources = managementResources();
    for required in [
        "id=\"ruleSetRows\"",
        "id=\"selectAllRuleSets\"",
        "id=\"deleteSelectedRuleSets\"",
        "id=\"ruleSetDialog\"",
        "id=\"ruleSetContent\"",
        "request(\"/api/v1/ruleSets\"",
        "/enabled",
        "enabled: !ruleSet.enabled",
        "method: \"DELETE\"",
        "[DNS]",
        "PRIMARY,223.5.5.5",
        "SECONDARY,1.1.1.1",
        "[RoutingRule]",
        "[GRoutingRule]",
        "FINAL,PROXY",
        "[proxy_app]",
        "[RoutingRule] 只作用于所选应用",
        "[GRoutingRule] 只作用于其他应用",
        "DNS 由客户端直连指定上游",
        "VPN 与 Root 模式共用",
    ] {
        assert!(
            resources.contains(required),
            "规则集页面缺少契约：{required}"
        );
    }
}

/// 验证公开下载页独立分层、只提交当次凭据且请求结束立即清除密码。
#[test]
fn publicClientDownloadPageUsesOneTimeCredentialedPost() {
    let resources = [
        clientDownloadPage,
        clientDownloadScript,
        clientDownloadStyles,
    ]
    .join("\n");
    for required in [
        "<title>下载代理客户端</title>",
        "id=\"downloadForm\"",
        "id=\"username\"",
        "id=\"password\"",
        "id=\"applicationId\"",
        "id=\"applicationName\"",
        "id=\"applicationIcon\"",
        "下载代理客户端",
        "/api/v1/clientPackages/download",
        "optionalText(applicationIdInput)",
        "optionalText(applicationNameInput)",
        "iconBase64",
        "passwordInput.value = \"\"",
        "application/vnd.android.package-archive",
    ] {
        assert!(
            resources.contains(required),
            "客户端下载页缺少契约：{required}"
        );
    }
    assert!(!resources.contains("localStorage"));
    assert!(!resources.contains("sessionStorage"));
    assert!(!resources.contains("已内置节点"));
    assert!(!clientDownloadScript.contains("innerHTML"));
}

/// 验证概览默认展示在线态、连接数和实时上下行速率，并只在可见时定期刷新。
///
/// 运行上下文：统计接口保留累计字段以兼容旧调用方，但当前页面只能把速率字段展示为实时带宽。
/// 失败语义：字段、默认面板或轮询条件缺失表示页面会退回累计值或停止实时刷新。
#[test]
fn overviewUsesRealtimeBandwidthAndStartsAsDefaultWorkspace() {
    let resources = managementResources();
    for required in [
        "[\"连接数\", summary.activeConnections]",
        "[\"实时上行\", formatRate(summary.uploadBytesPerSecond)]",
        "[\"实时下行\", formatRate(summary.downloadBytesPerSecond)]",
        "connection.uploadBytesPerSecond",
        "connection.downloadBytesPerSecond",
        "await activateTab(\"overview\")",
        "overviewRefreshMilliseconds",
        "if (!byId(\"overviewSection\").hidden)",
    ] {
        assert!(
            resources.contains(required),
            "概览缺少实时统计契约：{required}"
        );
    }
    assert!(!resources.contains("[\"累计流量\""));
}

/// 验证服务端业务文本不会通过 innerHTML 进入浏览器解析器。
///
/// 运行上下文：账号名和备注可包含 HTML 特殊字符，页面必须只使用 textContent 和 DOM 节点写入。
/// 失败语义：出现 innerHTML 即视为不可信文本边界退化，测试直接失败。
#[test]
fn managementPageDoesNotInterpretBusinessTextAsHtml() {
    assert!(!managementScript.contains("innerHTML"));
    assert!(managementScript.contains("textContent"));
}

/// 验证登录和 SOCKS5 账号密码在操作完成或对话框关闭后显式清理。
///
/// 运行上下文：公共页面已移除管理员与 API Key 工作区，只需约束仍存在的两个密码输入生命周期。
/// 失败语义：缺少清理语句表示密码会超出对应操作周期留在页面内存。
#[test]
fn managementPageClearsPasswordValues() {
    for required in [
        "byId(\"loginPassword\").value = \"\"",
        "byId(\"editPassword\").value = \"\"",
    ] {
        assert!(
            managementScript.contains(required),
            "缺少密码清理：{required}"
        );
    }
}

/// 验证账号工作区保留搜索、状态、到期和排序，并由同一投影函数组合处理。
///
/// 运行上下文：公共账号接口提供有界分页，页面读取完整快照后执行本地组合查询。
/// 失败语义：缺少控件、筛选键或事件绑定会使账号定位能力不可达。
#[test]
fn accountWorkspaceContainsCompleteLocalQueryControls() {
    let resources = managementResources();
    for required in [
        "id=\"accountSearch\"",
        "id=\"accountStatusFilter\"",
        "id=\"accountExpiryFilter\"",
        "id=\"accountSort\"",
        "accountStatusKey(account) === statusFilter",
        "matchesExpiryFilter(account, expiryFilter)",
        "sort === \"usernameAsc\"",
        "sort === \"expiryAsc\"",
        "sort === \"trafficDesc\"",
        "byId(\"accountStatusFilter\").addEventListener(\"change\", renderAccounts)",
        "byId(\"accountExpiryFilter\").addEventListener(\"change\", renderAccounts)",
        "byId(\"accountSort\").addEventListener(\"change\", renderAccounts)",
    ] {
        assert!(
            resources.contains(required),
            "账号页面缺少本地查询契约：{required}"
        );
    }
}

/// 验证批量操作必须经过账号选择，并且单行不再暴露下线或删除操作。
///
/// 运行上下文：批量编辑对话框承载在线 IP、连接数、共享上下行和原到期时间基准加时。
/// 失败语义：选择门槛、事务端点或任一策略控件缺失都表示用户需求不可达。
#[test]
fn accountWorkspaceUsesSelectionDrivenBatchActions() {
    let resources = managementResources();
    let accountScript = managementScript
        .split("/** 构造规则集表格行")
        .next()
        .expect("账号脚本位于规则集脚本之前");
    for required in [
        "id=\"selectVisibleAccounts\"",
        "id=\"batchActions\"",
        "id=\"batchEditDialog\"",
        "id=\"batchOnlineIps\"",
        "id=\"batchConnections\"",
        "id=\"batchUpload\"",
        "id=\"batchDownload\"",
        "id=\"batchDuration\"",
        "以每个账号原到期时间为基准",
        "request(\"/api/v1/accounts/batch\", { method: \"PATCH\"",
        "request(\"/api/v1/accounts/batch\", { method: \"DELETE\"",
    ] {
        assert!(resources.contains(required), "批量操作缺少契约：{required}");
    }
    assert!(!accountScript.contains("[\"下线\", \"disconnect\""));
    assert!(!accountScript.contains("[\"删除\", \"delete\""));
}

/// 验证页面只引用独立同源资源，不把样式和应用脚本重新塞回 HTML。
#[test]
fn managementPageKeepsStructureStyleAndBehaviorSeparated() {
    assert!(managementPage.contains("href=\"styles.css\""));
    assert!(managementPage.contains("src=\"app.js\" defer"));
    assert!(!managementPage.contains("<style>"));
    assert!(!managementPage.contains("<script>"));
    assert!(managementStyles.contains(".appLayout"));
    assert!(managementScript.contains("async function request"));
}
