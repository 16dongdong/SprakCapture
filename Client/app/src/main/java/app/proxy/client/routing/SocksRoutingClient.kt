package app.proxy.client.routing

import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.domain.IpAddressLiteral
import java.io.BufferedInputStream
import java.io.ByteArrayOutputStream
import java.net.IDN
import java.net.InetSocketAddress
import java.net.Socket
import java.net.URI
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.Base64

/** 通过安装包内置 SOCKS5 节点拉取规则，避免客户端自身绕过代理直连管理 HTTP 服务。 */
class SocksRoutingClient : RoutingRuleSource {
    /**
     * 完成 RFC 1929 认证、SOCKS5 CONNECT 和单次 HTTP GET。
     * `etag` 仅来自上次已验证响应；网络、协议、状态码或 UTF-8 异常均直接抛出且不返回半份规则。
     */
    override fun fetch(profile: EmbeddedClientProfile, etag: String?): RoutingFetchResponse {
        profile.validate().getOrThrow()
        val request = HttpFetchRequest(
            uri = URI(profile.rulesUrl),
            accept = "text/plain",
            etag = etag,
        )
        return readRoutingResponse(executeHttpGet(profile, request))
    }

    /**
     * 使用与规则下载相同的 SOCKS5 和 HTTP Basic 凭据读取当前根证书。
     * 证书端点固定派生自已验证规则地址，客户端没有可编辑 URL；401、非 DER 或超限正文均阻止信任安装。
     */
    fun fetchRootCertificate(profile: EmbeddedClientProfile): ByteArray {
        profile.validate().getOrThrow()
        val rulesUri = URI(profile.rulesUrl)
        val certificateUri = URI(
            rulesUri.scheme,
            null,
            rulesUri.host,
            rulesUri.port,
            rootCertificatePath,
            null,
            null,
        )
        val response = executeHttpGet(
            profile,
            HttpFetchRequest(certificateUri, "application/pkix-cert"),
        )
        check(response.status == 200) { "根证书下载失败，HTTP ${response.status}" }
        val contentType = singleHeader(response.headers, "content-type").orEmpty().lowercase()
        check(contentType.substringBefore(';').trim() == "application/pkix-cert") {
            "根证书服务返回了无效内容类型"
        }
        check(response.body.isNotEmpty()) { "根证书正文为空" }
        return response.body
    }

    /** 完成一次认证 SOCKS5 CONNECT 与有界 HTTP GET；返回值尚未按具体资源语义解释。 */
    private fun executeHttpGet(profile: EmbeddedClientProfile, request: HttpFetchRequest): HttpFetchResponse {
        val destinationPort = resolveHttpPort(request.uri)
        Socket().use { socket ->
            socket.soTimeout = ioTimeoutMillis
            val nodeAddress = checkNotNull(IpAddressLiteral.parse(profile.node.host)) { "SOCKS5 节点地址无效" }
            socket.connect(InetSocketAddress(nodeAddress, profile.node.port), connectTimeoutMillis)
            val input = BufferedInputStream(socket.getInputStream())
            val output = socket.getOutputStream()
            negotiateSocks(profile, input, output)
            connectDestination(request.uri.host, destinationPort, input, output)
            writeHttpRequest(profile, request, output)
            return readHttpEnvelope(input)
        }
    }

    /** 使用打包器已验证的非空用户名密码建立 SOCKS 会话；此层保持 UTF-8 字节不变且不替换凭据。 */
    private fun negotiateSocks(
        profile: EmbeddedClientProfile,
        input: BufferedInputStream,
        output: java.io.OutputStream,
    ) {
        output.write(byteArrayOf(5, 1, 2))
        output.flush()
        val greeting = readExact(input, 2)
        check(greeting[0].toInt() == 5 && greeting[1].toInt() == 2) { "SOCKS5 服务未接受账号认证" }
        val username = profile.credentials.username.toByteArray(Charsets.UTF_8)
        val password = profile.credentials.password.toByteArray(Charsets.UTF_8)
        output.write(byteArrayOf(1, username.size.toByte()))
        output.write(username)
        output.write(password.size)
        output.write(password)
        output.flush()
        val authentication = readExact(input, 2)
        check(authentication[0].toInt() == 1 && authentication[1].toInt() == 0) { "SOCKS5 账号认证失败" }
    }

