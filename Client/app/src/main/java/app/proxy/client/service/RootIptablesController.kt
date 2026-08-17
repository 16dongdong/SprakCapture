package app.proxy.client.service

import android.content.Context
import app.proxy.client.routing.RoutingRuleDocument
import app.proxy.client.runtime.NativeRuntimeConfiguration
import java.util.concurrent.TimeUnit

private const val outputChain = "SPRK_OUT"
private const val udpQueueChain = "SPRK_MOUT"
private const val ipv6GuardChain = "SPRK6_OUT"

// 这些名称只用于卸载曾发布过的 veth/TPROXY 版本；新链不再创建对应资源。
private const val legacyUdpPreroutingChain = "SPRK_MPRE"
private const val legacyUdpOwnershipChain = "SPRK_MOWN"
private const val legacyUdpInputGuardChain = "SPRK_UIN"
private const val legacyGlobalRoutingTable = 26001
private const val legacySelectedRoutingTable = 26002
private const val legacyDeliveryRoutingTable = 26003
private const val legacyGlobalRulePriority = 12001
private const val legacySelectedRulePriority = 12002
private const val legacyDeliveryRulePriority = 12003
private const val legacyGlobalVeth = "sprkGOut"
private const val legacySelectedVeth = "sprkSOut"

/**
 * 管理 Root 模式的 IPv4 TCP/UDP 透明捕获与 IPv6 防泄漏边界。
 * TCP 直接使用 NAT REDIRECT；UDP 先经 NFQUEUE 保存原始五元组，再由 NAT REDIRECT
 * 投递到回环 Native 端口。该顺序不依赖 Android 内核对本机 TPROXY 的支持。
 */
class RootIptablesController(private val context: Context) {
    private var applied = false

    /**
     * 使用同一生命周期内已验证的 UID 快照安装透明链；伴随进程和应用自身先 RETURN，避免上游连接回环。
     * 任一步失败都会完整清理并返回内核诊断，调用方不得发布运行状态。
     */
    @Synchronized
    internal fun apply(rules: RoutingRuleDocument, packageScope: PackageScopeSnapshot) {
        check(!applied) { "Root iptables 已经应用" }
        val plan = createPlan(rules, packageScope)
        clear().getOrThrow()
        runCatching { runRootCommand(buildRootApplyCommand(plan)) }
            .onFailure { failure -> clear().exceptionOrNull()?.let(failure::addSuppressed) }
            .getOrThrow()
        applied = true
    }

    /** 删除当前链及旧版 TPROXY/veth 资源；清理不完整时返回失败，禁止复用残留规则。 */
    @Synchronized
    fun clear(): Result<Unit> = runCatching {
        runRootCommand(buildRootCleanupCommand())
        applied = false
    }

    /** 从规则与包投影生成纯数字捕获计划；规则正文不会进入 shell。 */
    private fun createPlan(rules: RoutingRuleDocument, packageScope: PackageScopeSnapshot): RootRoutingPlan =
        RootRoutingPlan(
            applicationUid = context.applicationInfo.uid,
            captureScope = RootCaptureScope(packageScope.selectedUids, captureUnselected = rules.hasGlobalRules),
        )

    /** 执行有界 su 命令；超时强制终止，非零退出保留内核工具诊断供上层统一映射。 */
    private fun runRootCommand(command: String) {
        val process = ProcessBuilder("su", "-c", command).redirectErrorStream(true).start()
        if (!process.waitFor(commandTimeoutSeconds, TimeUnit.SECONDS)) {
            process.destroyForcibly()
            process.waitFor()
            error("Root 网络规则命令执行超时")
        }
        val output = process.inputStream.bufferedReader().use { it.readText().trim() }
        check(process.exitValue() == 0) { output.ifBlank { "Root 网络规则命令执行失败" } }
    }

    private companion object {
        const val commandTimeoutSeconds = 10L
    }
}

/** 保存一次 Root 规则事务需要的 UID 投影，确保生成 shell 时不接触规则正文。 */
internal data class RootRoutingPlan(val applicationUid: Int, val captureScope: RootCaptureScope)

/** 选中 UID 进入应用规则上下文；其余 UID 是否捕获由全局规则开关决定。 */
internal data class RootCaptureScope(val selectedUids: Set<Int>, val captureUnselected: Boolean)

/**
 * 生成先 NFQUEUE、后 REDIRECT 的 IPv4 事务与 IPv6 防泄漏链。
 * selected UID 必须先于全局兜底；NFQUEUE verdict 后立即 RETURN，避免同一包进入两个规则上下文。
 */
