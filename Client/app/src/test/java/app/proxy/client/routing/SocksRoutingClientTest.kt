package app.proxy.client.routing

import app.proxy.client.domain.AccountCredentials
import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.domain.EmbeddedNode
import java.io.BufferedInputStream
import java.io.ByteArrayInputStream
import java.net.ServerSocket
import java.util.Base64
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证规则请求真实经过 RFC 1929 SOCKS5，并在 HTTP 层复用同一份 Basic 凭据。 */
class SocksRoutingClientTest {
    /** CONNECT 请求必须按地址类型编码，防止 IPv4/IPv6 被错误当成待解析域名。 */
    @Test
    fun connectRequestUsesMatchingAddressType() {
        val client = SocksRoutingClient()
        val ipv4 = client.createConnectRequest("192.0.2.8", 19090)
        val domain = client.createConnectRequest("rules.example", 19090)
        val ipv6 = client.createConnectRequest("2001:db8::8", 19090)

        assertEquals(1, ipv4[3].toInt())
        assertEquals(listOf(192, 0, 2, 8), ipv4.slice(4..7).map { it.toInt() and 0xff })
        assertEquals(3, domain[3].toInt())
        assertEquals("rules.example", String(domain, 5, domain[4].toInt(), Charsets.US_ASCII))
        assertEquals(4, ipv6[3].toInt())
        assertEquals(22, ipv6.size)
    }

    /** 形似 IPv4 的非规范文本不得降级为域名交给上游 DNS，避免不同解析器解释前导零。 */
    @Test
    fun ambiguousIpv4DestinationIsRejected() {
        val failure = runCatching { SocksRoutingClient().createConnectRequest("192.000.2.8", 19090) }
            .exceptionOrNull()

        assertTrue(failure?.message?.contains("IPv4 字面量无效") == true)
    }

    /** HTTP Host 对 IPv6 必须保留方括号，IPv4 和域名不得增加括号。 */
    @Test
    fun httpHostBracketsIpv6Literal() {
        val client = SocksRoutingClient()
        assertEquals("[2001:db8::8]", client.formatHttpHost("2001:db8::8"))
        assertEquals("[2001:db8::8]", client.formatHttpHost("[2001:db8::8]"))
        assertEquals("rules.example", client.formatHttpHost("rules.example"))
    }

    /** CONNECT 响应 RSV 必须为零，非零值不能被忽略后继续发送含凭据的 HTTP 请求。 */
    @Test
    fun connectReplyRejectsNonZeroReservedField() {
        val failure = consumeConnectReply(byteArrayOf(5, 0, 1, 1, 127, 0, 0, 1, 0, 80)).exceptionOrNull()

        assertTrue(failure?.message?.contains("非零保留字段") == true)
    }

    /** ATYP=DOMAIN 的长度字段必须为 1..255，零长度不是合法 BND 地址。 */
    @Test
    fun connectReplyRejectsEmptyDomain() {
        val failure = consumeConnectReply(byteArrayOf(5, 0, 0, 3, 0, 0, 80)).exceptionOrNull()

        assertTrue(failure?.message?.contains("空 BND 域名") == true)
    }

    /** 未知 ATYP 必须终止握手，客户端不能猜测地址长度后错位读取端口。 */
    @Test
    fun connectReplyRejectsUnknownAddressType() {
        val failure = consumeConnectReply(byteArrayOf(5, 0, 0, 9)).exceptionOrNull()

        assertTrue(failure?.message?.contains("未知地址类型") == true)
    }

    /** BND.ADDR 或 BND.PORT 截断时必须报告连接提前结束，不能把后续 HTTP 字节补入 SOCKS 响应。 */
    @Test
    fun connectReplyRejectsTruncatedBoundAddress() {
        val failure = consumeConnectReply(byteArrayOf(5, 0, 0, 1, 127, 0)).exceptionOrNull()

        assertTrue(failure?.message?.contains("代理连接意外结束") == true)
    }

    /** REP 按无符号字节展示，0xff 必须诊断为 255 而不是负数。 */
    @Test
    fun connectReplyReportsUnsignedResponseCode() {
        val failure = consumeConnectReply(byteArrayOf(5, 0xff.toByte(), 0, 1)).exceptionOrNull()

        assertTrue(failure?.message?.contains("响应码 255") == true)
    }

