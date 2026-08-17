package app.proxy.client.routing

import app.proxy.client.domain.IpAddressLiteral
import java.util.Locale

/**
 * 保存服务端规则的已验证投影。
 * Native 是 VPN 与 Root 的唯一决策引擎；Kotlin 只提取 TUN 应用范围和直连 DNS 地址，避免两套规则语义漂移。
 */
data class RoutingRuleDocument(
    val text: String,
    val proxyPackages: Set<String>,
    val hasRoutingRules: Boolean,
    val hasGlobalRules: Boolean,
    val dnsServers: List<String>,
) {
    /** 标识当前正文需要的连接归属上下文，数据面据此选择全局、选中应用或五元组混合分流。 */
    val routingContext: RoutingContext = when {
        hasRoutingRules && hasGlobalRules -> RoutingContext.MIXED
        hasRoutingRules -> RoutingContext.SELECTED
        else -> RoutingContext.GLOBAL
    }

    /** 存在全局规则时 VPN 必须捕获全部应用并在 TUN 内区分 UID；否则只接管服务端列出的包。 */
    val vpnScope: VpnApplicationScope = if (hasGlobalRules) {
        VpnApplicationScope.Global
    } else {
        VpnApplicationScope.Packages(proxyPackages)
    }
}

/** 固定规则上下文语义；MIXED 必须保留原始五元组并查询连接 UID，禁止退化成单 SOCKS 入口。 */
enum class RoutingContext {
    GLOBAL,
    SELECTED,
    MIXED,
}

/** 表达 VpnService 唯一允许的两种应用范围，禁止客户端再引入本地应用选择状态。 */
sealed interface VpnApplicationScope {
    data object Global : VpnApplicationScope
    data class Packages(val packageNames: Set<String>) : VpnApplicationScope
}

/**
 * 按 Native 统一路由核心的语法边界校验云端规则。
 * DNS 必须使用明确 IP，禁止为解析 DNS 配置而先访问系统 DNS；规则正文仍由 Native 二次校验后原子生效。
 */
object RoutingRuleParser {
    private const val maximumRuleBytes = 1024 * 1024
    private const val maximumLineBytes = 8192
    private const val primaryDnsKey = "PRIMARY"
    private const val secondaryDnsKey = "SECONDARY"
    private val packageNamePattern = Regex("^[a-z][a-z0-9_]*(?:\\.[a-z][a-z0-9_]*)+$")
    private val supportedRuleTypes = setOf("PORT", "IP-CIDR", "DOMAIN", "DOMAIN-KEYWORD")
    private val supportedActions = setOf("PROXY", "DIRECT", "REJECT")

    /** 解析完整 UTF-8 规则正文；缺少关键段、DNS 重复或语法越界时返回精确中文错误。 */
    fun parse(text: String): RoutingRuleDocument {
        require(text.toByteArray(Charsets.UTF_8).size <= maximumRuleBytes) { "规则文件超过 1 MiB 上限" }
        require('\u0000' !in text) { "规则文件包含 NUL 字符" }
        var section = RuleSection.NONE
        var routingSectionFound = false
        var globalSectionFound = false
        var proxyAppSectionFound = false
        var dnsSectionFound = false
        var hasRoutingRules = false
        var hasGlobalRules = false
        var routingFinalSeen = false
        var globalFinalSeen = false
        val proxyPackages = linkedSetOf<String>()
        val dnsEntries = linkedMapOf<String, String>()

        text.lineSequence().forEachIndexed { index, sourceLine ->
            val lineNumber = index + 1
            require(sourceLine.toByteArray(Charsets.UTF_8).size <= maximumLineBytes) {
                "规则第 $lineNumber 行超过 $maximumLineBytes 字节上限"
            }
            val row = sourceLine.substringBefore('#').trim().removePrefix("\uFEFF").trim()
            if (row.isEmpty()) return@forEachIndexed
            if (row.startsWith('[') && row.endsWith(']')) {
                section = when (row.uppercase(Locale.ROOT)) {
                    "[ROUTINGRULE]" -> RuleSection.ROUTING.also {
                        require(!routingSectionFound) { "规则第 $lineNumber 行重复声明 [RoutingRule]" }
                        routingSectionFound = true
                    }
                    "[GROUTINGRULE]" -> RuleSection.GLOBAL.also {
                        require(!globalSectionFound) { "规则第 $lineNumber 行重复声明 [GRoutingRule]" }
                        globalSectionFound = true
                    }
                    "[PROXY_APP]" -> RuleSection.PROXY_APP.also {
                        require(!proxyAppSectionFound) { "规则第 $lineNumber 行重复声明 [proxy_app]" }
                        proxyAppSectionFound = true
                    }
                    "[DNS]" -> RuleSection.DNS.also {
                        require(!dnsSectionFound) { "规则第 $lineNumber 行重复声明 [DNS]" }
                        dnsSectionFound = true
                    }
                    else -> throw IllegalArgumentException("规则第 $lineNumber 行包含未知段：$row")
                }
                return@forEachIndexed
            }
            when (section) {
                RuleSection.ROUTING -> {
                    require(!routingFinalSeen) { "规则第 $lineNumber 行位于 [RoutingRule] FINAL 之后" }
                    routingFinalSeen = validateRouteRow(row, lineNumber)
                    hasRoutingRules = true
                }
                RuleSection.GLOBAL -> {
                    require(!globalFinalSeen) { "规则第 $lineNumber 行位于 [GRoutingRule] FINAL 之后" }
                    globalFinalSeen = validateRouteRow(row, lineNumber)
                    hasGlobalRules = true
                }
                RuleSection.PROXY_APP -> {
                    val packageName = parsePackageName(row, lineNumber)
                    require(proxyPackages.add(packageName)) { "规则第 $lineNumber 行重复声明 proxy_app：$packageName" }
                }
                RuleSection.DNS -> parseDnsRow(row, lineNumber, dnsEntries)
                RuleSection.NONE -> throw IllegalArgumentException("规则第 $lineNumber 行的有效内容不在协议段内")
            }
        }

        require(routingSectionFound) { "规则文件缺少 [RoutingRule]" }
        require(globalSectionFound) { "规则文件缺少 [GRoutingRule]" }
        require(proxyAppSectionFound) { "规则文件缺少 [proxy_app]" }
        require(dnsSectionFound) { "规则文件缺少 [DNS]" }
        require(primaryDnsKey in dnsEntries) { "规则文件 [DNS] 缺少 PRIMARY" }
        require(hasRoutingRules || hasGlobalRules) { "规则文件至少需要一条 [RoutingRule] 或 [GRoutingRule]" }
        require(hasRoutingRules == proxyPackages.isNotEmpty()) {
            "[RoutingRule] 与 [proxy_app] 必须同时配置，避免应用规则缺少 UID 归属"
        }
        return RoutingRuleDocument(
            text = text,
            proxyPackages = proxyPackages,
            hasRoutingRules = hasRoutingRules,
            hasGlobalRules = hasGlobalRules,
            dnsServers = listOfNotNull(dnsEntries[primaryDnsKey], dnsEntries[secondaryDnsKey]),
        )
    }

