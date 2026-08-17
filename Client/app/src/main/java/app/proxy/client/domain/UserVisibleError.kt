package app.proxy.client.domain

/**
 * 将底层异常转换为不含部署资料的界面文案。
 * Native、Socket 和 HTTP 层的异常可能包含节点地址、端口、来源地址或规则 URL；这些值只允许留在受控诊断日志，不能进入 Compose 状态、通知或 Toast。
 */
fun userVisibleProxyError(rawMessage: String?, fallback: String): String {
    val message = rawMessage.orEmpty()
    return when {
        message.contains("规则") -> "云规则更新失败，已继续使用上次有效规则"
        message.contains("Root", ignoreCase = true) -> "Root 数据面初始化失败"
        message.contains("VPN", ignoreCase = true) -> "VPN 数据面初始化失败"
        message.contains("SOCKS", ignoreCase = true) ||
            message.contains("connect", ignoreCase = true) ||
            message.contains("连接") -> "代理服务器连接失败，请检查网络后重试"
        else -> fallback
    }
}