    /** 客户端必须发送 SOCKS 认证、远端 CONNECT、Basic 和 ETag，再严格读取文本响应。 */
    @Test
    fun downloadsRuleThroughAuthenticatedSocksConnection() {
        ServerSocket(0).use { server ->
            val observedRequest = CompletableFuture<String>()
            val serverThread = thread(name = "routingSocksFixture") {
                server.accept().use { socket ->
                    val input = BufferedInputStream(socket.getInputStream())
                    val output = socket.getOutputStream()
                    assertEquals(listOf(5, 1, 2), readExact(input, 3).map { it.toInt() and 0xff })
                    output.write(byteArrayOf(5, 2))
                    output.flush()
                    val authHead = readExact(input, 2)
                    val username = String(readExact(input, authHead[1].toInt() and 0xff), Charsets.UTF_8)
                    val passwordLength = input.read()
                    val password = String(readExact(input, passwordLength), Charsets.UTF_8)
                    assertEquals("user", username)
                    assertEquals("pass", password)
                    output.write(byteArrayOf(1, 0))
                    output.flush()
                    val connectHead = readExact(input, 5)
                    val host = String(readExact(input, connectHead[4].toInt() and 0xff), Charsets.US_ASCII)
                    val port = readExact(input, 2).let { ((it[0].toInt() and 0xff) shl 8) or (it[1].toInt() and 0xff) }
                    assertEquals("rules.example", host)
                    assertEquals(19090, port)
                    output.write(byteArrayOf(5, 0, 0, 1, 127, 0, 0, 1, 0, 80))
                    output.flush()
                    val request = readHeaders(input)
                    observedRequest.complete(request)
                    val body = "[RoutingRule]\n[GRoutingRule]\nFINAL,PROXY\n[proxy_app]\n[DNS]\nPRIMARY,223.5.5.5\n"
                    output.write(
                        ("HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n" +
                            "Content-Length: ${body.toByteArray().size}\r\nETag: \"rules-3\"\r\n\r\n$body")
                            .toByteArray(Charsets.UTF_8),
                    )
                    output.flush()
                }
            }
            val profile = EmbeddedClientProfile(
                EmbeddedNode("127.0.0.1", server.localPort),
                AccountCredentials("user", "pass"),
                "http://rules.example:19090/api/v1/client/routing.txt",
            )

            val response = SocksRoutingClient().fetch(profile, "\"rules-2\"") as RoutingFetchResponse.Content
            val request = observedRequest.get(5, TimeUnit.SECONDS)
            serverThread.join(5_000)

            assertEquals("\"rules-3\"", response.etag)
            assertTrue(response.text.contains("[RoutingRule]"))
            assertTrue(request.contains("GET /api/v1/client/routing.txt HTTP/1.1"))
            assertTrue(request.contains("Authorization: Basic ${Base64.getEncoder().encodeToString("user:pass".toByteArray())}"))
            assertTrue(request.contains("If-None-Match: \"rules-2\""))
        }
    }

    /** 根证书请求必须复用同一 SOCKS/Basic 凭据、固定派生路径并接受二进制 DER 正文。 */
    @Test
    fun downloadsRootCertificateThroughAuthenticatedSocksConnection() {
        ServerSocket(0).use { server ->
            val observedRequest = CompletableFuture<String>()
            val certificate = byteArrayOf(0x30, 0x03, 0x02, 0x01, 0x01)
            val serverThread = thread(name = "certificateSocksFixture") {
                server.accept().use { socket ->
                    val input = BufferedInputStream(socket.getInputStream())
                    val output = socket.getOutputStream()
                    readExact(input, 3)
                    output.write(byteArrayOf(5, 2))
                    output.flush()
                    val authHead = readExact(input, 2)
                    readExact(input, authHead[1].toInt() and 0xff)
                    readExact(input, input.read())
                    output.write(byteArrayOf(1, 0))
                    output.flush()
                    val connectHead = readExact(input, 5)
                    readExact(input, connectHead[4].toInt() and 0xff)
                    readExact(input, 2)
                    output.write(byteArrayOf(5, 0, 0, 1, 127, 0, 0, 1, 0, 80))
                    output.flush()
                    observedRequest.complete(readHeaders(input))
                    output.write(
                        ("HTTP/1.1 200 OK\r\nContent-Type: application/pkix-cert\r\n" +
                            "Content-Length: ${certificate.size}\r\n\r\n").toByteArray(Charsets.US_ASCII),
                    )
                    output.write(certificate)
                    output.flush()
                }
            }
            val profile = EmbeddedClientProfile(
                EmbeddedNode("127.0.0.1", server.localPort),
                AccountCredentials("user", "pass"),
                "http://rules.example:19090/api/v1/client/routing.txt",
            )

            val downloaded = SocksRoutingClient().fetchRootCertificate(profile)
            val request = observedRequest.get(5, TimeUnit.SECONDS)
            serverThread.join(5_000)

            assertTrue(downloaded.contentEquals(certificate))
            assertTrue(request.contains("GET /api/v1/client/ca.cer HTTP/1.1"))
            assertTrue(request.contains("Accept: application/pkix-cert"))
            assertTrue(request.contains("Authorization: Basic ${Base64.getEncoder().encodeToString("user:pass".toByteArray())}"))
        }
    }

