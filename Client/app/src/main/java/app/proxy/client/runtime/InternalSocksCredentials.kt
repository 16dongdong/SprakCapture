package app.proxy.client.runtime

import java.security.SecureRandom
import java.util.Base64

/**
 * 保存一次数据面生命周期的回环 SOCKS5 凭据。
 * 它只用于 HEV 与同进程 Native 入口之间隔离其他 UID，不得复用远端账号或持久化。
 */
class InternalSocksCredentials internal constructor(
    val username: String,
    val password: String,
) {
    init {
        require(isEncodedToken(username) && isEncodedToken(password)) {
            "内部 SOCKS5 凭据必须是固定长度 Base64URL"
        }
    }

    companion object {
        private val secureRandom = SecureRandom()
        private val encoder = Base64.getUrlEncoder().withoutPadding()
        private const val randomBytes = 24
        private const val encodedTokenLength = 32

        /**
         * 为每次 VPN/Root 启动生成独立 192 位账号和密码。
         * 随机源异常直接向上抛出并阻止数据面启动，禁止使用固定值降级。
         */
        fun generate(): InternalSocksCredentials = InternalSocksCredentials(
            username = randomToken(),
            password = randomToken(),
        )

        /** 从系统 CSPRNG 生成无换行 Base64URL 字符串，可直接进入逐行配置和 YAML 单引号。 */
        private fun randomToken(): String = ByteArray(randomBytes).also(secureRandom::nextBytes).let(encoder::encodeToString)

        /** 限定为生产工厂的固定 Base64URL 字符集，避免任意构造值逸出 YAML 或逐行配置。 */
        private fun isEncodedToken(value: String): Boolean =
            value.length == encodedTokenLength && value.all { character ->
                character in 'A'..'Z' ||
                    character in 'a'..'z' ||
                    character in '0'..'9' ||
                    character == '-' ||
                    character == '_'
            }
    }
}
