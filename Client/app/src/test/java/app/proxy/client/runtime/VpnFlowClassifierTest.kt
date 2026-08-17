package app.proxy.client.runtime

import java.net.InetAddress
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** 验证 HEV/Kotlin 五元组 ABI、UID 上下文选择和失败时拒绝合同。 */
class VpnFlowClassifierTest {
    /** 每个测试清除全局回调状态，避免并行或失败用例污染后续归属判断。 */
    @After
    fun clearClassifier() {
        VpnFlowClassifier.clear()
    }

    /** Native 固定 38 字节 IPv4 向量必须按网络序无损还原，供 ConnectivityManager 查询 owner UID。 */
    @Test
    fun ipv4FlowDecodesStableWireFormat() {
        val encoded = encodeFlow(FlowFixture(6, "198.18.0.1", 43120, "203.0.113.8", 443))
        val flow = VpnFlowClassifier.decodeFlow(encoded)

        assertEquals(6, flow?.protocol)
        assertEquals("198.18.0.1", flow?.localEndpoint?.address?.hostAddress)
        assertEquals(43120, flow?.localEndpoint?.port)
        assertEquals("203.0.113.8", flow?.remoteEndpoint?.address?.hostAddress)
        assertEquals(443, flow?.remoteEndpoint?.port)
    }

    /** 非对称端口固定向量锁定跨 SO 字节序：12345 必须是 `30 39`，443 必须是 `01 BB`。 */
    @Test
    fun fixedNativeVectorPreservesPortByteOrder() {
        val encoded = byteArrayOf(
            6, 4, 0x30, 0x39, 0x01, 0xbb.toByte(),
            198.toByte(), 18, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            203.toByte(), 0, 113, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        )
        val flow = VpnFlowClassifier.decodeFlow(encoded)

        assertEquals(12345, flow?.localEndpoint?.port)
        assertEquals(443, flow?.remoteEndpoint?.port)
        assertEquals("198.18.0.1", flow?.localEndpoint?.address?.hostAddress)
        assertEquals("203.0.113.8", flow?.remoteEndpoint?.address?.hostAddress)
    }

    /** 选中 UID 与普通 UID 必须分别返回两个 Native SOCKS 上下文，不能共用单端口。 */
    @Test
    fun ownerUidSelectsExpectedRoutingContext() {
        val encoded = encodeFlow(FlowFixture(17, "fc00::1", 53530, "2001:4860:4860::8888", 53))
        VpnFlowClassifier.configure(setOf(11001)) { 11001 }
        assertEquals(VpnFlowClassifier.selectedContext, VpnFlowClassifier.classify(encoded))

        VpnFlowClassifier.configure(setOf(11001)) { 12001 }
        assertEquals(VpnFlowClassifier.globalContext, VpnFlowClassifier.classify(encoded))
    }

    /** Native 总是启用 HEV 回调，纯全局与纯应用必须返回固定入口而不是因未配置 state 拒绝全部流。 */
    @Test
    fun fixedContextsBypassUidLookup() {
        VpnFlowClassifier.configureFixed(VpnFlowClassifier.globalContext)
        assertEquals(VpnFlowClassifier.globalContext, VpnFlowClassifier.classify(byteArrayOf()))

        VpnFlowClassifier.configureFixed(VpnFlowClassifier.selectedContext)
        assertEquals(VpnFlowClassifier.selectedContext, VpnFlowClassifier.classify(byteArrayOf()))
    }

    /** ABI 损坏、归属失败或生命周期未配置时必须拒绝，禁止默认走全局规则形成直连或越域。 */
    @Test
    fun invalidOrUnknownFlowIsRejected() {
        val encoded = encodeFlow(FlowFixture(6, "198.18.0.1", 43120, "203.0.113.8", 443))
        assertEquals(VpnFlowClassifier.rejectContext, VpnFlowClassifier.classify(encoded))

        VpnFlowClassifier.configure(setOf(11001)) { -1 }
        assertEquals(VpnFlowClassifier.rejectContext, VpnFlowClassifier.classify(encoded))
        assertNull(VpnFlowClassifier.decodeFlow(encoded.copyOf(encoded.size - 1)))
    }

    /** 生成与 HEV 固定一致的 38 字节五元组；IPv4 地址右侧补零，测试不复用生产解码器。 */
    private fun encodeFlow(fixture: FlowFixture): ByteArray {
        val localAddress = InetAddress.getByName(fixture.localHost).address
        val remoteAddress = InetAddress.getByName(fixture.remoteHost).address
        require(localAddress.size == remoteAddress.size)
        val encoded = ByteArray(38)
        encoded[0] = fixture.protocol.toByte()
        encoded[1] = if (localAddress.size == 4) 4 else 6
        encodePort(fixture.localPort).copyInto(encoded, 2)
        encodePort(fixture.remotePort).copyInto(encoded, 4)
        localAddress.copyInto(encoded, 6)
        remoteAddress.copyInto(encoded, 22)
        return encoded
    }

    /** 按网络字节序编码端口，保证 16 位无符号边界与 Native 一致。 */
    private fun encodePort(port: Int): ByteArray = byteArrayOf((port ushr 8).toByte(), port.toByte())

    /** 汇总测试五元组，保持辅助函数参数边界并让每个字段语义在调用点清晰可见。 */
    private data class FlowFixture(
        val protocol: Int,
        val localHost: String,
        val localPort: Int,
        val remoteHost: String,
        val remotePort: Int,
    )
}
