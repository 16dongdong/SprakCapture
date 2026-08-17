package app.proxy.client.runtime

import app.proxy.client.domain.ConnectionPhase
import app.proxy.client.domain.ProxyMode
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证热切换完成边界，避免服务刚进入 STARTING 就向界面报告成功。 */
class ProxyServiceControllerTest {
    /** 只有 RUNNING 和 FAILED 是目标启动终态，其余生命周期必须继续等待。 */
    @Test
    fun targetStartWaitsForTerminalOutcome() {
        assertFalse(isTargetStartCompleted(ConnectionPhase.STOPPED))
        assertFalse(isTargetStartCompleted(ConnectionPhase.STARTING))
        assertFalse(isTargetStartCompleted(ConnectionPhase.STOPPING))
        assertTrue(isTargetStartCompleted(ConnectionPhase.RUNNING))
        assertTrue(isTargetStartCompleted(ConnectionPhase.FAILED))
    }

    /** 停止只发送给正在持有或释放资源的服务，终态不得重新创建前台服务。 */
    @Test
    fun stopIntentOnlyTargetsLiveLifecycle() {
        assertFalse(shouldDispatchStop(ConnectionPhase.STOPPED))
        assertFalse(shouldDispatchStop(ConnectionPhase.FAILED))
        assertTrue(shouldDispatchStop(ConnectionPhase.STARTING))
        assertTrue(shouldDispatchStop(ConnectionPhase.RUNNING))
        assertTrue(shouldDispatchStop(ConnectionPhase.STOPPING))
    }

    /** 状态在整个超时窗口保持 STOPPED 时，取消代次仍必须让随后送达的 START 失效。 */
    @Test
    fun delayedStartIsRejectedAfterTimeoutCancellation() {
        val generation = StartRequestRegistry.create(ProxyMode.VPN)
        assertTrue(StartRequestRegistry.isActive(ProxyMode.VPN, generation))

        StartRequestRegistry.cancel(ProxyMode.VPN, generation)

        assertFalse(StartRequestRegistry.isActive(ProxyMode.VPN, generation))
    }

    /** 旧超时回调不得取消随后创建的新代次，避免快速重试被前一次清理误杀。 */
    @Test
    fun staleCancellationPreservesNewGeneration() {
        val staleGeneration = StartRequestRegistry.create(ProxyMode.ROOT)
        val activeGeneration = StartRequestRegistry.create(ProxyMode.ROOT)

        StartRequestRegistry.cancel(ProxyMode.ROOT, staleGeneration)

        assertFalse(StartRequestRegistry.isActive(ProxyMode.ROOT, staleGeneration))
        assertTrue(StartRequestRegistry.isActive(ProxyMode.ROOT, activeGeneration))
        StartRequestRegistry.cancel(ProxyMode.ROOT, activeGeneration)
    }
}