internal fun buildRootApplyCommand(plan: RootRoutingPlan): String = buildString {
    append("set -e; ")
    appendIpv4NatChain(plan)
    appendUdpQueueChain(plan)
    appendIpv6Guard(plan)
    append("iptables -w 5 -t mangle -I OUTPUT 1 -j $udpQueueChain; ")
    append("ip6tables -w 5 -t filter -I OUTPUT 1 -j $ipv6GuardChain; ")
    append("iptables -w 5 -t nat -I OUTPUT 1 -j $outputChain")
}

/** 创建 TCP/UDP REDIRECT 链；NFQUEUE 已在更早的 mangle OUTPUT 保存 UDP 原目标。 */
private fun StringBuilder.appendIpv4NatChain(plan: RootRoutingPlan) {
    append("iptables -w 5 -t nat -N $outputChain; ")
    appendCommonReturns("nat", outputChain, plan.applicationUid)
    plan.captureScope.selectedUids.sorted().forEach { uid ->
        appendRedirect(outputChain, uid, "tcp", NativeRuntimeConfiguration.selectedTransparentTcpPort)
        appendRedirect(outputChain, uid, "udp", NativeRuntimeConfiguration.selectedTransparentUdpPort)
    }
    if (plan.captureScope.captureUnselected) {
        appendRedirect(outputChain, null, "tcp", NativeRuntimeConfiguration.transparentTcpPort)
        appendRedirect(outputChain, null, "udp", NativeRuntimeConfiguration.transparentUdpPort)
    }
}

/** 创建 UDP 原目标捕获链；队列没有消费者时 queue-bypass 只用于故障恢复，不表示数据面成功。 */
private fun StringBuilder.appendUdpQueueChain(plan: RootRoutingPlan) {
    append("iptables -w 5 -t mangle -N $udpQueueChain; ")
    appendCommonReturns("mangle", udpQueueChain, plan.applicationUid)
    plan.captureScope.selectedUids.sorted().forEach { uid ->
        appendQueueDispatch(uid, NativeRuntimeConfiguration.selectedUdpQueueNumber)
    }
    if (plan.captureScope.captureUnselected) {
        appendQueueDispatch(null, NativeRuntimeConfiguration.globalUdpQueueNumber)
    }
}

/** 为给定链排除 Root、应用自身与回环目标；这些连接属于代理的数据面出口。 */
private fun StringBuilder.appendCommonReturns(table: String, chain: String, applicationUid: Int) {
    append("iptables -w 5 -t $table -A $chain -m owner --uid-owner 0 -j RETURN; ")
    append("iptables -w 5 -t $table -A $chain -m owner --uid-owner $applicationUid -j RETURN; ")
    append("iptables -w 5 -t $table -A $chain -d 127.0.0.0/8 -j RETURN; ")
}

/** 把单一 UID 或全局兜底送进指定队列，随后终止当前自有链继续匹配。 */
private fun StringBuilder.appendQueueDispatch(uid: Int?, queueNumber: Int) {
    val owner = uid?.let { " -m owner --uid-owner $it" }.orEmpty()
    append("iptables -w 5 -t mangle -A $udpQueueChain$owner -p udp ")
    append("-j NFQUEUE --queue-num $queueNumber --queue-bypass; ")
    append("iptables -w 5 -t mangle -A $udpQueueChain$owner -p udp -j RETURN; ")
}

/** 把单一 UID 或全局兜底的协议流量交给回环 Native 端口。 */
private fun StringBuilder.appendRedirect(chain: String, uid: Int?, protocol: String, port: Int) {
    val owner = uid?.let { " -m owner --uid-owner $it" }.orEmpty()
    append("iptables -w 5 -t nat -A $chain$owner -p $protocol -j REDIRECT --to-ports $port; ")
}

/** 为捕获范围安装 IPv6 显式拒绝；当前 Root 仅代理 IPv4，VPN 模式提供双栈代理。 */
private fun StringBuilder.appendIpv6Guard(plan: RootRoutingPlan) {
    append("ip6tables -w 5 -t filter -N $ipv6GuardChain; ")
    append("ip6tables -w 5 -t filter -A $ipv6GuardChain -m owner --uid-owner 0 -j RETURN; ")
    append("ip6tables -w 5 -t filter -A $ipv6GuardChain -m owner --uid-owner ${plan.applicationUid} -j RETURN; ")
    append("ip6tables -w 5 -t filter -A $ipv6GuardChain -d ::1/128 -j RETURN; ")
    plan.captureScope.selectedUids.sorted().forEach(::appendIpv6Reject)
    if (plan.captureScope.captureUnselected) appendIpv6Reject(null)
}

