package app.proxy.client.certificate

import java.security.MessageDigest
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.Base64

/**
 * 保存已经完成密码学与用途校验的公开根证书。
 * 对象只在同步事务内短暂存在；`pem` 用于 Android 系统证书目录，`subjectHash` 遵循系统兼容文件名规则。
 */
class TrustedRootCertificate private constructor(
    val pem: String,
    val subjectHash: String,
    val fingerprint: String,
) {
    companion object {
        /**
         * 从服务端 DER 正文构造可信安装材料。
         * 证书必须是当前有效、自签名且具备 CA 能力；任何解析或验证失败都会阻止 Root 文件系统变更。
         */
        fun parse(der: ByteArray): TrustedRootCertificate {
            require(der.isNotEmpty()) { "根证书正文为空" }
            val certificate = CertificateFactory.getInstance("X.509")
                .generateCertificate(der.inputStream()) as X509Certificate
            certificate.checkValidity()
            require(certificate.basicConstraints >= 0) { "服务端证书不是根证书" }
            require(certificate.subjectX500Principal == certificate.issuerX500Principal) { "服务端证书不是自签名根证书" }
            certificate.verify(certificate.publicKey)
            val canonicalDer = certificate.encoded
            return TrustedRootCertificate(
                pem = encodePem(canonicalDer),
                subjectHash = androidSubjectHash(certificate),
                fingerprint = MessageDigest.getInstance("SHA-256").digest(canonicalDer).toHex(),
            )
        }

        /** Android 证书文件名沿用 OpenSSL subject_hash_old 的 MD5 小端前四字节，仅承担索引兼容而非安全校验。 */
        private fun androidSubjectHash(certificate: X509Certificate): String {
            val digest = MessageDigest.getInstance("MD5").digest(certificate.subjectX500Principal.encoded)
            return digest.take(4).reversed().joinToString("") { byte -> "%02x".format(byte.toInt() and 0xff) }
        }

        /** 把规范 DER 编码为 Android cacerts 接受的 64 字符换行 PEM。 */
        private fun encodePem(der: ByteArray): String = buildString {
            appendLine("-----BEGIN CERTIFICATE-----")
            Base64.getEncoder().encodeToString(der).chunked(64).forEach(::appendLine)
            appendLine("-----END CERTIFICATE-----")
        }
    }
}

/** 把摘要转成稳定小写十六进制，仅用于比较证书版本，不输出证书正文。 */
private fun ByteArray.toHex(): String = joinToString("") { byte -> "%02x".format(byte.toInt() and 0xff) }
