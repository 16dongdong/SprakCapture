package app.proxy.client.routing

import android.content.Context
import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.domain.userVisibleProxyError
import java.io.File
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.nio.file.StandardCopyOption

/** 管理最后一个已验证规则及 ETag；更新失败只可复用该缓存，绝不回退 APK 内置长期规则。 */
class RoutingRuleRepository internal constructor(
    private val ruleDirectory: File,
    private val source: RoutingRuleSource,
) {
    private val snapshotFile = File(ruleDirectory, snapshotFileName)

    /** 生产构造固定使用应用私有目录和 SOCKS5 传输，不允许调用方传入公共存储。 */
    constructor(context: Context) : this(File(context.filesDir, ruleDirectoryName), SocksRoutingClient())

    /**
     * 通过 SOCKS5 检查云端规则并返回当前可用版本。
     * 首次没有缓存时网络失败直接失败；已有缓存时保留已验证版本并返回诊断，供运行数据面继续服务。
     */
    fun refresh(profile: EmbeddedClientProfile): RoutingRefreshResult {
        val cached = loadCached()
        return runCatching {
            when (val response = source.fetch(profile, cached?.etag)) {
                RoutingFetchResponse.NotModified -> {
                    checkNotNull(cached) { "规则服务返回 304，但客户端没有有效缓存" }
                    RoutingRefreshResult(cached.document, changed = false)
                }
                is RoutingFetchResponse.Content -> {
                    val document = RoutingRuleParser.parse(response.text)
                    val changed = cached?.etag != response.etag || cached.document.text != document.text
                    if (changed) storeValidated(response.etag, document.text)
                    RoutingRefreshResult(document, changed)
                }
            }
        }.getOrElse { failure ->
            if (cached == null) throw failure
            RoutingRefreshResult(
                cached.document,
                changed = false,
                diagnostic = userVisibleProxyError(failure.message, "云规则更新失败，已继续使用上次有效规则"),
            )
        }
    }

    /**
     * 读取并重新校验单文件快照。
     * ETag 与正文共用一次原子 rename，因此进程崩溃只能观察到旧代或新代，不会组合两个不同代次。
     */
    private fun loadCached(): CachedRoutingRule? {
        removeObsoleteSplitCache()
        removeAbandonedStagingFile()
        if (!snapshotFile.isFile) return null
        return runCatching {
            require(snapshotFile.length() in snapshotHeaderBytes.toLong()..maximumSnapshotBytes.toLong()) {
                "规则快照长度无效"
            }
            decodeSnapshot(snapshotFile.readBytes())
        }.getOrElse {
            check(snapshotFile.delete() || !snapshotFile.exists()) { "损坏规则快照清理失败" }
            null
        }
    }

    /** 把 ETag 与已验证正文编码为一个有界快照，同步落盘后只执行一次原子替换。 */
    private fun storeValidated(etag: String, text: String) {
        val snapshot = encodeSnapshot(etag, text)
        check(ruleDirectory.isDirectory || ruleDirectory.mkdirs()) { "规则缓存目录创建失败" }
        atomicReplace(snapshotFile, snapshot)
    }

    /**
     * 清理旧的正文/ETag 分文件格式。
     * 该格式无代次标识，无法证明两个文件来自同一更新，因此不迁移其内容而是要求重新下载。
     */
    private fun removeObsoleteSplitCache() {
        listOf(legacyRuleFileName, legacyEtagFileName).forEach { fileName ->
            val obsoleteFile = File(ruleDirectory, fileName)
            check(obsoleteFile.delete() || !obsoleteFile.exists()) { "旧规则缓存清理失败：$fileName" }
        }
    }

    /** 清理上次进程在原子移动前留下的 staging；该文件从未提交，不得参与恢复。 */
    private fun removeAbandonedStagingFile() {
        val stagingFile = File(ruleDirectory, "$snapshotFileName.next")
        check(stagingFile.delete() || !stagingFile.exists()) { "未提交规则快照清理失败" }
    }

    /** 编码固定魔数、两个网络序长度和 UTF-8 内容；越界响应在写盘前直接拒绝。 */
    private fun encodeSnapshot(etag: String, text: String): ByteArray {
        validateEtag(etag)
        val etagBytes = etag.toByteArray(Charsets.UTF_8)
        val ruleBytes = text.toByteArray(Charsets.UTF_8)
        require(ruleBytes.size <= maximumRuleBytes) { "规则响应正文超过 1 MiB 上限" }
        return ByteBuffer.allocate(snapshotHeaderBytes + etagBytes.size + ruleBytes.size).apply {
            put(snapshotMagic)
            putInt(etagBytes.size)
            putInt(ruleBytes.size)
            put(etagBytes)
            put(ruleBytes)
        }.array()
    }

    /** 解码单一快照并复验 ETag、长度与规则语法；截断、尾随字节或非 UTF-8 均使整份缓存失效。 */
    private fun decodeSnapshot(contents: ByteArray): CachedRoutingRule {
        require(contents.size in snapshotHeaderBytes..maximumSnapshotBytes) { "规则快照长度无效" }
        val buffer = ByteBuffer.wrap(contents)
        val magic = ByteArray(snapshotMagic.size).also(buffer::get)
        require(magic.contentEquals(snapshotMagic)) { "规则快照格式无效" }
        val etagLength = buffer.int
        val ruleLength = buffer.int
        require(etagLength in 1..maximumEtagBytes && ruleLength in 1..maximumRuleBytes) { "规则快照字段长度无效" }
        require(buffer.remaining() == etagLength + ruleLength) { "规则快照内容长度不匹配" }
        val etag = decodeUtf8(ByteArray(etagLength).also(buffer::get), "规则快照 ETag")
        val text = decodeUtf8(ByteArray(ruleLength).also(buffer::get), "规则快照正文")
        validateEtag(etag)
        return CachedRoutingRule(etag, RoutingRuleParser.parse(text))
    }

    /** 使用 REPORT 模式解码 UTF-8，禁止默认替换字符把损坏快照伪装成可解析文本。 */
    private fun decodeUtf8(bytes: ByteArray, fieldName: String): String = runCatching {
        Charsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(bytes)).toString()
    }.getOrElse { failure ->
        throw IllegalArgumentException("$fieldName 不是有效 UTF-8", failure)
    }

    /** 校验 HTTP ETag 可以安全回填 If-None-Match；只接受有界可见 ASCII，禁止控制字符破坏请求边界。 */
    private fun validateEtag(etag: String) {
        require(etag.isNotEmpty() && etag.length <= maximumEtagBytes && etag.all { it.code in 0x21..0x7e }) {
            "规则响应 ETag 无效"
        }
    }

    /** 在目标目录内完成 fsync 和原子移动；失败时删除临时文件并保留上一版本。 */
    private fun atomicReplace(destination: File, contents: ByteArray) {
        val staging = File(ruleDirectory, "${destination.name}.next")
        try {
            FileOutputStream(staging).use { stream ->
                stream.write(contents)
                stream.fd.sync()
            }
            java.nio.file.Files.move(
                staging.toPath(),
                destination.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING,
            )
        } finally {
            check(staging.delete() || !staging.exists()) { "规则缓存临时文件清理失败" }
        }
    }

    private data class CachedRoutingRule(val etag: String, val document: RoutingRuleDocument)

    private companion object {
        const val ruleDirectoryName = "routingRules"
        const val snapshotFileName = "routing.snapshot"
        const val legacyRuleFileName = "routing.txt"
        const val legacyEtagFileName = "routing.etag"
        const val maximumEtagBytes = 8192
        const val maximumRuleBytes = 1024 * 1024
        val snapshotMagic = "SPRKRR01".toByteArray(Charsets.US_ASCII)
        val snapshotHeaderBytes = snapshotMagic.size + Int.SIZE_BYTES * 2
        val maximumSnapshotBytes = snapshotHeaderBytes + maximumEtagBytes + maximumRuleBytes
    }
}

/** 返回一次更新后的有效规则、变化标记和非致命诊断，便于服务决定是否重建数据面。 */
data class RoutingRefreshResult(
    val document: RoutingRuleDocument,
    val changed: Boolean,
    val diagnostic: String? = null,
)
