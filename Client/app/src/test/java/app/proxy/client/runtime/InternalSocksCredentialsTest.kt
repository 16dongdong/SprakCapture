package app.proxy.client.runtime

import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证回环 SOCKS5 凭据每个数据面代次独立，且可安全进入逐行配置。 */
class InternalSocksCredentialsTest {
    /** 连续启动必须生成不同的账号和密码，并保持 RFC 1929 单字节长度边界。 */
    @Test
    fun eachDataPlaneGenerationUsesIndependentCredentials() {
        val first = InternalSocksCredentials.generate()
        val second = InternalSocksCredentials.generate()

        assertNotEquals(first.username, second.username)
        assertNotEquals(first.password, second.password)
        listOf(first.username, first.password, second.username, second.password).forEach { token ->
            assertTrue(token.toByteArray(Charsets.UTF_8).size in 24..255)
            assertTrue(token.all { it.isLetterOrDigit() || it == '-' || it == '_' })
        }
    }
}
