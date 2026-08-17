package app.proxy.client.domain

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证节点与 RFC 1929 凭据边界，避免服务启动后才暴露不可编码输入。 */
class ClientModelsTest {
    /** 下载和打包契约要求嵌入非空密码，客户端再次校验可阻止损坏模板启动。 */
    @Test
    fun emptyPasswordIsRejected() {
        val error = AccountCredentials("account", "").validate().exceptionOrNull()
        assertEquals("密码不能为空且 UTF-8 长度不能超过 255 字节", error?.message)
    }

    /** 账号为空时无法生成 RFC 1929 报文，必须返回面向用户的精确失败。 */
    @Test
    fun emptyUsernameIsRejected() {
        val error = AccountCredentials("", "password").validate().exceptionOrNull()
        assertEquals("账号不能为空且 UTF-8 长度不能超过 255 字节", error?.message)
    }

    /** 首次连接 SOCKS 节点发生在规则下载之前，主机名不得触发规则外的系统 DNS。 */
    @Test
    fun embeddedNodeRequiresIpLiteral() {
        assertTrue(EmbeddedNode("192.0.2.8", 1080).validate().isSuccess)
        assertTrue(EmbeddedNode("2001:db8::8", 1080).validate().isSuccess)
        assertTrue(EmbeddedNode("proxy.example", 1080).validate().isFailure)
        assertTrue(EmbeddedNode("192.000.2.8", 1080).validate().isFailure)
    }

    /** 静态资料对象不得使用 data class 自动 toString，避免日志或异常插值意外输出连接资料。 */
    @Test
    fun secretBearingModelsDoNotRenderFields() {
        val credentials = AccountCredentials("privateAccount", "privatePassword")
        val profile = EmbeddedClientProfile(
            EmbeddedNode("192.0.2.8", 1080),
            credentials,
            "http://192.0.2.9/routing.txt",
        )

        assertTrue("privateAccount" !in credentials.toString())
        assertTrue("192.0.2.8" !in profile.toString())
    }
}
