package app.proxy.client.service

import app.proxy.client.domain.ProxyMode
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证 START_STICKY 恢复只允许持久模式对应的服务，防止热切换后旧组件复活。 */
class ServiceStartGenerationTest {
    /** VPN 切到 Root 后进程死亡时，旧 VPN 空 Intent 必须拒绝而 Root 可以恢复。 */
    @Test
    fun rootPreferenceRestoresOnlyRootService() {
        assertFalse(shouldRestoreService(true, ProxyMode.ROOT, ProxyMode.VPN))
        assertTrue(shouldRestoreService(true, ProxyMode.ROOT, ProxyMode.ROOT))
    }

    /** Root 切回 VPN 后进程死亡时，旧 Root 空 Intent 必须拒绝而 VPN 可以恢复。 */
    @Test
    fun vpnPreferenceRestoresOnlyVpnService() {
        assertFalse(shouldRestoreService(true, ProxyMode.VPN, ProxyMode.ROOT))
        assertTrue(shouldRestoreService(true, ProxyMode.VPN, ProxyMode.VPN))
    }

    /** 用户已关闭运行意图时两种服务都不得因 START_STICKY 自动恢复。 */
    @Test
    fun disabledIntentRestoresNeitherService() {
        assertFalse(shouldRestoreService(false, ProxyMode.VPN, ProxyMode.VPN))
        assertFalse(shouldRestoreService(false, ProxyMode.ROOT, ProxyMode.ROOT))
    }
}