    /** 冲突 Content-Length 会制造两种正文边界，客户端必须在读取正文前拒绝。 */
    @Test
    fun conflictingContentLengthsAreRejected() {
        val response = validResponseHeaders("Content-Length: 1\r\nContent-Length: 2\r\n") + "a"

        val failure = parseHttpResponse(response).exceptionOrNull()

        assertTrue(failure?.message?.contains("冲突的 Content-Length") == true)
    }

    /** 非数字、负数和溢出的 Content-Length 都是协议错误，不得降级成读到连接关闭。 */
    @Test
    fun malformedContentLengthIsRejected() {
        listOf("-1", "invalid", "999999999999999999999999").forEach { length ->
            val failure = parseHttpResponse(validResponseHeaders("Content-Length: $length\r\n")).exceptionOrNull()
            assertTrue(failure?.message?.contains("Content-Length") == true)
        }
    }

    /** Transfer-Encoding 与 Content-Length 并存时边界不唯一，必须拒绝而非任选一种。 */
    @Test
    fun transferEncodingAndContentLengthAreRejected() {
        val response = validResponseHeaders("Transfer-Encoding: chunked\r\nContent-Length: 1\r\n") +
            "1\r\na\r\n0\r\n\r\n"

        val failure = parseHttpResponse(response).exceptionOrNull()

        assertTrue(failure?.message?.contains("不得同时包含") == true)
    }

    /** 分块长度只接受无符号十六进制；负值、0x 前缀和 Long 溢出都不能进入数组长度计算。 */
    @Test
    fun invalidChunkSizesAreRejected() {
        listOf("-1", "0x1", "FFFFFFFFFFFFFFFFF").forEach { chunkSize ->
            val response = validResponseHeaders("Transfer-Encoding: chunked\r\n") + "$chunkSize\r\n"
            val failure = parseHttpResponse(response).exceptionOrNull()
            assertTrue(failure?.message?.contains("分块长度") == true)
        }
    }

    /** 初始响应头和 trailer 共用字段预算，服务不能在正文结束后继续发送无界元数据。 */
    @Test
    fun trailersShareHeaderBudget() {
        val trailers = (1..128).joinToString(separator = "") { index -> "X-Trailer-$index: value\r\n" }
        val response = validResponseHeaders("Transfer-Encoding: chunked\r\n") + "0\r\n$trailers\r\n"

        val failure = parseHttpResponse(response).exceptionOrNull()

        assertTrue(failure?.message?.contains("响应头数量超过上限") == true)
    }

    /** 单行虽短但字段总量仍受响应级数量预算约束，避免慢速唯一头累积占用内存。 */
    @Test
    fun initialHeadersHaveCountBudget() {
        val headers = (1..128).joinToString(separator = "") { index -> "X-Header-$index: value\r\n" }
        val failure = parseHttpResponse(validResponseHeaders(headers)).exceptionOrNull()

        assertTrue(failure?.message?.contains("响应头数量超过上限") == true)
    }

    /** 构造包含固定文本类型和 ETag 的响应头，附加字段用于专门验证 HTTP 分帧边界。 */
    private fun validResponseHeaders(additionalHeaders: String): String =
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nETag: \"rules\"\r\n$additionalHeaders\r\n"

    /** 直接调用 HTTP 解析边界；输入完全驻留内存，失败不会启动网络夹具或遗留线程。 */
    private fun parseHttpResponse(response: String): Result<RoutingFetchResponse> = runCatching {
        ByteArrayInputStream(response.toByteArray(Charsets.US_ASCII)).use { source ->
            SocksRoutingClient().readHttpResponse(BufferedInputStream(source))
        }
    }

    /** 用内存字节流验证 SOCKS 响应解析；输入耗尽会稳定映射为协议截断。 */
    private fun consumeConnectReply(reply: ByteArray): Result<Unit> = runCatching {
        ByteArrayInputStream(reply).use { source ->
            SocksRoutingClient().consumeConnectReply(BufferedInputStream(source))
        }
    }

    /** 精确读取测试协议字段；提前 EOF 直接让测试失败，避免夹具伪造成功。 */
    private fun readExact(input: BufferedInputStream, length: Int): ByteArray {
        val bytes = ByteArray(length)
        var offset = 0
        while (offset < length) {
            val count = input.read(bytes, offset, length - offset)
            check(count > 0)
            offset += count
        }
        return bytes
    }

    /** 读取 HTTP 头直到 CRLFCRLF，测试不消费正文。 */
    private fun readHeaders(input: BufferedInputStream): String {
        val bytes = ArrayList<Byte>()
        while (bytes.takeLast(4) != listOf(13, 10, 13, 10).map(Int::toByte)) {
            val value = input.read()
            check(value >= 0)
            bytes.add(value.toByte())
        }
        return bytes.toByteArray().toString(Charsets.US_ASCII)
    }
}
