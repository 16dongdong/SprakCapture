package app.proxy.client.runtime

import app.proxy.client.domain.EmbeddedClientProfile

/**
 * 提供 VPN 与 Root 共用的单例 Native 数据面入口。
 * JNI 生命周期在进程内严格互斥；任何非空错误都转换为异常，服务不得在核心未启动时发布运行状态。
 */
object NativeRuntime {
    @Volatile private var started = false
    @Volatile private var vpnSocketProtector: ((Int) -> Boolean)? = null

    /**
     * 注册当前 VpnService 的逐 socket 排除能力。
     * 必须在 Native start 前设置、在 stop 完成后清除；Root 模式保持 null，防止错误借用已销毁服务实例。
     */
    @Synchronized
    fun setVpnSocketProtector(protector: ((Int) -> Boolean)?) {
        check(!started) { "Native 运行期间不能替换 VPN socket 排除器" }
        vpnSocketProtector = protector
    }

    /**
     * 供 Native 工作线程在 connect 前调用；系统拒绝、服务已清理或 Java 异常均返回 false，
     * C++ 随即关闭该 socket，绝不让节点连接回流进 TUN。
     */
    @JvmStatic
    private fun protectSocket(descriptor: Int): Boolean = runCatching {
        vpnSocketProtector?.invoke(descriptor) == true
    }.getOrDefault(false)

    /**
     * 启动统一本地 SOCKS 路由器，并按 `rootMode` 决定是否同时监听透明 TCP/UDP 入口。
     * `configurationText` 和 `routingText` 只在内存传递；Native 返回错误时本方法抛出且不改变生命周期状态。
     */
    @Synchronized
    fun start(configurationText: String, routingText: String, rootMode: Boolean) {
        check(!started) { "Native 数据面已经启动" }
        nativeStart(configurationText, routingText, rootMode)?.let { error(it) }
        started = true
    }

    /** 原子替换规则并关闭全部旧连接；Native 拒绝新正文时抛出异常且继续使用上一份规则。 */
    @Synchronized
    fun updateRules(routingText: String) {
        check(started) { "Native 数据面尚未启动" }
        nativeUpdateRules(routingText)?.let { error(it) }
    }

    /** 同步停止监听器和所有活动连接；重复停止保持幂等，便于服务异常路径统一回收。 */
    @Synchronized
    fun stop() {
        if (!started) return
        nativeStop()
        started = false
    }

    /**
     * 先检查监听/任务线程的致命状态，再返回五个稳定统计字段。
     * Native fatal 或统计结构不完整都会抛出，VPN/Root 采样协程据此进入统一停止和资源回收路径。
     */
    @Synchronized
    fun stats(): LongArray {
        check(started) { "Native 数据面尚未启动" }
        requireHealthyRuntime(nativeHealth())
        return nativeStats().also { check(it.size == statisticsFieldCount) { "Native 数据面返回了无效统计结构" } }
    }

    /**
     * 认证并解封打包器写入的静态连接资料。
     * Kotlin 只传入可擦除密文并接收可擦除明文；Native 失败会抛出固定中文异常，返回值不得包含密钥或诊断原文。
     */
    fun decryptProfile(encryptedProfile: ByteArray): ByteArray = nativeDecryptProfile(encryptedProfile)

    /** 调用静态 JNI 启动入口；返回 null 表示成功，非空字符串是 Native 已脱敏的精确失败原因。 */
    @JvmStatic private external fun nativeStart(
        configurationText: String,
        routingText: String,
        rootMode: Boolean,
    ): String?

    /** 调用静态 JNI 停止入口；C++ 保证未启动和重复调用均为幂等。 */
    @JvmStatic private external fun nativeStop()

    /** 调用静态 JNI 原子换规则入口；失败字符串表示原有规则保持不变。 */
    @JvmStatic private external fun nativeUpdateRules(routingText: String): String?

    /** 调用静态 JNI 统计入口；数组字段顺序由本对象公开的 stats 契约固定。 */
    @JvmStatic private external fun nativeStats(): LongArray

    /** 查询异步监听/任务线程的致命失败；null 表示健康，非空中文文本不含配置或凭据。 */
    @JvmStatic private external fun nativeHealth(): String?

    /** 调用 Native XChaCha20-Poly1305 解封入口；密钥槽、容器或认证无效时抛出不含秘密的参数异常。 */
    @JvmStatic private external fun nativeDecryptProfile(encryptedProfile: ByteArray): ByteArray

    init {
        val absoluteLibrary = System.getProperty(rootCompanionLibraryProperty)
        if (absoluteLibrary.isNullOrBlank()) {
            System.loadLibrary("routesocks")
        } else {
            // Root 伴随进程由 app_process 启动，不具备 APK 默认 nativeLibraryDir 搜索路径；绝对路径只用于加载代码，
            // 不包含节点或凭据。主应用进程不设置该属性，仍由 Android 按 ABI 正常解析库。
            System.load(absoluteLibrary)
        }
    }

    private const val statisticsFieldCount = 5
    internal const val rootCompanionLibraryProperty = "app.proxy.client.rootLibrary"
}

/** 把 Native 健康快照转换为统一异常；空值继续采样，非空值保留精确阶段并触发上层清理。 */
internal fun requireHealthyRuntime(failure: String?) {
    failure?.let { error(it) }
}

/** 把解封后的短生命周期资料编码成 Native 逐行协议，禁止账号凭据进入文件、日志或 shell 参数。 */
object NativeRuntimeConfiguration {
    /**
     * 创建固定端口配置；字段已经由 EmbeddedClientProfile 校验，换行等控制字符会在进入这里前被拒绝。
     * 节点地址已在进入本层前限定为 IP 字面量；这里只负责生成不落盘的固定字段协议。
     */
    fun create(profile: EmbeddedClientProfile, internalCredentials: InternalSocksCredentials): String = buildString {
        appendLine("upstreamHost=${profile.node.host}")
        appendLine("upstreamPort=${profile.node.port}")
        appendLine("username=${profile.credentials.username}")
        appendLine("password=${profile.credentials.password}")
        appendLine("localUsername=${internalCredentials.username}")
        appendLine("localPassword=${internalCredentials.password}")
        appendLine("localSocksPort=$localSocksPort")
        appendLine("selectedSocksPort=$selectedSocksPort")
        appendLine("transparentTcpPort=$transparentTcpPort")
        appendLine("transparentUdpPort=$transparentUdpPort")
        appendLine("selectedTransparentTcpPort=$selectedTransparentTcpPort")
        appendLine("selectedTransparentUdpPort=$selectedTransparentUdpPort")
        appendLine("globalUdpQueueNumber=$globalUdpQueueNumber")
        appendLine("selectedUdpQueueNumber=$selectedUdpQueueNumber")
    }

    const val localSocksPort = 12580
    const val selectedSocksPort = 12581
    const val transparentTcpPort = 12345
    const val transparentUdpPort = 12346
    const val selectedTransparentTcpPort = 12347
    const val selectedTransparentUdpPort = 12348
    const val globalUdpQueueNumber = 6100
    const val selectedUdpQueueNumber = 6101
}