    /** 请求 SOCKS 服务连接规则 HTTP 主机；域名原样交给服务端解析，避免本机 DNS 绕过代理。 */
    private fun connectDestination(
        host: String,
        port: Int,
        input: BufferedInputStream,
        output: java.io.OutputStream,
    ) {
        output.write(createConnectRequest(host, port))
        output.flush()
        consumeConnectReply(input)
    }

    /**
     * 严格消费 RFC 1928 CONNECT 响应的 VER/REP/RSV/BND.ADDR/BND.PORT。
     * RSV 非零、零长度域名、未知 ATYP 或截断边界均视为协议失败，不能在未读完整响应时发送 HTTP。
     */
    internal fun consumeConnectReply(input: BufferedInputStream) {
        val replyHead = readExact(input, 4)
        check((replyHead[0].toInt() and 0xff) == 5) { "SOCKS5 返回了无效协议版本" }
        val responseCode = replyHead[1].toInt() and 0xff
        check(responseCode == 0) { "SOCKS5 连接规则服务失败，响应码 $responseCode" }
        check((replyHead[2].toInt() and 0xff) == 0) { "SOCKS5 返回了非零保留字段" }
        val addressLength = when (replyHead[3].toInt() and 0xff) {
            1 -> 4
            3 -> (readExact(input, 1)[0].toInt() and 0xff).also { domainLength ->
                require(domainLength in 1..255) { "SOCKS5 返回了空 BND 域名" }
            }
            4 -> 16
            else -> error("SOCKS5 返回了未知地址类型")
        }
        // BND.PORT 即使业务不使用也必须完整消费，防止残留字节污染紧随其后的 HTTP 状态行。
        readExact(input, addressLength + 2)
    }

    /**
     * 按目标字面量选择 RFC 1928 ATYP：IPv4=1、域名=3、IPv6=4。
     * 域名不在客户端解析，IPv6 只解析含冒号的字面量，因此不会产生绕过 SOCKS 的 DNS 请求。
     */
    internal fun createConnectRequest(host: String, port: Int): ByteArray {
        require(port in 1..65535) { "规则地址端口无效" }
        val normalizedHost = host.removeSurrounding("[", "]")
        val literalAddress = IpAddressLiteral.parse(normalizedHost)
        require(literalAddress != null || !normalizedHost.looksLikeIpv4()) { "规则地址 IPv4 字面量无效" }
        val addressField = when {
            literalAddress?.address?.size == 4 -> byteArrayOf(1) + literalAddress.address
            ':' in normalizedHost -> {
                require(literalAddress?.address?.size == 16) { "规则地址 IPv6 字面量无效" }
                byteArrayOf(4) + literalAddress.address
            }
            else -> {
                val domain = IDN.toASCII(normalizedHost).toByteArray(Charsets.US_ASCII)
                require(domain.size in 1..255) { "规则地址域名长度超出 SOCKS5 边界" }
                byteArrayOf(3, domain.size.toByte()) + domain
            }
        }
        return byteArrayOf(5, 1, 0) + addressField + byteArrayOf((port ushr 8).toByte(), port.toByte())
    }

    /**
     * 写入不可重定向的 HTTP/1.1 请求；Basic 凭据与 SOCKS5 使用同一打包快照。
     * Host 端口直接从同一 URI 推导，避免调用方传入与 CONNECT 目标不一致的冗余参数。
     */
    private fun writeHttpRequest(
        profile: EmbeddedClientProfile,
        requestSpec: HttpFetchRequest,
        output: java.io.OutputStream,
    ) {
        val uri = requestSpec.uri
        val port = resolveHttpPort(uri)
        require(requestSpec.etag == null || ('\r' !in requestSpec.etag && '\n' !in requestSpec.etag)) {
            "资源缓存 ETag 包含非法换行"
        }
        require(requestSpec.accept.all { it.code in 0x21..0x7e }) { "资源 Accept 包含非法字符" }
        val path = buildString {
            append(uri.rawPath.ifEmpty { "/" })
            uri.rawQuery?.let { append('?').append(it) }
        }
        val credentials = "${profile.credentials.username}:${profile.credentials.password}"
        val authorization = Base64.getEncoder().encodeToString(credentials.toByteArray(Charsets.UTF_8))
        val request = buildString {
            append("GET ").append(path).append(" HTTP/1.1\r\n")
            append("Host: ").append(formatHttpHost(uri.host))
            if (port != 80) append(':').append(port)
            append("\r\nAuthorization: Basic ").append(authorization)
            append("\r\nAccept: ").append(requestSpec.accept).append("\r\nConnection: close\r\n")
            requestSpec.etag?.let { append("If-None-Match: ").append(it).append("\r\n") }
            append("\r\n")
        }
        output.write(request.toByteArray(Charsets.US_ASCII))
        output.flush()
    }

