package app.proxy.client.runtime

import java.net.InetAddress
import java.net.InetSocketAddress

/**
 * 为 HEV 的新建 TCP/UDP 会话选择 Native 规则上下文。
 * 纯模式返回固定上下文；混合模式查询 UID，五元组无法归属时返回 REJECT，禁止把未知应用静默送入全局规则。
 */
object VpnFlowClassifier {
    @Volatile private var classifierState: ClassifierState? = null

    /** 安装混合规则所需的 UID 集合与系统归属查询器；空集合表示配置错误并直接拒绝启动。 */
    fun configure(selectedUids: Set<Int>, ownershipResolver: FlowOwnershipResolver) {
        require(selectedUids.isNotEmpty()) { "混合规则缺少 proxy_app UID" }
        classifierState = ClassifierState(selectedUids.toSet(), ownershipResolver, fixedContext = null)
    }

    /**
     * 为纯全局或纯应用规则安装固定上下文。
     * Native 始终注册 HEV 回调，因此纯模式也必须显式返回有效入口，不能用 `clear` 表示无需分类。
     */
    fun configureFixed(context: Int) {
        require(context == globalContext || context == selectedContext) { "VPN 固定规则上下文无效" }
        classifierState = ClassifierState(emptySet(), ownershipResolver = null, fixedContext = context)
    }

    /** 停止 HEV 后清除归属查询状态；并发到达的迟到回调会返回 REJECT，不会访问已销毁服务。 */
    fun clear() {
        classifierState = null
    }

    /**
     * 接收 HEV 按固定网络字节序编码的原始五元组并返回规则上下文。
     * 返回 GLOBAL/SELECTED 表示对应 SOCKS 入口，REJECT 表示结构损坏、系统无法归属或生命周期已停止。
     */
    @JvmStatic
    fun classify(encodedFlow: ByteArray): Int {
        val state = classifierState ?: return rejectContext
        state.fixedContext?.let { return it }
        val flow = decodeFlow(encodedFlow) ?: return rejectContext
        val resolver = state.ownershipResolver ?: return rejectContext
        val ownerUid = runCatching { resolver.resolve(flow) }.getOrElse { return rejectContext }
        if (ownerUid < 0) return rejectContext
        return if (ownerUid in state.selectedUids) selectedContext else globalContext
    }

    /** 解码固定 38 字节五元组；严格校验长度和地址族，防止不同 HEV 构建使用不兼容布局后错误分流。 */
    internal fun decodeFlow(encodedFlow: ByteArray): VpnFlowTuple? = runCatching {
        require(encodedFlow.size == encodedFlowSize)
        val protocol = encodedFlow.readUnsigned(protocolOffset)
        require(protocol == tcpProtocol || protocol == udpProtocol)
        val family = encodedFlow.readUnsigned(familyOffset)
        require(family == ipv4Family || family == ipv6Family)
        val addressLength = if (family == ipv4Family) ipv4AddressLength else ipv6AddressLength
        val localAddress = InetAddress.getByAddress(
            encodedFlow.copyOfRange(sourceAddressOffset, sourceAddressOffset + addressLength),
        )
        val remoteAddress = InetAddress.getByAddress(
            encodedFlow.copyOfRange(destinationAddressOffset, destinationAddressOffset + addressLength),
        )
        VpnFlowTuple(
            protocol = protocol,
            localEndpoint = InetSocketAddress(localAddress, encodedFlow.readPort(sourcePortOffset)),
            remoteEndpoint = InetSocketAddress(remoteAddress, encodedFlow.readPort(destinationPortOffset)),
        )
    }.getOrNull()

    private data class ClassifierState(
        val selectedUids: Set<Int>,
        val ownershipResolver: FlowOwnershipResolver?,
        val fixedContext: Int?,
    )

    const val globalContext = 0
    const val selectedContext = 1
    const val rejectContext = -1
    private const val tcpProtocol = 6
    private const val udpProtocol = 17
    private const val ipv4Family = 4
    private const val ipv6Family = 6
    private const val ipv4AddressLength = 4
    private const val ipv6AddressLength = 16
    private const val protocolOffset = 0
    private const val familyOffset = 1
    private const val sourcePortOffset = 2
    private const val destinationPortOffset = 4
    private const val sourceAddressOffset = 6
    private const val destinationAddressOffset = 22
    private const val encodedFlowSize = 38
}

/** 隔离 Android 连接归属 API，便于对五元组 ABI 和拒绝语义进行纯 JVM 单元测试。 */
fun interface FlowOwnershipResolver {
    /** 返回五元组所属 UID；负数或系统异常均由分类器转换为 REJECT。 */
    fun resolve(flow: VpnFlowTuple): Int
}

/** 保存从 TUN 数据包恢复出的 IP 协议、本地端点和远端端点。 */
data class VpnFlowTuple(
    val protocol: Int,
    val localEndpoint: InetSocketAddress,
    val remoteEndpoint: InetSocketAddress,
)

/** 无符号读取协议字段；索引越界由上层严格解码转换为无效五元组。 */
private fun ByteArray.readUnsigned(index: Int): Int = this[index].toInt() and 0xff

/** 读取两字节网络序端口；端口零也交给系统归属 API 决定，避免在 ABI 层臆测传输状态。 */
private fun ByteArray.readPort(offset: Int): Int = (readUnsigned(offset) shl 8) or readUnsigned(offset + 1)
