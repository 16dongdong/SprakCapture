package app.proxy.client.config

import java.io.ByteArrayOutputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证 Native 解封后的二进制 ABI 不会因截断、错版或尾随字节生成部分连接资料。 */
class EmbeddedNodeProviderTest {
    /** 合法大端资料应还原为一个不可分割快照，字段顺序与 Native/打包器合同一致。 */
    @Test
    fun authenticatedBinaryProfileIsParsed() {
        val expected = TestProfile(
            host = "192.0.2.8",
            port = 1080,
            username = "testAccount",
            password = "testPassword",
            rulesUrl = "http://192.0.2.9/routing.txt",
        )

        val profile = EmbeddedNodeProvider.parseDecrypted(profilePayload(expected))

        assertEquals(expected.host, profile.node.host)
        assertEquals(expected.port, profile.node.port)
        assertEquals(expected.username, profile.credentials.username)
        assertEquals(expected.password, profile.credentials.password)
        assertEquals(expected.rulesUrl, profile.rulesUrl)
    }

    /** 明文尾随字节表示 ABI 错版或拼接污染，必须在构造运行配置前拒绝。 */
    @Test
    fun trailingProfileBytesAreRejected() {
        val payload = profilePayload(validProfile()) + byteArrayOf(1)

        val failure = runCatching { EmbeddedNodeProvider.parseDecrypted(payload) }.exceptionOrNull()

        assertTrue(failure?.message?.contains("尾随字节") == true)
    }

    /** 字段长度超出剩余明文时必须精确失败，禁止用空值或截断字符串继续连接。 */
    @Test
    fun truncatedProfileFieldIsRejected() {
        val payload = profilePayload(validProfile()).copyOf(5)

        val failure = runCatching { EmbeddedNodeProvider.parseDecrypted(payload) }.exceptionOrNull()

        assertTrue(failure?.message?.contains("字段截断") == true)
    }

    /** 构造合法 ABI 明文；测试只使用 TEST-NET 地址，绝不复用真实节点或账号资料。 */
    private fun profilePayload(profile: TestProfile): ByteArray = ByteArrayOutputStream().use { output ->
        output.write(1)
        writeUtf8(output, profile.host)
        writeUnsignedShort(output, profile.port)
        writeUtf8(output, profile.username)
        writeUtf8(output, profile.password)
        writeUtf8(output, profile.rulesUrl)
        output.toByteArray()
    }

    /** 写入 u16 长度前缀 UTF-8 字段，保持测试夹具与生产解析器相反方向可验证。 */
    private fun writeUtf8(output: ByteArrayOutputStream, value: String) {
        val bytes = value.toByteArray(Charsets.UTF_8)
        writeUnsignedShort(output, bytes.size)
        output.write(bytes)
    }

    /** 按大端写入 ABI 的无符号短整数；测试值越界时立即失败。 */
    private fun writeUnsignedShort(output: ByteArrayOutputStream, value: Int) {
        require(value in 0..65535)
        output.write(value ushr 8)
        output.write(value)
    }

    /** 返回不含部署资料的统一合法夹具，供错误边界测试复用。 */
    private fun validProfile(): TestProfile = TestProfile(
        host = "192.0.2.8",
        port = 1080,
        username = "testAccount",
        password = "testPassword",
        rulesUrl = "http://192.0.2.9/routing.txt",
    )
}

/** 聚合测试资料，避免构造函数参数扩散并明确所有字段均为保留测试值。 */
private data class TestProfile(
    val host: String,
    val port: Int,
    val username: String,
    val password: String,
    val rulesUrl: String,
)
