package app.proxy.client.service

import android.content.Intent
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import app.proxy.client.certificate.RootCertificateTrustManager
import app.proxy.client.config.ClientPreferences
import app.proxy.client.config.EmbeddedNodeProvider
import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.domain.ProxyMode
import app.proxy.client.domain.TrafficSnapshot
import app.proxy.client.routing.RoutingContext
import app.proxy.client.routing.RoutingRuleDocument
import app.proxy.client.routing.RoutingRuleRepository
import app.proxy.client.routing.VpnApplicationScope
import app.proxy.client.runtime.FlowOwnershipResolver
import app.proxy.client.runtime.InternalSocksCredentials
import app.proxy.client.runtime.NativeRuntime
import app.proxy.client.runtime.NativeRuntimeConfiguration
import app.proxy.client.runtime.ProxyRuntime
import app.proxy.client.runtime.ProxyServiceController
import app.proxy.client.runtime.StartRequestRegistry
import app.proxy.client.runtime.VpnFlowClassifier
import hev.sockstun.TProxyService
import java.util.concurrent.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/** 通过 VpnService 按云端规则建立 TUN，并把被接管流量交给 Native SOCKS5 隧道。 */
class ProxyVpnService : VpnService() {
    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val resourceLock = Any()
    private val packageChangeMonitor by lazy { PackageChangeMonitor(this) }
    private val packageScopeResolver by lazy { PackageScopeResolver(this) }
    private var tunnelDescriptor: ParcelFileDescriptor? = null
    private var lifecycleJob: Job? = null
    private var statisticsJob: Job? = null
    private var statisticsGeneration = 0L
    private var tunnelCoreStarted = false
    private var routeCoreStarted = false
    private var activeStartGeneration: Long? = null
    @Volatile private var stopping = false

    /** 注册包生命周期监听；UID 投影必须覆盖服务运行期间的安装、卸载和更新。 */
    override fun onCreate() {
        super.onCreate()
        packageChangeMonitor.start()
    }

