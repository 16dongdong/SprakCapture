package app.proxy.client.runtime

import app.proxy.client.domain.AccountCredentials
import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.domain.EmbeddedNode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证 JNI 配置字段与固定入口端口，防止 Kotlin/C++ 升级后静默错位。 */
class NativeRuntimeConfigurationTest {
    private val internalCredentials = InternalSocksCredentials(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )

    /** 配置只携带 VPN SOCKS 与 Root IPv4 透明端口，禁止再次要求应用进程创建特权 IPv6 socket。 */
    @Test
    fun configurationContainsUnifiedRuntimeContract() {
        val configuration = NativeRuntimeConfiguration.create(
            EmbeddedClientProfile(
                EmbeddedNode("192.0.2.1", 1080),
                AccountCredentials("account", "password"),
                "http://192.0.2.1:19090/api/v1/client/routing.txt",
            ),
            internalCredentials,
        )
        val fields = configuration.lineSequence().filter(String::isNotBlank).associate { line ->
            line.substringBefore('=') to line.substringAfter('=')
        }

        assertEquals("192.0.2.1", fields["upstreamHost"])
        assertEquals("1080", fields["upstreamPort"])
        assertEquals("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", fields["localUsername"])
        assertEquals("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", fields["localPassword"])
        assertEquals(NativeRuntimeConfiguration.localSocksPort.toString(), fields["localSocksPort"])
        assertEquals(NativeRuntimeConfiguration.selectedSocksPort.toString(), fields["selectedSocksPort"])
        assertEquals(NativeRuntimeConfiguration.transparentTcpPort.toString(), fields["transparentTcpPort"])
        assertEquals(NativeRuntimeConfiguration.transparentUdpPort.toString(), fields["transparentUdpPort"])
        assertEquals(
            NativeRuntimeConfiguration.selectedTransparentTcpPort.toString(),
            fields["selectedTransparentTcpPort"],
        )
        assertEquals(
            NativeRuntimeConfiguration.selectedTransparentUdpPort.toString(),
            fields["selectedTransparentUdpPort"],
        )
        assertTrue(fields.keys.none { key -> key.contains("Ipv6") })
    }

    /** 打包结果不得包含空密码，避免 Native 配置产生 RFC 1929 零长度字段。 */
    @Test
    fun emptyPasswordIsRejectedBeforeConfiguration() {
        val profile = EmbeddedClientProfile(
            EmbeddedNode("192.0.2.1", 1080),
            AccountCredentials("account", ""),
            "http://192.0.2.1/routing.txt",
        )

        assertTrue(profile.validate().isFailure)
    }
}