    /**
     * 校验 Native 支持的路由行并返回是否为 FINAL。
     * 调用方按段锁定 FINAL 后的输入，确保 Kotlin 与 Native 的首个终止规则语义完全一致且没有不可达配置。
     */
    private fun validateRouteRow(row: String, lineNumber: Int): Boolean {
        val fields = row.split(',').map(String::trim)
        val type = fields.firstOrNull()?.uppercase(Locale.ROOT).orEmpty()
        if (type == "FINAL") {
            require(fields.size == 2 && fields[1].uppercase(Locale.ROOT) in supportedActions) {
                "规则第 $lineNumber 行 FINAL 动作无效"
            }
            return true
        }
        require(fields.size == 3) { "规则第 $lineNumber 行字段数量无效" }
        require(type in supportedRuleTypes) { "规则第 $lineNumber 行类型不受支持：$type" }
        require(fields[1].isNotEmpty()) { "规则第 $lineNumber 行缺少匹配值" }
        require(fields[2].uppercase(Locale.ROOT) in supportedActions) { "规则第 $lineNumber 行动作无效：${fields[2]}" }
        when (type) {
            "PORT" -> validatePort(fields[1], lineNumber)
            "IP-CIDR" -> validateCidr(fields[1], lineNumber)
        }
        return false
    }

    /** 校验单端口或闭区间，严格限制在 TCP/UDP 端口范围内。 */
    private fun validatePort(value: String, lineNumber: Int) {
        val boundaries = value.split('-', limit = 2).map { it.trim().toIntOrNull() }
        require(boundaries.all { it != null }) { "规则第 $lineNumber 行端口格式无效" }
        val start = checkNotNull(boundaries.first())
        val end = checkNotNull(boundaries.last())
        require(start in 1..65535 && end in start..65535) { "规则第 $lineNumber 行端口越界" }
    }

    /** 校验 IPv4 CIDR；仅完全省略斜线时按 `/32`，显式空值、非数字或多余斜线均返回语法错误。 */
    private fun validateCidr(value: String, lineNumber: Int) {
        val parts = value.split('/')
        require(parts.size in 1..2 && isIpv4Literal(parts[0])) { "规则第 $lineNumber 行 IPv4 CIDR 无效" }
        val prefix = if (parts.size == 1) 32 else parts[1].toIntOrNull()
        require(prefix != null && prefix in 0..32) { "规则第 $lineNumber 行 IPv4 CIDR 无效" }
    }

    /** 读取 `[proxy_app]` 的唯一小写包名；额外列或大小写变体必须阻止跨实现产生不同 UID 投影。 */
    private fun parsePackageName(row: String, lineNumber: Int): String {
        val fields = row.split(',').map(String::trim)
        require(fields.size == 1) { "规则第 $lineNumber 行 proxy_app 只能包含包名" }
        val packageName = fields.single()
        require(packageName.matches(packageNamePattern)) { "规则第 $lineNumber 行应用包名无效：$packageName" }
        return packageName
    }

    /**
     * 解析 PRIMARY/SECONDARY 地址并拒绝重复和未知键。
     * 地址只能是 IP 字面量；SECONDARY 是显式容灾服务器，不允许 Native 隐式回退系统 DNS。
     */
    private fun parseDnsRow(row: String, lineNumber: Int, entries: MutableMap<String, String>) {
        val fields = row.split(',').map(String::trim)
        require(fields.size == 2) { "规则第 $lineNumber 行 DNS 字段数量无效" }
        val key = fields[0].uppercase(Locale.ROOT)
        require(key == primaryDnsKey || key == secondaryDnsKey) { "规则第 $lineNumber 行 DNS 键无效：${fields[0]}" }
        require(key !in entries) { "规则第 $lineNumber 行重复声明 DNS $key" }
        require(IpAddressLiteral.matches(fields[1])) { "规则第 $lineNumber 行 DNS 必须是 IPv4 或 IPv6 字面量" }
        entries[key] = fields[1]
    }

    /** 按四段十进制严格识别 IPv4，拒绝八进制、整数和域名等平台相关形式。 */
    private fun isIpv4Literal(value: String): Boolean {
        return IpAddressLiteral.parse(value)?.address?.size == 4
    }

    private enum class RuleSection {
        NONE,
        ROUTING,
        GLOBAL,
        PROXY_APP,
        DNS,
    }
}
