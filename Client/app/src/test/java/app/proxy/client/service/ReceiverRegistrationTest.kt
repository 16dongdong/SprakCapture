package app.proxy.client.service

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证包广播注册句柄在服务销毁和重复调用时不会泄漏或重复注销。 */
class ReceiverRegistrationTest {
    /** 正常启动后停止只调用一次注销动作，第二次停止保持幂等。 */
    @Test
    fun stopUnregistersExactlyOnce() {
        var unregisterCalls = 0
        val registration = ReceiverRegistration()
        registration.start { { unregisterCalls += 1 } }

        registration.stop()
        registration.stop()

        assertEquals(1, unregisterCalls)
    }

    /** 重复启动必须保留首个句柄并抛错，随后仍能注销原注册。 */
    @Test
    fun duplicateStartPreservesOriginalRegistration() {
        var unregisterCalls = 0
        val registration = ReceiverRegistration()
        registration.start { { unregisterCalls += 1 } }

        val failure = runCatching { registration.start { {} } }.exceptionOrNull()
        registration.stop()

        assertTrue(failure?.message?.contains("已经启动") == true)
        assertEquals(1, unregisterCalls)
    }

    /** 注册动作失败时不保存伪句柄，后续停止保持空操作且允许重新注册。 */
    @Test
    fun failedStartDoesNotCreateRegistration() {
        var unregisterCalls = 0
        val registration = ReceiverRegistration()
        runCatching { registration.start { error("注册失败") } }

        registration.stop()
        registration.start { { unregisterCalls += 1 } }
        registration.stop()

        assertEquals(1, unregisterCalls)
    }
}
