package app.proxy.client.config

import android.content.Context
import app.proxy.client.domain.AccountCredentials
import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.domain.EmbeddedNode
import app.proxy.client.runtime.NativeRuntime
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction

/**
 * 从 APK 二进制资产读取认证加密的静态连接资料。
 * 密钥与解密算法只存在于 Native 打包槽和实现中；Kotlin 不缓存密文、明文或资料对象，也不提供第二配置入口。
 */
object EmbeddedNodeProvider {
    private const val assetName = "bootstrap/profile.bin"
    private const val profileVersion = 1
    private const val maximumEncryptedProfileBytes = 4096

    /**
     * 解封 APK 唯一静态连接资料并返回当前服务生命周期使用的不可打印对象。
     * 密文与 Native 返回的可变明文字节在 finally 中覆盖；认证、格式或字段校验失败直接抛出不含秘密的异常。
     */
    fun current(context: Context): EmbeddedClientProfile {
        val encryptedProfile = readEncryptedProfile(context)
        return try {
            val decryptedProfile = NativeRuntime.decryptProfile(encryptedProfile)
            try {
                parseDecrypted(decryptedProfile)
            } finally {
                decryptedProfile.fill(0)
            }
        } finally {
            encryptedProfile.fill(0)
        }
    }

    /**
     * 严格解析 Native 认证后的大端二进制资料。
     * 字段顺序由跨层 ABI 固定，版本不符、UTF-8 损坏或尾随字节均拒绝，禁止猜测或兼容旧 JSON。
     */
    internal fun parseDecrypted(contents: ByteArray): EmbeddedClientProfile {
        val decoder = ProfileDecoder(contents)
        require(decoder.readUnsignedByte("版本") == profileVersion) { "安装包静态资料版本不受支持" }
        val profile = EmbeddedClientProfile(
            node = EmbeddedNode(
                host = decoder.readUtf8("服务地址"),
                port = decoder.readUnsignedShort("服务端口"),
            ),
            credentials = AccountCredentials(
                username = decoder.readUtf8("账号"),
                password = decoder.readUtf8("密码"),
            ),
            rulesUrl = decoder.readUtf8("规则地址"),
        )
        decoder.requireEnd()
        profile.validate().getOrThrow()
        return profile
    }

    /** 以固定上限读取资产；超限在分配更大缓冲前失败，避免损坏 APK 触发无界内存增长。 */
    private fun readEncryptedProfile(context: Context): ByteArray = context.assets.open(assetName).use { input ->
        val boundedBuffer = ByteArray(maximumEncryptedProfileBytes + 1)
        try {
            var size = 0
            while (size < boundedBuffer.size) {
                val count = input.read(boundedBuffer, size, boundedBuffer.size - size)
                if (count < 0) return@use boundedBuffer.copyOf(size)
                check(count > 0) { "安装包静态资料读取没有进展" }
                size += count
            }
            error("安装包静态资料超过大小上限")
        } finally {
            boundedBuffer.fill(0)
        }
    }
}

/** 按严格游标解析可擦除明文；所有边界错误只返回字段角色，不拼接字段内容。 */
private class ProfileDecoder(private val contents: ByteArray) {
    private var offset = 0

    /** 读取单字节无符号整数；截断时返回不含原始字节的格式错误。 */
    fun readUnsignedByte(fieldName: String): Int {
        requireRemaining(1, fieldName)
        return contents[offset++].toInt() and 0xff
    }

    /** 读取大端无符号短整数；端口和长度共用该稳定 ABI。 */
    fun readUnsignedShort(fieldName: String): Int {
        requireRemaining(2, fieldName)
        val value = ((contents[offset].toInt() and 0xff) shl 8) or (contents[offset + 1].toInt() and 0xff)
        offset += 2
        return value
    }

    /** 读取 u16 长度前缀的严格 UTF-8；临时字段字节在解码后立即覆盖。 */
    fun readUtf8(fieldName: String): String {
        val length = readUnsignedShort("$fieldName 长度")
        requireRemaining(length, fieldName)
        val fieldBytes = contents.copyOfRange(offset, offset + length)
        offset += length
        return try {
            Charsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
                .decode(ByteBuffer.wrap(fieldBytes))
                .toString()
        } catch (failure: CharacterCodingException) {
            throw IllegalArgumentException("安装包静态资料${fieldName}不是有效 UTF-8", failure)
        } finally {
            fieldBytes.fill(0)
        }
    }

    /** 要求解析恰好消费完整明文；尾随数据可能表示错版 ABI，必须拒绝。 */
    fun requireEnd() {
        require(offset == contents.size) { "安装包静态资料包含尾随字节" }
    }

    /** 校验剩余长度并避免 `offset + length` 整数溢出；失败消息只包含字段角色。 */
    private fun requireRemaining(length: Int, fieldName: String) {
        require(length >= 0 && length <= contents.size - offset) { "安装包静态资料${fieldName}字段截断" }
    }
}
