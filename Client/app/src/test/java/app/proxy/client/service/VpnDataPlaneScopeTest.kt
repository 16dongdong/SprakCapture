package app.proxy.client.service

import app.proxy.client.routing.RoutingRuleDocument
import app.proxy.client.routing.RoutingRuleParser
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证规则热更新何时必须重建 TUN/HEV 分类状态，避免沿用旧 proxy_app UID 投影。 */
class VpnDataPlaneScopeTest {
    /** 全局切换到混合时 VPN 都是全局捕获，但必须因五元组分类器从固定变为 UID 查询而重建。 */
    @Test
    fun globalToMixedRequiresRebuild() {
        val global = parseRule(routing = "", global = "FINAL,PROXY", packages = "")
        val mixed = parseRule("FINAL,PROXY", "FINAL,DIRECT", "com.example.app")

        assertFalse(mixed.vpnDataPlaneScopeEquals(global))
    }

    /** 混合规则更换应用集合会改变 owner UID 归属，必须重建而不是只调用 Native updateRules。 */
    @Test
    fun mixedPackageChangeRequiresRebuild() {
        val first = parseRule("FINAL,PROXY", "FINAL,DIRECT", "com.example.alpha")
        val second = parseRule("FINAL,PROXY", "FINAL,DIRECT", "com.example.beta")

        assertFalse(second.vpnDataPlaneScopeEquals(first))
    }

    /** 作用域不变时动作更新由 Native 原子处理，无需销毁系统 TUN 与 UID 分类状态。 */
    @Test
    fun actionOnlyChangeKeepsDataPlaneScope() {
        val first = parseRule("DOMAIN,example.com,PROXY", "FINAL,DIRECT", "com.example.app")
        val second = parseRule("DOMAIN,example.com,REJECT", "FINAL,PROXY", "com.example.app")

        assertTrue(second.vpnDataPlaneScopeEquals(first))
    }

    /** 构造包含固定 DNS 和新段名的有效规则，测试只改变与数据面范围相关的三个输入。 */
    private fun parseRule(routing: String, global: String, packages: String): RoutingRuleDocument =
        RoutingRuleParser.parse(
            """
                [RoutingRule]
                $routing
                [GRoutingRule]
                $global
                [proxy_app]
                $packages
                [DNS]
                PRIMARY,223.5.5.5
            """.trimIndent(),
        )
}
