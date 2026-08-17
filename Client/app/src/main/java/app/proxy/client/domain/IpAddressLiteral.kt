package app.proxy.client.domain

import java.net.Inet6Address
import java.net.InetAddress

/**
 * 统一识别无需 DNS 的 IPv4/IPv6 字面量。
 * 节点启动、规则 DNS 与 SOCKS 地址编码必须共用该边界，避免任一调用点把主机名交给系统解析器。
 */
object IpAddressLiteral {
    /** 返回严格数字地址；域名、IPv4 前导零、IPv6 zone id 或无效文本返回 null，且不会发起 DNS 查询。 */
    fun parse(value: String): InetAddress? {
        parseIpv4Bytes(value)?.let { return InetAddress.getByAddress(it) }
        if (':' !in value || '%' in value) return null
        return runCatching { InetAddress.getByName(value) }
            .getOrNull()
            ?.takeIf { it is Inet6Address }
    }

    /** 判断文本是否为严格 IP 字面量；失败只返回 false，不抛出平台解析异常。 */
    fun matches(value: String): Boolean = parse(value) != null

    /** 按四段十进制解析 IPv4；每段禁止前导零并限制在 0..255，失败返回 null。 */
    private fun parseIpv4Bytes(value: String): ByteArray? {
        val octets = value.split('.')
        if (octets.size != 4) return null
        val address = ByteArray(4)
        octets.forEachIndexed { index, octet ->
            if (
                octet.isEmpty() ||
                (octet.length > 1 && octet.first() == '0') ||
                !octet.all(Char::isDigit)
            ) {
                return null
            }
            val numeric = octet.toIntOrNull()?.takeIf { it in 0..255 } ?: return null
            address[index] = numeric.toByte()
        }
        return address
    }
}