    /** 将规则 HTTP URI 的缺省端口规范为 80；URI 已在配置边界拒绝 HTTPS 和非法显式端口。 */
    private fun resolveHttpPort(uri: URI): Int = if (uri.port == -1) 80 else uri.port

    /** 生成 HTTP Host 字段；IPv6 字面量必须使用方括号，域名和 IPv4 保持原样。 */
    internal fun formatHttpHost(host: String): String {
        val normalizedHost = host.removeSurrounding("[", "]")
        return if (':' in normalizedHost) "[$normalizedHost]" else normalizedHost
    }

    /**
     * 解析有界 HTTP 响应；状态行、响应头和 trailer 共用同一预算，避免服务在超时窗口内持续发送唯一头耗尽内存。
     * 只接受 200 规则正文和 304 未修改，其他状态保留服务端错误码。
     */
    internal fun readHttpResponse(input: BufferedInputStream): RoutingFetchResponse =
        readRoutingResponse(readHttpEnvelope(input))

    /** 把通用 HTTP 响应解释为规则协议；304 禁止正文，200 必须是带唯一 ETag 的 UTF-8 文本。 */
    private fun readRoutingResponse(response: HttpFetchResponse): RoutingFetchResponse {
        if (response.status == 304) return RoutingFetchResponse.NotModified
        check(response.status == 200) {
            val reason = response.body.toString(Charsets.UTF_8).take(maximumErrorTextLength)
            "规则下载失败，HTTP ${response.status}${if (reason.isBlank()) "" else "：$reason"}"
        }
        val contentType = singleHeader(response.headers, "content-type").orEmpty().lowercase()
        check(contentType.startsWith("text/plain")) { "规则服务返回了非文本内容" }
        val text = decodeUtf8(response.body)
        return RoutingFetchResponse.Content(
            singleHeader(response.headers, "etag") ?: error("规则响应缺少 ETag"),
            text,
        )
    }

    /** 解析通用有界 HTTP 响应；状态、头和正文分帧只解释一次，具体资源再校验内容类型与状态。 */
    private fun readHttpEnvelope(input: BufferedInputStream): HttpFetchResponse {
        val headerBudget = HttpHeaderBudget()
        val statusLine = readLine(input, headerBudget)
        val statusMatch = httpStatusPattern.matchEntire(statusLine)
            ?: error("规则服务返回了无效 HTTP 状态行")
        val status = statusMatch.groupValues[1].toInt()
        val headers = linkedMapOf<String, MutableList<String>>()
        while (true) {
            val line = readLine(input, headerBudget, countAsHeader = true)
            if (line.isEmpty()) break
            val separator = line.indexOf(':')
            require(separator > 0) { "规则服务返回了无效 HTTP 响应头" }
            val name = line.substring(0, separator).lowercase()
            require(name.matches(httpHeaderNamePattern)) { "规则服务返回了无效 HTTP 响应头名称" }
            headers.getOrPut(name, ::mutableListOf).add(line.substring(separator + 1).trim())
        }
        val framing = parseBodyFraming(headers)
        if (status == 304) {
            require(framing == HttpBodyFraming.ContentLength(0) || framing == HttpBodyFraming.ConnectionClose) {
                "HTTP 304 响应不得携带正文"
            }
            return HttpFetchResponse(status, headers, ByteArray(0))
        }
        val body = readBody(input, framing, headerBudget)
        return HttpFetchResponse(status, headers, body)
    }

    /**
     * 解析正文分帧并拒绝 TE/CL 歧义；重复 Content-Length 只有全部规范十进制值一致时才可接受。
     * Transfer-Encoding 仅支持单一 chunked，禁止客户端与服务端对消息边界产生不同解释。
     */
    private fun parseBodyFraming(headers: Map<String, List<String>>): HttpBodyFraming {
        val contentLengths = headers["content-length"].orEmpty().flatMap { value ->
            value.split(',').map(String::trim)
        }
        val parsedLengths = contentLengths.map { value ->
            require(value.isNotEmpty() && value.all(Char::isDigit)) { "规则响应 Content-Length 无效" }
            value.toLongOrNull()?.also { length ->
                require(length <= maximumBodyBytes) { "规则响应长度越界" }
            } ?: error("规则响应 Content-Length 溢出")
        }
        require(parsedLengths.distinct().size <= 1) { "规则响应包含冲突的 Content-Length" }
        val transferCodings = headers["transfer-encoding"].orEmpty().flatMap { value ->
            value.split(',').map(String::trim)
        }
        require(parsedLengths.isEmpty() || transferCodings.isEmpty()) {
            "规则响应不得同时包含 Transfer-Encoding 和 Content-Length"
        }
        if (transferCodings.isNotEmpty()) {
            require(transferCodings.size == 1 && transferCodings.single().equals("chunked", ignoreCase = true)) {
                "规则响应 Transfer-Encoding 不受支持"
            }
            return HttpBodyFraming.Chunked
        }
        return parsedLengths.singleOrNull()?.let { HttpBodyFraming.ContentLength(it.toInt()) }
            ?: HttpBodyFraming.ConnectionClose
    }

