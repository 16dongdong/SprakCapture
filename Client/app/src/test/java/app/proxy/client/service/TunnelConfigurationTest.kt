package app.proxy.client.service

import app.proxy.client.runtime.InternalSocksCredentials
import app.proxy.client.runtime.NativeRuntimeConfiguration
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证 HEV 只连接本地统一路由核心，不再持有上游凭据、远端 DNS 或分流语义。 */
class TunnelConfigurationTest {
    private val internalCredentials = InternalSocksCredentials(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )

    /** HEV 上游必须固定为回环 Native SOCKS 端口，避免 TUN 流量绕过统一规则核心。 */
    @Test
    fun tunnelAlwaysConnectsLocalRouteCore() {
        val yaml = TunnelConfiguration.create(internalCredentials)

        assertTrue(yaml.contains("address: '127.0.0.1'"))
        assertTrue(yaml.contains("port: ${NativeRuntimeConfiguration.localSocksPort}"))
    }

    /** Sprak Capture 使用标准 UDP ASSOCIATE，HEV 不得退回 UDP-over-TCP 私有扩展。 */
    @Test
    fun udpUsesStandardSocksAssociation() {
        val yaml = TunnelConfiguration.create(internalCredentials)

        assertTrue(yaml.contains("udp: 'udp'"))
        assertFalse(yaml.contains("udp: 'tcp'"))
    }

    /** HEV 只持有本次回环凭据，不得泄露远端账号或恢复 mapdns 第二套 DNS 语义。 */
    @Test
    fun tunnelDoesNotContainRemoteSecretsOrDnsMapping() {
        val yaml = TunnelConfiguration.create(internalCredentials)

        assertTrue(yaml.contains("username: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'"))
        assertTrue(yaml.contains("password: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'"))
        assertFalse(yaml.contains("remote-account"))
        assertFalse(yaml.contains("mapdns:"))
    }
}
