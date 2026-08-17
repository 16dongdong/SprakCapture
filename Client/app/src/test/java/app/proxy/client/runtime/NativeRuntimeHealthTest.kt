package app.proxy.client.runtime

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** 验证异步 Native 致命状态不会被统计轮询吞掉。 */
class NativeRuntimeHealthTest {
    /** 健康快照为空时不产生异常，采样可以继续读取固定五字段 ABI。 */
    @Test
    fun healthySnapshotContinuesSampling() {
        assertNull(runCatching { requireHealthyRuntime(null) }.exceptionOrNull())
    }

    /** 监听线程致命文本必须原样抛出，服务层才能展示原因并停止全部数据面。 */
    @Test
    fun fatalSnapshotStopsSampling() {
        val failure = runCatching { requireHealthyRuntime("Native TCP 监听线程异常退出") }.exceptionOrNull()

        assertEquals("Native TCP 监听线程异常退出", failure?.message)
    }
}