    /** 按已验证分帧读取正文；连接关闭模式仍受 1 MiB 总长度约束。 */
    private fun readBody(
        input: BufferedInputStream,
        framing: HttpBodyFraming,
        headerBudget: HttpHeaderBudget,
    ): ByteArray = when (framing) {
        is HttpBodyFraming.ContentLength -> readExact(input, framing.length)
        HttpBodyFraming.Chunked -> readChunkedBody(input, headerBudget)
        HttpBodyFraming.ConnectionClose -> readUntilConnectionClose(input)
    }

    /** 严格读取十六进制分块和 trailer；负号、0x 前缀、溢出、裸 LF 与超预算 trailer 均终止更新。 */
    private fun readChunkedBody(input: BufferedInputStream, headerBudget: HttpHeaderBudget): ByteArray {
        val body = ByteArrayOutputStream()
        while (true) {
            val chunkLine = readLine(input, headerBudget)
            val chunkSizeText = chunkLine.substringBefore(';')
            require(chunkSizeText.isNotEmpty() && chunkSizeText.all { it.isDigit() || it.lowercaseChar() in 'a'..'f' }) {
                "规则响应分块长度无效"
            }
            if (';' in chunkLine) {
                val extension = chunkLine.substringAfter(';')
                require(extension.isNotEmpty() && extension.all { it.code in 0x20..0x7e }) {
                    "规则响应分块扩展无效"
                }
            }
            val chunkSize = chunkSizeText.toLongOrNull(16) ?: error("规则响应分块长度溢出")
            require(chunkSize <= maximumBodyBytes - body.size()) { "规则响应长度越界" }
            if (chunkSize == 0L) {
                readTrailers(input, headerBudget)
                return body.toByteArray()
            }
            body.write(readExact(input, chunkSize.toInt()))
            check(readLine(input, headerBudget).isEmpty()) { "规则响应分块边界无效" }
        }
    }

    /** 读取并验证 trailer 字段；与初始头共享数量和字节预算，禁止第二段无界头部。 */
    private fun readTrailers(input: BufferedInputStream, headerBudget: HttpHeaderBudget) {
        while (true) {
            val line = readLine(input, headerBudget, countAsHeader = true)
            if (line.isEmpty()) return
            val separator = line.indexOf(':')
            require(separator > 0 && line.substring(0, separator).matches(httpHeaderNamePattern)) {
                "规则服务返回了无效 HTTP trailer"
            }
        }
    }