/** 为捕获范围内 IPv6 追加明确拒绝，避免 Root 模式静默绕过规则。 */
private fun StringBuilder.appendIpv6Reject(uid: Int?) {
    val owner = uid?.let { " -m owner --uid-owner $it" }.orEmpty()
    append("ip6tables -w 5 -t filter -A $ipv6GuardChain$owner -p tcp -j REJECT; ")
    append("ip6tables -w 5 -t filter -A $ipv6GuardChain$owner -p udp -j REJECT; ")
}

/**
 * 先解除系统挂接，再删除当前链；只有旧 marker 存在时才回收旧策略表和 veth，避免碰触外部资源。
 */
internal fun buildRootCleanupCommand(): String = buildString {
    append("owns_legacy=0; if iptables -w 5 -t mangle -S $legacyUdpOwnershipChain >/dev/null 2>&1; then owns_legacy=1; fi; ")
    appendDeleteJump(ChainReference("iptables", "nat", "OUTPUT", outputChain))
    appendDeleteJump(ChainReference("iptables", "mangle", "OUTPUT", udpQueueChain))
    appendDeleteJump(ChainReference("iptables", "mangle", "PREROUTING", legacyUdpPreroutingChain))
    appendDeleteJump(ChainReference("iptables", "filter", "INPUT", legacyUdpInputGuardChain))
    appendDeleteJump(ChainReference("ip6tables", "filter", "OUTPUT", ipv6GuardChain))
    appendDeleteJump(ChainReference("iptables", "filter", "OUTPUT", "SPRK_UDP_RJ"))
    appendLegacyPolicyCleanup()
    listOf(outputChain, "SPRK_TCP", "SPRK_STCP", "SPRK_UDP", "SPRK_SUDP").forEach {
        appendDropChain("iptables", "nat", it)
    }
    listOf(udpQueueChain, legacyUdpPreroutingChain, legacyUdpOwnershipChain).forEach {
        appendDropChain("iptables", "mangle", it)
    }
    appendDropChain("iptables", "filter", legacyUdpInputGuardChain)
    appendDropChain("ip6tables", "filter", ipv6GuardChain)
    appendDropChain("iptables", "filter", "SPRK_UDP_RJ")
    appendDeleteJump(ChainReference("iptables", "nat", "OUTPUT", "ROUTESOCKS"))
    appendDeleteJump(ChainReference("iptables", "filter", "OUTPUT", "ROUTESOCKS_RJ"))
    appendDropChain("iptables", "nat", "ROUTESOCKS")
    appendDropChain("iptables", "filter", "ROUTESOCKS_RJ")
    appendRootCleanupChecks()
}

/** 仅在旧 marker 证明所有权时删除旧策略规则、路由表与 veth。 */
private fun StringBuilder.appendLegacyPolicyCleanup() {
    append("if [ \"\$owns_legacy\" = 1 ]; then ")
    listOf(legacyGlobalRulePriority, legacySelectedRulePriority, legacyDeliveryRulePriority).forEach {
        append("while ip rule del pref $it 2>/dev/null; do :; done; ")
    }
    listOf(legacyGlobalRoutingTable, legacySelectedRoutingTable, legacyDeliveryRoutingTable).forEach {
        append("ip route flush table $it 2>/dev/null || true; ")
    }
    append("ip link del $legacyGlobalVeth 2>/dev/null || true; ")
    append("ip link del $legacySelectedVeth 2>/dev/null || true; fi; ")
}

/** 验证旧私有设备均已释放；残留直接失败。 */
private fun StringBuilder.appendRootCleanupChecks() {
    append("if ip link show $legacyGlobalVeth >/dev/null 2>&1 || ip link show $legacySelectedVeth >/dev/null 2>&1; then ")
    append("echo 'IPv4 私有回送设备仍然存在'; exit 1; fi")
}

/** 表达一个系统链挂接点，避免清理函数依赖易错的位置参数。 */
private data class ChainReference(val tool: String, val table: String, val parent: String, val chain: String)

/** 重复删除系统链跳转直到不存在；真实删除失败会终止清理事务。 */
private fun StringBuilder.appendDeleteJump(reference: ChainReference) {
    append("while ${reference.tool} -w 5 -t ${reference.table} -C ${reference.parent} -j ${reference.chain} 2>/dev/null; do ")
    append("${reference.tool} -w 5 -t ${reference.table} -D ${reference.parent} -j ${reference.chain} || ")
    append("{ echo '${reference.chain} 挂接删除失败'; exit 1; }; done; ")
}

/** 幂等清空并删除自有链；调用方已经解除全部系统挂接。 */
private fun StringBuilder.appendDropChain(tool: String, table: String, chain: String) {
    append("$tool -w 5 -t $table -F $chain 2>/dev/null || true; ")
    append("$tool -w 5 -t $table -X $chain 2>/dev/null || true; ")
}
