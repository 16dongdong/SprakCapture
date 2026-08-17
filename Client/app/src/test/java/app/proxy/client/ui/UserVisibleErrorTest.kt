package app.proxy.client.ui

import app.proxy.client.domain.userVisibleProxyError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/** 验证底层连接诊断不会把节点地址、端口或来源地址传播到用户界面。 */
class UserVisibleErrorTest {
    /** 网络异常只保留连接类别，完整端点文本不得进入界面文案。 */
    @Test
    fun networkFailureHidesEndpoint() {
        val raw = "failed to connect to /38.246.237.192 (port 1080) from /192.168.2.6 (port 52262) after 10000ms"

        val visible = userVisibleProxyError(raw, "代理数据面运行失败")

        assertEquals("代理服务器连接失败，请检查网络后重试", visible)
        assertFalse(visible.contains("38.246.237.192"))
        assertFalse(visible.contains("1080"))
    }

    /** 规则同步失败保留用户可执行的状态信息，但不传播底层 HTTP/SOCKS 细节。 */
    @Test
    fun ruleFailureUsesStableMessage() {
        val visible = userVisibleProxyError("规则下载失败：HTTP 503 http://node.example:19090", "未知")

        assertEquals("云规则更新失败，已继续使用上次有效规则", visible)
        assertFalse(visible.contains("node.example"))
    }
}