    /** 读取直到连接关闭；服务未提供显式长度时仍逐段检查累计上限。 */
    private fun readUntilConnectionClose(input: BufferedInputStream): ByteArray {
        val body = ByteArrayOutputStream()
        val buffer = ByteArray(8192)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) return body.toByteArray()
            require(body.size() + count <= maximumBodyBytes) { "规则响应长度越界" }
            body.write(buffer, 0, count)
        }
    }

    /**
     * 读取严格 CRLF 行并扣减响应级预算；裸 LF、非 ASCII、超长单行或超总量都视为协议损坏。
     * `countAsHeader` 只对初始头和 trailer 计数，状态行与 chunk 元数据仅消耗字节预算。
     */
    private fun readLine(
        input: BufferedInputStream,
        headerBudget: HttpHeaderBudget,
        countAsHeader: Boolean = false,
    ): String {
        val line = ByteArrayOutputStream()
        while (line.size() <= maximumHeaderLineBytes) {
            val value = input.read()
            check(value >= 0) { "规则服务响应意外结束" }
            if (value == '\n'.code) {
                val bytes = line.toByteArray()
                require(bytes.lastOrNull() == '\r'.code.toByte()) { "规则服务响应必须使用 CRLF" }
                headerBudget.consume(bytes.size + 1, countAsHeader)
                return String(bytes, 0, bytes.size - 1, Charsets.US_ASCII)
            }
            require(value <= 0x7f) { "规则服务响应头包含非 ASCII 字节" }
            line.write(value)
        }
        error("规则服务响应头超过长度上限")
    }

    /** 精确读取固定字节数；SOCKS 或 HTTP 提前关闭时直接终止本次更新。 */
    private fun readExact(input: BufferedInputStream, length: Int): ByteArray {
        val bytes = ByteArray(length)
        var offset = 0
        while (offset < length) {
            val count = input.read(bytes, offset, length - offset)
            check(count > 0) { "代理连接意外结束" }
            offset += count
        }
        return bytes
    }

    /** 严格解码 UTF-8，禁止替换字符进入规则缓存。 */
    private fun decodeUtf8(bytes: ByteArray): String = Charsets.UTF_8.newDecoder()
        .onMalformedInput(CodingErrorAction.REPORT)
        .onUnmappableCharacter(CodingErrorAction.REPORT)
        .decode(ByteBuffer.wrap(bytes))
        .toString()

    private companion object {
        const val connectTimeoutMillis = 10_000
        const val ioTimeoutMillis = 15_000
        const val maximumBodyBytes = 1024 * 1024
        const val maximumHeaderLineBytes = 8192
        const val maximumErrorTextLength = 256
        const val rootCertificatePath = "/api/v1/client/ca.cer"
        val httpStatusPattern = Regex("^HTTP/1\\.[01] ([0-9]{3})(?: .*)?$")
        val httpHeaderNamePattern = Regex("^[!#$%&'*+.^_`|~0-9A-Za-z-]+$")
    }
}

/** 描述一次客户端内部 HTTP 资源请求；URI、Accept 与条件缓存必须作为同一协议快照使用。 */
private data class HttpFetchRequest(
    val uri: URI,
    val accept: String,
    val etag: String? = null,
)

/** 保存已完成分帧的 HTTP 响应；正文受统一 1 MiB 上限约束，不包含仍连接的流对象。 */
private data class HttpFetchResponse(
    val status: Int,
    val headers: Map<String, List<String>>,
    val body: ByteArray,
)

/** 保存已验证的三种 HTTP 正文边界，后续读取不得重新解释原始响应头。 */
private sealed interface HttpBodyFraming {
    data class ContentLength(val length: Int) : HttpBodyFraming
    data object Chunked : HttpBodyFraming
    data object ConnectionClose : HttpBodyFraming
}

/**
 * 维护单个响应的头部总字节与字段数量预算。
 * 初始头和 trailer 共用计数，chunk 元数据共用字节上限，任一越界都在分配正文前拒绝。
 */
private class HttpHeaderBudget {
    private var consumedBytes = 0
    private var consumedHeaders = 0

    /** 扣减一行预算；超出固定边界时抛出协议错误，不保留部分响应。 */
    fun consume(lineBytes: Int, countAsHeader: Boolean) {
        consumedBytes += lineBytes
        require(consumedBytes <= maximumHttpHeaderBytes) { "规则服务响应头总量超过上限" }
        if (!countAsHeader) return
        consumedHeaders += 1
        require(consumedHeaders <= maximumHttpHeaderCount) { "规则服务响应头数量超过上限" }
    }
}

/** 读取必须唯一的协议字段；重复 Content-Type 或 ETag 会造成语义歧义，因此直接拒绝。 */
private fun singleHeader(headers: Map<String, List<String>>, name: String): String? {
    val values = headers[name] ?: return null
    require(values.size == 1) { "规则响应重复声明 $name" }
    return values.single()
}

private const val maximumHttpHeaderBytes = 64 * 1024
private const val maximumHttpHeaderCount = 128

/** 识别意图为 IPv4 的纯数字点分文本；实际合法性由统一字面量解析器决定。 */
private fun String.looksLikeIpv4(): Boolean = '.' in this && all { it.isDigit() || it == '.' }

/** 抽象规则传输边界，生产实现固定走 SOCKS5，单元测试可注入确定性响应验证缓存事务。 */
fun interface RoutingRuleSource {
    fun fetch(profile: EmbeddedClientProfile, etag: String?): RoutingFetchResponse
}

/** 表示规则端点仅有的两个成功结果；304 不携带正文，必须由已验证缓存配对使用。 */
sealed interface RoutingFetchResponse {
    data object NotModified : RoutingFetchResponse
    data class Content(val etag: String, val text: String) : RoutingFetchResponse
}
