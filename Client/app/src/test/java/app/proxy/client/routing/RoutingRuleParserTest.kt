package app.proxy.client.routing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证云规则、DNS 直连契约和两个 Android 数据面的公共应用范围投影。 */
class RoutingRuleParserTest {
    /** 没有全局规则时，VPN 与 Root 都只接管 `[proxy_app]` 中由服务器下发的应用。 */
    @Test
    fun packageRulesCreateRestrictedVpnScope() {
        val document = RoutingRuleParser.parse(
            validRule(
                routing = "FINAL,PROXY",
                global = "",
                packages = "com.example.alpha\ncom.example.beta",
            ),
        )

        assertEquals(setOf("com.example.alpha", "com.example.beta"), document.proxyPackages)
        assertEquals(VpnApplicationScope.Packages(document.proxyPackages), document.vpnScope)
    }

    /** 任意全局规则都可能命中所有 UID，因此两个数据面必须切换为全局捕获并排除自身。 */
    @Test
    fun globalRulesCreateGlobalVpnScope() {
        val document = RoutingRuleParser.parse(validRule("", "PORT,443,PROXY", ""))

        assertEquals(VpnApplicationScope.Global, document.vpnScope)
    }

    /** 应用规则缺少 UID 列表会让归属不明确，必须在落盘前拒绝。 */
    @Test
    fun emptyCaptureScopeIsRejected() {
        val failure = runCatching { RoutingRuleParser.parse(validRule("FINAL,PROXY", "", "")) }.exceptionOrNull()

        assertTrue(failure?.message?.contains("必须同时配置") == true)
    }

    /** 路由动作和 CIDR 必须在 Native 支持范围内，防止 Kotlin 缓存 Native 稍后拒绝的正文。 */
    @Test
    fun invalidNativeRuleIsRejectedBeforeCaching() {
        val failure = runCatching {
            RoutingRuleParser.parse(validRule("IP-CIDR,300.1.1.1/24,PROXY", "", "com.example.app"))
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("IPv4 CIDR 无效") == true)
    }

    /** 显式 CIDR 前缀不能回退 `/32`；非数字、空前缀和多余斜线必须与 Server/Native 一致拒绝。 */
    @Test
    fun malformedExplicitCidrPrefixesAreRejected() {
        listOf("1.2.3.4/foo", "1.2.3.4/", "1.2.3.4/24/extra").forEach { cidr ->
            val failure = runCatching {
                RoutingRuleParser.parse(validRule("IP-CIDR,$cidr,PROXY", "", "com.example.app"))
            }.exceptionOrNull()

            assertTrue("未拒绝非法 CIDR：$cidr", failure?.message?.contains("IPv4 CIDR 无效") == true)
        }
    }

    /** 包名必须是有效 Android applicationId，禁止 Root 与 VPN 对非法字符产生不同 UID 投影。 */
    @Test
    fun invalidAndroidApplicationIdIsRejected() {
        val failure = runCatching {
            RoutingRuleParser.parse(validRule("FINAL,PROXY", "", "com.example.invalid-name"))
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("应用包名无效") == true)
    }

    /** 统一 Native 核心必须让应用模式接受 DIRECT/REJECT 混合动作，不再退回单出口 HEV 限制。 */
    @Test
    fun mixedActionsAreAcceptedForBothModes() {
        val document = RoutingRuleParser.parse(
            validRule("DOMAIN,example.com,DIRECT\nFINAL,REJECT", "", "com.example.app"),
        )

        assertEquals(VpnApplicationScope.Packages(setOf("com.example.app")), document.vpnScope)
    }

    /** 每个作用域最多一个 FINAL；第二个终止规则在 Native 中不可达，必须在缓存前拒绝。 */
    @Test
    fun duplicateFinalInSameScopeIsRejected() {
        val failure = runCatching {
            RoutingRuleParser.parse(validRule("FINAL,PROXY\nFINAL,DIRECT", "", "com.example.app"))
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("[RoutingRule] FINAL 之后") == true)
    }

    /** FINAL 必须是所属段最后一条有效规则，普通规则放在其后不能被静默保留为死配置。 */
    @Test
    fun routeAfterGlobalFinalIsRejected() {
        val failure = runCatching {
            RoutingRuleParser.parse(validRule("", "FINAL,DIRECT\nDOMAIN,example.com,PROXY", ""))
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("[GRoutingRule] FINAL 之后") == true)
    }

    /** 混合模式保留两类规则及应用列表，供数据面按 UID 选择 RoutingRule 或 GRoutingRule 上下文。 */
    @Test
    fun mixedGlobalAndApplicationScopesAreAccepted() {
        val document = RoutingRuleParser.parse(
            validRule("DOMAIN,abc.com,PROXY", "DOMAIN,aaa.com,PROXY", "com.example.app"),
        )

        assertTrue(document.hasRoutingRules)
        assertTrue(document.hasGlobalRules)
        assertEquals(setOf("com.example.app"), document.proxyPackages)
        assertEquals(VpnApplicationScope.Global, document.vpnScope)
    }

    /** 旧空格段名必须拒绝，避免服务端、Kotlin 与 Native 对同一正文产生不同解析结果。 */
    @Test
    fun legacyProxyAppSectionNameIsRejected() {
        val failure = runCatching {
            RoutingRuleParser.parse(
                validRule("FINAL,PROXY", "", "com.example.app").replace("[proxy_app]", "[proxy app]"),
            )
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("包含未知段：[proxy app]") == true)
    }

    /** proxy_app 行只允许包名，静默忽略额外字段会让服务端与两个数据面产生不同 UID 集合。 */
    @Test
    fun proxyAppExtraFieldsAreRejected() {
        val failure = runCatching {
            RoutingRuleParser.parse(validRule("FINAL,PROXY", "", "com.example.app,unexpected"))
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("只能包含包名") == true)
    }

