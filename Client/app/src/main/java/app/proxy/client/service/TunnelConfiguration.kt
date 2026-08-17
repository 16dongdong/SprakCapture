package app.proxy.client.service

import app.proxy.client.runtime.InternalSocksCredentials
import app.proxy.client.runtime.NativeRuntimeConfiguration

/**
 * 生成 HEV 仅承担 TUN 协议栈时使用的固定 YAML。
 * 所有出口认证、路由、DNS 直连和域名嗅探均由回环上的 libroutesocks 执行，HEV 不再持有服务凭据。
 */
object TunnelConfiguration {
    /**
     * 创建连接 127.0.0.1 Native SOCKS 入口的私有配置。
     * `internalCredentials` 每次启动随机生成，使其他 Android UID 无法直连固定端口借用内置远端账号。
     */
    fun create(internalCredentials: InternalSocksCredentials): String = buildString {
        appendLine("misc:")
        appendLine("  task-stack-size: 86016")
        appendLine("  tcp-buffer-size: 65536")
        appendLine("  log-level: warn")
        appendLine("tunnel:")
        appendLine("  mtu: 8500")
        appendLine("  icmp: 'reply'")
        appendLine("socks5:")
        appendLine("  address: '127.0.0.1'")
        appendLine("  port: ${NativeRuntimeConfiguration.localSocksPort}")
        appendLine("  username: '${internalCredentials.username}'")
        appendLine("  password: '${internalCredentials.password}'")
        appendLine("  udp: 'udp'")
    }
}
