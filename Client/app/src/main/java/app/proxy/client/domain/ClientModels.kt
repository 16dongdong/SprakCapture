package app.proxy.client.domain

import java.net.URI

/** 描述客户端可选择的数据面；VPN 适用于普通设备，ROOT 使用 iptables 透明转发。 */
enum class ProxyMode {
    VPN,
    ROOT,
}

/** 表示代理生命周期的稳定状态，界面只消费该模型而不直接推断服务进程。 */
enum class ConnectionPhase {
    STOPPED,
    STARTING,
    RUNNING,
    STOPPING,
    FAILED,
}

/** 保存打包阶段注入的唯一节点；客户端不提供修改地址和端口的入口。 */
class EmbeddedNode(
    val host: String,
    val port: Int,
) {
    /** 校验节点为可直接连接的 IP 字面量；失败返回中文原因并阻止首次规则下载使用系统 DNS。 */
    fun validate(): Result<Unit> {
        if (host.isBlank()) return Result.failure(IllegalArgumentException("安装包未内置服务地址"))
        if (!IpAddressLiteral.matches(host)) {
            return Result.failure(IllegalArgumentException("安装包内置服务地址必须是 IPv4 或 IPv6 字面量"))
        }
        if (port !in 1..65535) return Result.failure(IllegalArgumentException("安装包内置服务端口无效"))
        return Result.success(Unit)
    }

}

/** 保存打包阶段内置的账号凭据；下载端点要求用户提交非空密码，打包结果不得制造空 RFC 1929 字段。 */
class AccountCredentials(
    val username: String,
    val password: String,
) {
    /** 验证账号密码均为非空且符合 RFC 1929 单字节长度边界；失败时禁止启动而不是替换或截断输入。 */
    fun validate(): Result<Unit> {
        val usernameBytes = username.toByteArray(Charsets.UTF_8).size
        val passwordBytes = password.toByteArray(Charsets.UTF_8).size
        if (usernameBytes !in 1..255) {
            return Result.failure(IllegalArgumentException("账号不能为空且 UTF-8 长度不能超过 255 字节"))
        }
        if (passwordBytes !in 1..255) {
            return Result.failure(IllegalArgumentException("密码不能为空且 UTF-8 长度不能超过 255 字节"))
        }
        if (username.any(Char::isISOControl) || password.any(Char::isISOControl)) {
            return Result.failure(IllegalArgumentException("账号密码不能包含控制字符"))
        }
        return Result.success(Unit)
    }
}

/**
 * 汇总 APK 打包阶段注入的服务端连接资料。
 * 节点、凭据和规则地址必须作为一个不可分割快照读取，避免不同模板版本之间交叉使用。
 */
class EmbeddedClientProfile(
    val node: EmbeddedNode,
    val credentials: AccountCredentials,
    val rulesUrl: String,
) {
    /** 校验完整注入资料；规则地址只允许明文 HTTP，因为部署契约由 SOCKS5 隧道承担传输边界。 */
    fun validate(): Result<Unit> = runCatching {
        node.validate().getOrThrow()
        credentials.validate().getOrThrow()
        val uri = URI(rulesUrl)
        check(uri.scheme.equals("http", ignoreCase = true)) { "安装包内置规则地址必须使用 HTTP" }
        check(!uri.host.isNullOrBlank()) { "安装包内置规则地址缺少主机" }
        check(uri.host.all { it.code in 33..126 }) { "安装包内置规则地址主机必须是 ASCII" }
        check(uri.rawUserInfo == null && uri.rawFragment == null) { "安装包内置规则地址包含不允许的字段" }
        check(uri.port in -1..65535 && uri.port != 0) { "安装包内置规则地址端口无效" }
    }
}

/** 仅持久化用户可选的数据面；节点、凭据和规则范围全部由安装包及服务端控制。 */
data class ClientSettings(
    val mode: ProxyMode = ProxyMode.VPN,
    val certificateTrustEnabled: Boolean = false,
)

/** 提供服务到界面的单一运行投影；累计字节只在当前服务生命周期内单调增加。 */
data class ProxyRuntimeState(
    val phase: ConnectionPhase = ConnectionPhase.STOPPED,
    val mode: ProxyMode? = null,
    val startedAtMillis: Long? = null,
    val uploadBytes: Long = 0,
    val downloadBytes: Long = 0,
    val uploadBytesPerSecond: Long = 0,
    val downloadBytesPerSecond: Long = 0,
    val error: String? = null,
    val diagnostic: String? = null,
)

/** 封装一次流量采样，避免累计值和速率在跨层调用时发生参数顺序错配。 */
data class TrafficSnapshot(
    val uploadBytes: Long,
    val downloadBytes: Long,
    val uploadBytesPerSecond: Long,
    val downloadBytesPerSecond: Long,
)