    /** 完全相同包名重复出现会掩盖服务端编辑错误，必须在进入 UID 投影前精确拒绝。 */
    @Test
    fun duplicateProxyAppIsRejected() {
        val failure = runCatching {
            RoutingRuleParser.parse(validRule("FINAL,PROXY", "", "com.example.app\ncom.example.app"))
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("重复声明 proxy_app：com.example.app") == true)
    }

    /** 包名合同固定为小写；大小写变体不得作为另一个应用绕过重复项和 shared UID 校验。 */
    @Test
    fun uppercaseProxyAppVariantIsRejected() {
        val failure = runCatching {
            RoutingRuleParser.parse(validRule("FINAL,PROXY", "", "com.example.app\ncom.Example.app"))
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("应用包名无效：com.Example.app") == true)
    }

    /** 四个协议段都只能出现一次，重复段不得采用拼接或最后覆盖等不一致语义。 */
    @Test
    fun duplicateProtocolSectionsAreRejected() {
        val valid = validRule("", "FINAL,DIRECT", "")
        val sections = listOf("[RoutingRule]", "[GRoutingRule]", "[proxy_app]", "[DNS]")

        sections.forEach { section ->
            val failure = runCatching { RoutingRuleParser.parse("$valid\n$section\n") }.exceptionOrNull()
            assertTrue("未拒绝重复段 $section", failure?.message?.contains("重复声明") == true)
        }
    }

    /** 合同只允许四个固定段，拼错段名必须立即暴露而不是静默丢弃整段配置。 */
    @Test
    fun unknownSectionIsRejected() {
        val text = validRule("", "FINAL,DIRECT", "").replace("[DNS]", "[DMS]")
        val failure = runCatching { RoutingRuleParser.parse(text) }.exceptionOrNull()

        assertTrue(failure?.message?.contains("包含未知段：[DMS]") == true)
    }

    /** 非空规则必须属于一个已知段，文件头或段外游离文本不能被当作无效注释吞掉。 */
    @Test
    fun contentOutsideSectionIsRejected() {
        val failure = runCatching {
            RoutingRuleParser.parse("DOMAIN,example.com,PROXY\n${validRule("", "FINAL,DIRECT", "")}")
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("有效内容不在协议段内") == true)
    }

    /** PRIMARY 必填且 SECONDARY 可选，解析顺序必须固定供 Native 故障转移使用。 */
    @Test
    fun explicitDnsServersPreservePriority() {
        val document = RoutingRuleParser.parse(validRule("", "FINAL,DIRECT", ""))

        assertEquals(listOf("223.5.5.5", "2001:4860:4860::8888"), document.dnsServers)
    }

    /** DNS 段名和角色按协议不区分大小写，解析结果仍规范为主服务器优先。 */
    @Test
    fun dnsKeysAreCaseInsensitive() {
        val text = validRule("", "FINAL,DIRECT", "")
            .replace("[DNS]", "[dns]")
            .replace("PRIMARY,", "primary,")
            .replace("SECONDARY,", "secondary,")

        assertEquals(listOf("223.5.5.5", "2001:4860:4860::8888"), RoutingRuleParser.parse(text).dnsServers)
    }

    /** DNS 地址不得使用域名，否则解析配置本身就会产生未受规则控制的系统 DNS 请求。 */
    @Test
    fun dnsHostnameIsRejected() {
        val text = validRule("", "FINAL,DIRECT", "")
            .replace("PRIMARY,223.5.5.5", "PRIMARY,dns.example")
        val failure = runCatching { RoutingRuleParser.parse(text) }.exceptionOrNull()

        assertTrue(failure?.message?.contains("必须是 IPv4 或 IPv6 字面量") == true)
    }

    /** 带前导零的 IPv4 在不同平台可能被解释为八进制，必须与服务端和 inet_pton 一致拒绝。 */
    @Test
    fun ambiguousIpv4DnsIsRejected() {
        val text = validRule("", "FINAL,DIRECT", "")
            .replace("PRIMARY,223.5.5.5", "PRIMARY,223.005.5.5")
        val failure = runCatching { RoutingRuleParser.parse(text) }.exceptionOrNull()

        assertTrue(failure?.message?.contains("必须是 IPv4 或 IPv6 字面量") == true)
    }

    /** 重复 DNS 段或键属于歧义配置，必须拒绝而不是采用最后一个值。 */
    @Test
    fun duplicateDnsConfigurationIsRejected() {
        val text = validRule("", "FINAL,DIRECT", "") + "\n[DNS]\nPRIMARY,1.1.1.1\n"
        val failure = runCatching { RoutingRuleParser.parse(text) }.exceptionOrNull()

        assertTrue(failure?.message?.contains("重复声明 [DNS]") == true)
    }

    /** 行长按 UTF-8 字节计数，多字节字符不得绕过与服务端一致的 8192 字节上限。 */
    @Test
    fun lineLimitUsesUtf8Bytes() {
        val failure = runCatching {
            RoutingRuleParser.parse(validRule("", "DOMAIN,${"界".repeat(2731)},DIRECT", ""))
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("8192 字节") == true)
    }

    /** 构造包含公共 DNS 段的最小规则，减少测试夹具之间无意义的格式差异。 */
    private fun validRule(routing: String, global: String, packages: String): String = """
        [RoutingRule]
        $routing
        [GRoutingRule]
        $global
        [proxy_app]
        $packages
        [DNS]
        PRIMARY,223.5.5.5
        SECONDARY,2001:4860:4860::8888
    """.trimIndent()
}