    /** 处理显式启停动作；系统恢复空 Intent 时只根据持久运行意图恢复，不接受第二套配置。 */
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ProxyServiceController.ACTION_STOP -> {
                StartRequestRegistry.cancel(ProxyMode.VPN)
                stopTunnel()
            }
            ProxyServiceController.ACTION_START -> {
                val generation = intent.activeStartGenerationOrNull(ProxyMode.VPN)
                if (generation == null) rejectStaleStart(startId) else startTunnel(generation)
            }
            null -> restoreAfterSystemRestart()
        }
        return START_STICKY
    }

    /**
     * 处理 START_STICKY 的空 Intent；只有运行意图开启且持久模式仍为 VPN 才恢复。
     * 偏好损坏或模式已切到 Root 时结束旧组件，避免两个数据面同时争用 TUN/iptables。
     */
    private fun restoreAfterSystemRestart() {
        val shouldRestore = ClientPreferences(this).shouldRestore(ProxyMode.VPN).getOrElse { failure ->
            ProxyRuntime.markFailed(ProxyMode.VPN, failure.message ?: "VPN 恢复配置读取失败")
            stopSelf()
            return
        }
        if (shouldRestore) startTunnel(null) else stopSelf()
    }

    /**
     * 拒绝已经取消的迟到 START；空闲实例按当前 startId 结束，已有有效生命周期时只忽略旧请求。
     * 这样旧 Intent 不会停止随后已运行的新代次，单独创建的失效服务也不会常驻。
     */
    private fun rejectStaleStart(startId: Int) {
        if (lifecycleJob?.isActive != true && tunnelDescriptor == null && !tunnelCoreStarted && !routeCoreStarted) {
            stopSelf(startId)
        }
    }

    /** VPN 授权被系统撤销时进入统一异步停止路径，确保 Native 与 TUN 一并回收。 */
    override fun onRevoke() {
        stopTunnel()
        super.onRevoke()
    }

    /** 服务销毁时执行最终幂等回收；异常退出不得留下 Native 线程持有失效 TUN。 */
    override fun onDestroy() {
        transitionToStopping()
        activeStartGeneration?.let { StartRequestRegistry.cancel(ProxyMode.VPN, it) }
        activeStartGeneration = null
        lifecycleJob?.cancel()
        val packageMonitorFailure = runCatching(packageChangeMonitor::stop).exceptionOrNull()
        val resourceFailure = releaseTunnelResources().exceptionOrNull()
        val cleanupFailure = resourceFailure ?: packageMonitorFailure
        if (resourceFailure != null && packageMonitorFailure != null) {
            resourceFailure.addSuppressed(packageMonitorFailure)
        }
        if (cleanupFailure != null) {
            val previous = ProxyRuntime.state.value
            ProxyRuntime.markFailed(
                ProxyMode.VPN,
                appendFailure(previous.error ?: "VPN 数据面销毁失败", cleanupFailure, "资源清理"),
            )
        }
        serviceScope.cancel()
        super.onDestroy()
    }

    /**
     * 在 IO 生命周期中下载首份规则、启动数据面并持续检查云更新，避免主线程进行网络 I/O。
     * `startGeneration` 为 null 仅表示系统按已持久运行意图恢复；显式启动必须在发布 RUNNING 前保持代次有效。
     */
    private fun startTunnel(startGeneration: Long?) {
        if (lifecycleJob?.isActive == true || tunnelDescriptor != null || tunnelCoreStarted || routeCoreStarted) return
        activeStartGeneration = startGeneration
        stopping = false
        ProxyRuntime.markStarting(ProxyMode.VPN)
        val profile = EmbeddedNodeProvider.current(this)
        ServiceNotification.ensureChannel(this)
        startForeground(
            ServiceNotification.NOTIFICATION_ID,
            ServiceNotification.create(this, ProxyMode.VPN),
        )
        lifecycleJob = serviceScope.launch {
            try {
                val repository = RoutingRuleRepository(this@ProxyVpnService)
                val initialRules = repository.refresh(profile)
                startDataPlane(profile, initialRules.document)
                synchronizeCertificateTrust(profile)
                // startDataPlane 是同步资源事务；返回后必须重新观察取消，避免 onDestroy 清理后发布伪运行状态。
                currentCoroutineContext().ensureActive()
                publishRunningState(initialRules.diagnostic, startGeneration)
                monitorRuleUpdates(profile, repository, initialRules.document)
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (failure: Throwable) {
                failAndStop(failure)
            }
        }
    }

    /**
     * 先启动统一规则核心，再让 HEV 把 TUN 流量送到回环 SOCKS 入口。
     * 任一步失败都由生命周期上层按 HEV、TUN、规则核心的逆序回滚，不会遗留半启动数据面。
     */
    private fun startDataPlane(profile: EmbeddedClientProfile, rules: RoutingRuleDocument) = synchronized(resourceLock) {
        check(!stopping) { "VPN 数据面已经进入停止流程" }
        // 先完成 API 级别、包名和 shared UID 校验，配置无效时不创建任何本地监听器。
        val packageScope = packageScopeResolver.resolve(rules)
        configureFlowClassification(rules, packageScope.selectedUids)
        val internalCredentials = InternalSocksCredentials.generate()
        NativeRuntime.setVpnSocketProtector(::protect)
        try {
            NativeRuntime.start(
                NativeRuntimeConfiguration.create(profile, internalCredentials),
                rules.text,
                rootMode = false,
            )
        } catch (failure: Throwable) {
            NativeRuntime.setVpnSocketProtector(null)
            throw failure
        }
        routeCoreStarted = true
        val descriptor = establishTunnel(rules.vpnScope, rules.dnsServers)
        tunnelDescriptor = descriptor
        startTunnelCore(TunnelConfiguration.create(internalCredentials), descriptor.fd)
        tunnelCoreStarted = true
        startStatisticsSampling()
    }

    /**
     * 通过匿名管道把 HEV 配置仅暴露为当前进程的 `/proc/self/fd` 路径，并等待初始化握手。
     * 管道内容不进入 cache/files/tmp；握手返回后读端立即关闭，进程崩溃也不会留下可恢复凭据。
     */
    private fun startTunnelCore(configurationText: String, tunnelFileDescriptor: Int) {
        val pipe = ParcelFileDescriptor.createPipe()
        val readDescriptor = pipe[0]
        val writeDescriptor = pipe[1]
        var primaryFailure: Throwable? = null
        try {
            val configurationBytes = configurationText.toByteArray(Charsets.UTF_8)
            try {
                require(configurationBytes.size <= maximumTunnelConfigurationBytes) { "VPN 内存配置超过大小上限" }
                ParcelFileDescriptor.AutoCloseOutputStream(writeDescriptor).use { output ->
                    output.write(configurationBytes)
                }
            } finally {
                configurationBytes.fill(0)
            }
            startTunnelCoreFromPath("/proc/self/fd/${readDescriptor.fd}", tunnelFileDescriptor)
        } catch (failure: Throwable) {
            primaryFailure = failure
            throw failure
        } finally {
            closeConfigurationPipe(pipe, primaryFailure)
        }
    }

    /**
     * 关闭匿名配置管道并合并双端错误。
     * 启动已有主错误时把关闭错误作为 suppressed；正常路径关闭失败则直接阻止发布 RUNNING。
     */
    private fun closeConfigurationPipe(pipe: Array<ParcelFileDescriptor>, primaryFailure: Throwable?) {
        var closeFailure: Throwable? = null
        pipe.forEach { descriptor ->
            runCatching(descriptor::close).onFailure { failure ->
                if (closeFailure == null) closeFailure = failure else closeFailure.addSuppressed(failure)
            }
        }
        if (primaryFailure != null) {
            closeFailure?.let(primaryFailure::addSuppressed)
        } else {
            closeFailure?.let { throw it }
        }
    }

    /**
     * 等待 HEV 初始化握手；失败时仍调用幂等 stop，覆盖“已经启动”等部分生命周期异常。
     * stop 自身抛出的 JNI 异常作为 suppressed 保留，主错误始终是本次启动失败原因。
     */
    private fun startTunnelCoreFromPath(configurationPath: String, tunnelFileDescriptor: Int) {
        try {
            TProxyService.TProxyStartService(configurationPath, tunnelFileDescriptor)?.let { error(it) }
        } catch (failure: Throwable) {
            runCatching(TProxyService::TProxyStopService).exceptionOrNull()?.let(failure::addSuppressed)
            throw failure
        }
    }

    /**
     * 由包广播或五分钟规则截止时间唤醒监控循环。
     * 包事件不依赖 ETag，必须在资源锁事务内完整重建并重新校验 UID；规则范围不变时仍可原子热更。
     */
    private suspend fun monitorRuleUpdates(
        profile: EmbeddedClientProfile,
        repository: RoutingRuleRepository,
        initialDocument: RoutingRuleDocument,
    ) {
        var activeDocument = initialDocument
        var nextRuleRefreshAt = SystemClock.elapsedRealtime() + ruleRefreshIntervalMillis
        while (serviceScope.isActive && !stopping) {
            val waitMillis = (nextRuleRefreshAt - SystemClock.elapsedRealtime()).coerceAtLeast(1L)
            if (packageChangeMonitor.awaitChange(waitMillis)) {
                if (stopping) continue
                ProxyRuntime.markStopping()
                if (!applyRuleRefresh(profile, activeDocument, rebuildDataPlane = true)) return
                continue
            }
            val refresh = repository.refresh(profile)
            synchronizeCertificateTrust(profile)
            nextRuleRefreshAt = SystemClock.elapsedRealtime() + ruleRefreshIntervalMillis
            ProxyRuntime.updateDiagnostic(ProxyMode.VPN, refresh.diagnostic)
            if (!refresh.changed || stopping) continue
            ProxyRuntime.markStopping()
            val rebuildDataPlane = !refresh.document.vpnDataPlaneScopeEquals(activeDocument)
            if (!applyRuleRefresh(profile, refresh.document, rebuildDataPlane)) return
            activeDocument = refresh.document
        }
    }

    /**
     * 在代理数据面已建立后按持久意图同步系统根证书。
     * 关闭开关时不发请求；开启后任何鉴权、证书或 Root 失败都会终止数据面，避免界面显示运行但信任未生效。
     */
    private fun synchronizeCertificateTrust(profile: EmbeddedClientProfile) {
        if (!ClientPreferences(this).read().certificateTrustEnabled) return
        RootCertificateTrustManager().synchronize(profile)
    }

    /**
     * 在与 stop/onDestroy 共用的资源锁内更新规则或重建完整数据面。
     * `rebuildDataPlane` 为 true 时会再次检查停止标记，取消恰好发生在旧实例释放后也不会启动迟到实例。
     */
    private fun applyRuleRefresh(
        profile: EmbeddedClientProfile,
        rules: RoutingRuleDocument,
        rebuildDataPlane: Boolean,
    ): Boolean = synchronized(resourceLock) {
        if (stopping) return@synchronized false
        if (!rebuildDataPlane) {
            if (!routeCoreStarted || !tunnelCoreStarted) return@synchronized false
            NativeRuntime.updateRules(rules.text)
            ProxyRuntime.markRunning(ProxyMode.VPN)
            return@synchronized true
        }
        releaseTunnelResources().getOrThrow()
        if (stopping) return@synchronized false
        startDataPlane(profile, rules)
        ProxyRuntime.markRunning(ProxyMode.VPN)
        true
    }

    /**
     * 在停止请求共用的资源锁内提交初次运行状态。
     * `startGeneration` 标识显式启动代次；已取消时抛错并回滚，系统恢复路径传 null。
     * 只有数据面和持久运行意图同时成功后才发布 RUNNING；停止已开始时返回精确失败并由启动事务回滚。
     */
    private fun publishRunningState(diagnostic: String?, startGeneration: Long?) = synchronized(resourceLock) {
        check(!stopping) { "VPN 数据面启动期间收到停止请求" }
        check(startGeneration == null || StartRequestRegistry.isActive(ProxyMode.VPN, startGeneration)) {
            "VPN 启动请求已被取消"
        }
        check(ClientPreferences(this@ProxyVpnService).writeDesiredRunning(true)) { "运行状态保存失败" }
        ProxyRuntime.markRunning(ProxyMode.VPN)
        ProxyRuntime.updateDiagnostic(ProxyMode.VPN, diagnostic)
    }

    /**
     * 按服务端应用范围建立双栈 TUN，并把系统 DNS 指向规则明确列出的服务器。
     * DNS 数据报仍进入 TUN，再由统一 Native 核心按端口 53 直连，绝不交给上游 SOCKS。
     */
    private fun establishTunnel(scope: VpnApplicationScope, dnsServers: List<String>): ParcelFileDescriptor {
        val builder = Builder()
            .setSession("SOCKS5 客户端")
            .setMtu(tunnelMtu)
            .addAddress(tunnelIpv4, 32)
            .addRoute("0.0.0.0", 0)
            .addAddress(tunnelIpv6, 128)
            .addRoute("::", 0)
            .setBlocking(false)
        dnsServers.forEach(builder::addDnsServer)
        when (scope) {
            VpnApplicationScope.Global -> builder.addDisallowedApplication(packageName)
            is VpnApplicationScope.Packages -> scope.packageNames.forEach { selectedPackage ->
                try {
                    // Native 出口与服务共用应用 UID；捕获自身会把回环路由器的外连重新送回 TUN。
                    require(selectedPackage != packageName) { "服务端规则不能代理客户端自身" }
                    builder.addAllowedApplication(selectedPackage)
                } catch (error: PackageManager.NameNotFoundException) {
                    throw IllegalStateException("服务端规则中的应用不存在：$selectedPackage", error)
                }
            }
        }
        return checkNotNull(builder.establish()) { "系统未建立 VPN 接口" }
    }

    /**
     * 混合模式在 HEV 消费 TUN 包前安装五元组归属查询；Android 10 以下缺少公开 owner UID API，必须明确阻止。
     * `selectedUids` 已完成 sharedUserId 校验；纯全局或纯应用返回固定上下文，不进行逐连接系统查询。
     */
    private fun configureFlowClassification(rules: RoutingRuleDocument, selectedUids: Set<Int>) {
        when (rules.routingContext) {
            RoutingContext.GLOBAL -> VpnFlowClassifier.configureFixed(VpnFlowClassifier.globalContext)
            RoutingContext.SELECTED -> VpnFlowClassifier.configureFixed(VpnFlowClassifier.selectedContext)
            RoutingContext.MIXED -> {
                check(Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) { "混合应用/全局规则要求 Android 10 或更高版本" }
                require(applicationInfo.uid !in selectedUids) { "服务端规则不能代理客户端自身或共享 UID 应用" }
                val connectivityManager = checkNotNull(getSystemService(ConnectivityManager::class.java)) {
                    "系统未提供连接归属服务"
                }
                VpnFlowClassifier.configure(
                    selectedUids,
                    FlowOwnershipResolver { flow ->
                        connectivityManager.getConnectionOwnerUid(
                            flow.protocol,
                            flow.localEndpoint,
                            flow.remoteEndpoint,
                        )
                    },
                )
            }
        }
    }

    /** 每秒读取 Native 单调计数并计算实时速率；统计异常终止生命周期且保留精确原因。 */
    private fun startStatisticsSampling() {
        statisticsJob?.cancel()
        statisticsGeneration += 1
        val activeGeneration = statisticsGeneration
        statisticsJob = serviceScope.launch {
            try {
                var previousUpload = 0L
                var previousDownload = 0L
                while (isActive) {
                    delay(statisticsIntervalMillis)
                    val statistics = readDataPlaneStatistics(activeGeneration) ?: return@launch
                    val upload = statistics[0].coerceAtLeast(previousUpload)
                    val download = statistics[1].coerceAtLeast(previousDownload)
                    ProxyRuntime.updateTraffic(
                        ProxyMode.VPN,
                        TrafficSnapshot(
                            uploadBytes = upload,
                            downloadBytes = download,
                            uploadBytesPerSecond = upload - previousUpload,
                            downloadBytesPerSecond = download - previousDownload,
                        ),
                    )
                    previousUpload = upload
                    previousDownload = download
                }
            } catch (cancellation: CancellationException) {
                throw cancellation
            } catch (failure: Throwable) {
                failRunningDataPlane(failure)
            }
        }
    }

    /**
     * 在资源锁内先确认 HEV 持续运行，再读取统一 Native 统计。
     * `expectedGeneration` 标识启动该协程的数据面代次；热更新后的迟到采样返回 null，不读取新实例或误报故障。
     */
    private fun readDataPlaneStatistics(expectedGeneration: Long): LongArray? = synchronized(resourceLock) {
        if (expectedGeneration != statisticsGeneration || !tunnelCoreStarted || !routeCoreStarted) {
            return@synchronized null
        }
        TProxyService.TProxyRuntimeError()?.let { error(it) }
        NativeRuntime.stats()
    }

    /** 取消云更新生命周期后回收全部连接和 TUN；完成前不会发布已停止状态。 */
    private fun stopTunnel() {
        if (!transitionToStopping()) return
        ProxyRuntime.markStopping()
        serviceScope.launch {
            lifecycleJob?.cancelAndJoin()
            lifecycleJob = null
            val desiredStateFailure = persistStoppedIntent().exceptionOrNull()
            val cleanupFailure = releaseTunnelResources().exceptionOrNull()
            stopForeground(STOP_FOREGROUND_REMOVE)
            val stopFailure = desiredStateFailure ?: cleanupFailure
            if (desiredStateFailure != null && cleanupFailure != null) desiredStateFailure.addSuppressed(cleanupFailure)
            if (stopFailure == null) ProxyRuntime.markStopped()
            else ProxyRuntime.markFailed(ProxyMode.VPN, stopFailure.message ?: "VPN 数据面停止失败")
            stopSelf()
        }
    }

    /** 启动失败时清理运行意图、Native、TUN 与秘密配置，并把组合失败投影到界面。 */
    private fun failAndStop(failure: Throwable) {
        transitionToStopping()
        val desiredStateFailure = persistStoppedIntent().exceptionOrNull()
        val cleanupFailure = releaseTunnelResources().exceptionOrNull()
        var message = failure.message ?: "VPN 数据面启动失败"
        desiredStateFailure?.let { message = appendFailure(message, it, "运行意图清理") }
        cleanupFailure?.let { message = appendFailure(message, it, "资源清理") }
        ProxyRuntime.markFailed(ProxyMode.VPN, message)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    /** Native 统计失败代表数据面状态不可再信任；取消规则循环并同步回收当前会话。 */
    private fun failRunningDataPlane(failure: Throwable) {
        if (!transitionToStopping()) return
        lifecycleJob?.cancel()
        val desiredStateFailure = persistStoppedIntent().exceptionOrNull()
        val cleanupFailure = releaseTunnelResources().exceptionOrNull()
        var message = failure.message ?: "Native VPN 数据面统计读取失败"
        desiredStateFailure?.let { message = appendFailure(message, it, "运行意图清理") }
        cleanupFailure?.let { message = appendFailure(message, it, "资源清理") }
        ProxyRuntime.markFailed(ProxyMode.VPN, message)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    /** 以统计任务、Native、TUN 的固定顺序回收资源，并保留所有清理错误。 */
    private fun releaseTunnelResources(): Result<Unit> = synchronized(resourceLock) {
        var cleanupFailure: Throwable? = null
        fun collectFailure(cleanup: () -> Unit) {
            runCatching(cleanup).onFailure { failure ->
                if (cleanupFailure == null) cleanupFailure = failure else cleanupFailure.addSuppressed(failure)
            }
        }
        statisticsGeneration += 1
        statisticsJob?.cancel()
        statisticsJob = null
        if (tunnelCoreStarted) collectFailure {
            TProxyService.TProxyStopService()
            tunnelCoreStarted = false
        }
        VpnFlowClassifier.clear()
        collectFailure {
            tunnelDescriptor?.close()
            tunnelDescriptor = null
        }
        if (routeCoreStarted) collectFailure {
            NativeRuntime.stop()
            routeCoreStarted = false
        }
        collectFailure { NativeRuntime.setVpnSocketProtector(null) }
        cleanupFailure?.let { Result.failure(it) } ?: Result.success(Unit)
    }

    /** 同步关闭持久运行意图；失败返回 Result，防止系统重建服务后意外恢复旧模式。 */
    private fun persistStoppedIntent(): Result<Unit> = runCatching {
        check(ClientPreferences(this).writeDesiredRunning(false)) { "运行状态保存失败" }
    }

    /**
     * 在数据面资源锁内只提交一次停止转换。
     * 返回 false 表示另一条停止或故障路径已经取得所有权，调用方不得重复发布状态或启动清理协程。
     */
    private fun transitionToStopping(): Boolean = synchronized(resourceLock) {
        if (stopping) return@synchronized false
        stopping = true
        true
    }

    /** 把清理阶段错误附加到主错误文本；只公开消息，不输出规则正文或内置凭据。 */
    private fun appendFailure(primary: String, additional: Throwable, stage: String): String =
        "$primary；$stage 失败：${additional.message ?: "未知错误"}"

    private companion object {
        const val maximumTunnelConfigurationBytes = 4096
        const val tunnelMtu = 8500
        const val tunnelIpv4 = "198.18.0.1"
        const val tunnelIpv6 = "fc00::1"
        const val statisticsIntervalMillis = 1_000L
        const val ruleRefreshIntervalMillis = 5 * 60 * 1_000L
    }
}

/**
 * 判断 TUN 捕获范围与 HEV 五元组分类状态是否完全一致。
 * 混合上下文或 proxy_app UID 集合变化必须重启 HEV，不能只换 Native 规则后沿用旧归属投影。
 */
internal fun RoutingRuleDocument.vpnDataPlaneScopeEquals(other: RoutingRuleDocument): Boolean =
    vpnScope == other.vpnScope && routingContext == other.routingContext && proxyPackages == other.proxyPackages
