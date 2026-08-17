package app.proxy.client.service

import app.proxy.client.runtime.NativeRuntimeConfiguration
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证 Root IPv4 TCP/UDP 捕获顺序、作用域隔离与旧资源清理合同。 */
class RootIptablesControllerTest {
    /** 全局捕获必须先保存 UDP 原目标，再由 NAT REDIRECT 交给 Native。 */
    @Test
    fun globalRulesInstallQueueBeforeRedirect() {
        val command = globalCommand()

        assertTrue(command.contains("--uid-owner 10234 -j RETURN"))
        assertTrue(command.contains("--uid-owner 0 -j RETURN"))
        assertTrue(command.contains("-p tcp -j REDIRECT --to-ports ${NativeRuntimeConfiguration.transparentTcpPort}"))
        assertTrue(command.contains("-j NFQUEUE --queue-num ${NativeRuntimeConfiguration.globalUdpQueueNumber} --queue-bypass"))
        assertTrue(command.contains("-p udp -j REDIRECT --to-ports ${NativeRuntimeConfiguration.transparentUdpPort}"))
        assertTrue(command.indexOf("-t mangle -I OUTPUT") < command.indexOf("-t nat -I OUTPUT"))
        assertFalse(command.contains("TPROXY"))
        assertFalse(command.contains("ip link add"))
        assertFalse(command.contains("ip rule add"))
        assertTrue(command.contains("SPRK6_OUT -p udp -j REJECT"))
    }

    /** 指定应用只进入 selected 队列和端口，不能退化成全局捕获。 */
    @Test
    fun packageRulesOnlyDispatchSelectedUids() {
        val command = buildRootApplyCommand(
            RootRoutingPlan(
                applicationUid = 10234,
                captureScope = RootCaptureScope(setOf(11001, 11002), captureUnselected = false),
            ),
        )

        assertTrue(command.contains("--uid-owner 11001 -p udp -j NFQUEUE --queue-num 6101"))
        assertTrue(command.contains("--uid-owner 11002 -p udp -j REDIRECT --to-ports 12348"))
        assertFalse(command.contains("-j NFQUEUE --queue-num 6100"))
        assertFalse(command.contains("SPRK_OUT -p udp -j REDIRECT --to-ports 12346"))
        assertTrue(command.contains("SPRK6_OUT -m owner --uid-owner 11001 -p tcp -j REJECT"))
        assertFalse(command.contains("SPRK6_OUT -p tcp -j REJECT"))
    }

    /** 混合模式必须先处理 selected UID，再让剩余应用进入 global 上下文。 */
    @Test
    fun mixedRulesDispatchSelectedBeforeGlobalFallback() {
        val command = buildRootApplyCommand(
            RootRoutingPlan(
                applicationUid = 10234,
                captureScope = RootCaptureScope(setOf(11001), captureUnselected = true),
            ),
        )

        val selectedQueue = command.indexOf("--uid-owner 11001 -p udp -j NFQUEUE --queue-num 6101")
        val globalQueue = command.indexOf("SPRK_MOUT -p udp -j NFQUEUE --queue-num 6100")
        val selectedRedirect = command.indexOf("--uid-owner 11001 -p udp -j REDIRECT --to-ports 12348")
        val globalRedirect = command.indexOf("SPRK_OUT -p udp -j REDIRECT --to-ports 12346")
        assertTrue(selectedQueue >= 0)
        assertTrue(globalQueue > selectedQueue)
        assertTrue(selectedRedirect >= 0)
        assertTrue(globalRedirect > selectedRedirect)
    }

    /** DNS 与其他 UDP 使用完全相同的队列和规则路径，不能添加端口 53 绕过。 */
    @Test
    fun capturedDnsIsNotBypassed() {
        assertFalse(globalCommand().contains("--dport 53 -j RETURN"))
    }

    /** 停止必须删除当前 NFQUEUE 链，并兼容回收旧 veth/TPROXY 资源。 */
    @Test
    fun cleanupRemovesCurrentAndLegacyResources() {
        val command = buildRootCleanupCommand()

        assertTrue(command.contains("while iptables -w 5 -t nat -C OUTPUT -j SPRK_OUT"))
        assertTrue(command.contains("while iptables -w 5 -t mangle -C OUTPUT -j SPRK_MOUT"))
        assertTrue(command.contains("while iptables -w 5 -t mangle -C PREROUTING -j SPRK_MPRE"))
        assertTrue(command.contains("if iptables -w 5 -t mangle -S SPRK_MOWN"))
        assertTrue(command.contains("while ip rule del pref 12001"))
        assertTrue(command.contains("ip route flush table 26003"))
        assertTrue(command.contains("ip link del sprkGOut"))
        assertTrue(command.contains("ip link del sprkSOut"))
        assertTrue(command.contains("while ip6tables -w 5 -t filter -C OUTPUT -j SPRK6_OUT"))
    }

    /** 生成统一全局夹具，避免各测试复制 UID 与范围配置。 */
    private fun globalCommand(): String = buildRootApplyCommand(
        RootRoutingPlan(
            applicationUid = 10234,
            captureScope = RootCaptureScope(emptySet(), captureUnselected = true),
        ),
    )
}
