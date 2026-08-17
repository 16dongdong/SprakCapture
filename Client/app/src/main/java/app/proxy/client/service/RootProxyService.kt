package app.proxy.client.service

import android.app.Service
import android.content.Intent
import android.os.IBinder
import android.os.SystemClock
import app.proxy.client.certificate.RootCertificateTrustManager
import app.proxy.client.config.ClientPreferences
import app.proxy.client.config.EmbeddedNodeProvider
import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.domain.ProxyMode
import app.proxy.client.domain.TrafficSnapshot
import app.proxy.client.routing.RoutingRuleDocument
import app.proxy.client.routing.RoutingRuleRepository
import app.proxy.client.runtime.InternalSocksCredentials
import app.proxy.client.runtime.NativeRuntimeConfiguration
import app.proxy.client.runtime.ProxyRuntime
import app.proxy.client.runtime.ProxyServiceController
import app.proxy.client.runtime.RootAccess
import app.proxy.client.runtime.RootCompanionProcess
import app.proxy.client.runtime.StartRequestRegistry
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

/** 在应用前台服务内运行统一 Native 核心，并用 Root iptables 提供 TCP/UDP 透明入口。 */
class RootProxyService : Service() {
    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val resourceLock = Any()
    private val packageChangeMonitor by lazy { PackageChangeMonitor(this) }
    private val packageScopeResolver by lazy { PackageScopeResolver(this) }
    private lateinit var iptablesController: RootIptablesController
    private var lifecycleJob: Job? = null
    private var statisticsJob: Job? = null
    private var statisticsGeneration = 0L
    private var companionProcess: RootCompanionProcess? = null
    private var activeStartGeneration: Long? = null
    @Volatile private var stopping = false

    /** 初始化只持应用 Context 的 iptables 控制器；Native 仍运行在当前应用进程，不复制 Root 可执行文件。 */
    override fun onCreate() {
        super.onCreate()
        iptablesController = RootIptablesController(this)
        packageChangeMonitor.start()
    }

    /** 处理显式启停动作；空 Intent 只按持久运行意图恢复，避免系统重建时读取第二套配置。 */
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ProxyServiceController.ACTION_START -> {
                val generation = intent.activeStartGenerationOrNull(ProxyMode.ROOT)
                if (generation == null) rejectStaleStart(startId) else startRootProxy(generation)
            }
            ProxyServiceController.ACTION_STOP -> {
                StartRequestRegistry.cancel(ProxyMode.ROOT)
                stopRootProxy()
            }
            null -> restoreAfterSystemRestart()
        }
        return START_STICKY
    }

    /**
     * 用户划掉任务时撤销持续运行意图并进入统一停止事务。
     * 若应用进程同时被系统终止，Root 伴随进程仍会通过控制管道 EOF 独立执行同一幂等清理命令。
     */
    override fun onTaskRemoved(rootIntent: Intent?) {
        StartRequestRegistry.cancel(ProxyMode.ROOT)
        stopRootProxy()
        super.onTaskRemoved(rootIntent)
    }

    /**
     * 处理 START_STICKY 的空 Intent；只有运行意图开启且持久模式仍为 Root 才恢复。
     * 偏好损坏或模式已切到 VPN 时结束旧组件，避免旧透明链与目标 TUN 并存。
     */
    private fun restoreAfterSystemRestart() {
        val shouldRestore = ClientPreferences(this).shouldRestore(ProxyMode.ROOT).getOrElse { failure ->
            ProxyRuntime.markFailed(ProxyMode.ROOT, failure.message ?: "Root 恢复配置读取失败")
            stopSelf()
            return
        }
        if (shouldRestore) startRootProxy(null) else stopSelf()
    }

    /**
     * 拒绝已经取消的迟到 START；空闲实例按当前 startId 结束，已有有效生命周期时只忽略旧请求。
     * 旧 Intent 因此不能关闭随后已运行的新代次，也不能单独留下常驻 Service。
     */
    private fun rejectStaleStart(startId: Int) {
        if (lifecycleJob?.isActive != true && companionProcess == null) stopSelf(startId)
    }

    /** 明确拒绝绑定；界面只消费 ProxyRuntime 投影，不能绕过服务生命周期操作 Native。 */
    override fun onBind(intent: Intent?): IBinder? = null

    /** 系统销毁服务时完成最终幂等回滚，确保 iptables 不会指向已经卸载的应用进程。 */
    override fun onDestroy() {
        transitionToStopping()
        activeStartGeneration?.let { StartRequestRegistry.cancel(ProxyMode.ROOT, it) }
        activeStartGeneration = null
        lifecycleJob?.cancel()
        val packageMonitorFailure = runCatching(packageChangeMonitor::stop).exceptionOrNull()
        val resourceFailure = releaseResources().exceptionOrNull()
        val cleanupFailure = resourceFailure ?: packageMonitorFailure
        if (resourceFailure != null && packageMonitorFailure != null) {
            resourceFailure.addSuppressed(packageMonitorFailure)
        }
        if (cleanupFailure != null) {
            ProxyRuntime.markFailed(ProxyMode.ROOT, cleanupFailure.message ?: "Root 数据面清理失败")
        }
        serviceScope.cancel()
        super.onDestroy()
    }

    /**
     * 在 IO 生命周期中下载首份规则、启动 Native 与透明链，并持续检查云端规则更新。
     * `startGeneration` 为 null 仅用于系统恢复；显式请求在超时取消后不得继续提交运行状态。
     */
    private fun startRootProxy(startGeneration: Long?) {
        if (lifecycleJob?.isActive == true || companionProcess != null) return
        activeStartGeneration = startGeneration
        stopping = false
        ProxyRuntime.markStarting(ProxyMode.ROOT)
        val profile = EmbeddedNodeProvider.current(this)
        ServiceNotification.ensureChannel(this)
        startForeground(
            ServiceNotification.NOTIFICATION_ID,
            ServiceNotification.create(this, ProxyMode.ROOT),
        )
        lifecycleJob = serviceScope.launch {
            try {
                check(RootAccess.isAvailable()) { "设备未授予 Root 权限" }
                val repository = RoutingRuleRepository(this@RootProxyService)
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
     * 先启动具有内核能力的 Root 伴随进程，再挂接 iptables，保证透明 socket 能保留 UDP 原目标。
     * 配置只经匿名管道传递；iptables 安装失败会立即停止伴随进程，并由上层报告组合错误。
     */
    private fun startDataPlane(profile: EmbeddedClientProfile, rules: RoutingRuleDocument) = synchronized(resourceLock) {
        check(!stopping) { "Root 数据面已经进入停止流程" }
        val packageScope = packageScopeResolver.resolve(rules)
        val startedProcess = RootCompanionProcess.start(
            this,
            NativeRuntimeConfiguration.create(profile, InternalSocksCredentials.generate()),
            rules.text,
        )
        companionProcess = startedProcess
        runCatching { iptablesController.apply(rules, packageScope) }.onFailure { failure ->
            runCatching(::stopCompanion).exceptionOrNull()?.let(failure::addSuppressed)
            throw failure
        }
        startStatisticsSampling()
    }

    /**
     * 由包广播或五分钟规则截止时间唤醒监控循环。
     * 包事件必须在资源锁内重新解析 UID 并重建 Native+iptables，规则范围不变时才允许仅热更正文。
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
            ProxyRuntime.updateDiagnostic(ProxyMode.ROOT, refresh.diagnostic)
            if (!refresh.changed || stopping) continue
            ProxyRuntime.markStopping()
            val rebuildDataPlane = !refresh.document.captureScopeEquals(activeDocument)
            if (!applyRuleRefresh(profile, refresh.document, rebuildDataPlane)) return
            activeDocument = refresh.document
        }
    }

    /**
     * 在透明代理已经接管流量后按账号鉴权同步根证书。
     * 定时规则刷新共用本边界，使服务端轮换 CA 后至多一个刷新周期更新设备信任；失败会进入统一停机回滚。
     */
    private fun synchronizeCertificateTrust(profile: EmbeddedClientProfile) {
        if (!ClientPreferences(this).read().certificateTrustEnabled) return
        RootCertificateTrustManager().synchronize(profile)
    }

    /**
     * 在 stop/onDestroy 共用的资源锁内更新规则或重建 Native+iptables。
     * `rebuildDataPlane` 为 true 时在释放旧实例后再次检查停止标记，避免取消过程启动迟到透明链。
     */
    private fun applyRuleRefresh(
        profile: EmbeddedClientProfile,
        rules: RoutingRuleDocument,
        rebuildDataPlane: Boolean,
    ): Boolean = synchronized(resourceLock) {
        if (stopping) return@synchronized false
        if (!rebuildDataPlane) {
            val process = companionProcess ?: return@synchronized false
            process.updateRules(rules.text)
            ProxyRuntime.markRunning(ProxyMode.ROOT)
            return@synchronized true
        }
        releaseResources().getOrThrow()
        if (stopping) return@synchronized false
        startDataPlane(profile, rules)
        ProxyRuntime.markRunning(ProxyMode.ROOT)
        true
    }

    /**
     * 在停止请求共用的资源锁内提交初次运行状态。
     * `startGeneration` 标识显式启动代次；已取消时抛错并由上层完整清理，系统恢复路径传 null。
     * Native、iptables 和持久运行意图必须同时有效；停止已开始时抛错并交由启动事务完整回滚。
     */
    private fun publishRunningState(diagnostic: String?, startGeneration: Long?) = synchronized(resourceLock) {
        check(!stopping) { "Root 数据面启动期间收到停止请求" }
        check(startGeneration == null || StartRequestRegistry.isActive(ProxyMode.ROOT, startGeneration)) {
            "Root 启动请求已被取消"
        }
        check(ClientPreferences(this@RootProxyService).writeDesiredRunning(true)) { "运行状态保存失败" }
        ProxyRuntime.markRunning(ProxyMode.ROOT)
        ProxyRuntime.updateDiagnostic(ProxyMode.ROOT, diagnostic)
    }

    /** 每秒读取统一 Native 计数并计算实时速率；ABI 或运行异常会终止整个 Root 数据面。 */
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
                    val statistics = readNativeStatistics(activeGeneration) ?: return@launch
                    val upload = statistics[0].coerceAtLeast(previousUpload)
                    val download = statistics[1].coerceAtLeast(previousDownload)
                    ProxyRuntime.updateTraffic(
                        ProxyMode.ROOT,
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
     * 在资源锁内读取 Native 统计，避免热更新先停止核心、迟到采样再把正常重建误报为故障。
     * `expectedGeneration` 必须与当前实例一致；返回 null 表示协程已过期或数据面正在清理。
     */
    private fun readNativeStatistics(expectedGeneration: Long): LongArray? = synchronized(resourceLock) {
        if (expectedGeneration != statisticsGeneration) return@synchronized null
        companionProcess?.stats()
    }

    /** 取消更新任务后清链并停止 Native；资源全部释放前不发布 STOPPED。 */
    private fun stopRootProxy() {
        if (!transitionToStopping()) return
        ProxyRuntime.markStopping()
        serviceScope.launch {
            lifecycleJob?.cancelAndJoin()
            lifecycleJob = null
            val desiredStateFailure = persistStoppedIntent().exceptionOrNull()
            val cleanupFailure = releaseResources().exceptionOrNull()
            stopForeground(STOP_FOREGROUND_REMOVE)
            val stopFailure = desiredStateFailure ?: cleanupFailure
            if (desiredStateFailure != null && cleanupFailure != null) desiredStateFailure.addSuppressed(cleanupFailure)
            if (stopFailure == null) ProxyRuntime.markStopped()
            else ProxyRuntime.markFailed(ProxyMode.ROOT, stopFailure.message ?: "Root 数据面清理失败")
            stopSelf()
        }
    }

    /** 启动或更新失败时清理运行意图和所有 Root 资源，再发布不包含凭据的组合失败原因。 */
    private fun failAndStop(failure: Throwable) {
        transitionToStopping()
        val cleanupFailure = releaseResources().exceptionOrNull()
        val desiredStateFailure = persistStoppedIntent().exceptionOrNull()
        var message = failure.message ?: "Root 数据面启动失败"
        cleanupFailure?.let { message = appendFailure(message, it, "资源清理") }
        desiredStateFailure?.let { message = appendFailure(message, it, "运行意图清理") }
        ProxyRuntime.markFailed(ProxyMode.ROOT, message)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    /** Native 统计失败说明连接状态不可再信任；取消更新循环后按同一路径关闭透明链。 */
    private fun failRunningDataPlane(failure: Throwable) {
        if (!transitionToStopping()) return
        lifecycleJob?.cancel()
        failAndStop(failure)
    }

    /**
     * 先移除流量入口，再停止 Native 连接；两个步骤都会执行并合并错误。
     * 该顺序保证 stop 或热更新期间不会有新数据报进入正在销毁的会话表。
     */
    private fun releaseResources(): Result<Unit> = synchronized(resourceLock) {
        statisticsGeneration += 1
        statisticsJob?.cancel()
        statisticsJob = null
        val iptablesFailure = iptablesController.clear().exceptionOrNull()
        val companionFailure = runCatching(::stopCompanion).exceptionOrNull()
        val failure = iptablesFailure ?: companionFailure
        if (iptablesFailure != null && companionFailure != null) iptablesFailure.addSuppressed(companionFailure)
        failure?.let { Result.failure(it) } ?: Result.success(Unit)
    }

    /** 停止 Root 伴随进程并清空唯一所有者；未启动时保持幂等，失败时仍禁止复用旧实例。 */
    private fun stopCompanion() {
        val process = companionProcess ?: return
        companionProcess = null
        process.stop()
    }

    /** 同步关闭持久运行意图；失败会阻止系统重建服务后意外恢复旧模式。 */
    private fun persistStoppedIntent(): Result<Unit> = runCatching {
        check(ClientPreferences(this).writeDesiredRunning(false)) { "运行状态保存失败" }
    }

    /**
     * 在 Native 与 iptables 共用的资源锁内只提交一次停止转换。
     * 返回 false 表示已有停止路径负责清理，调用方不得重复启动异步停止事务。
     */
    private fun transitionToStopping(): Boolean = synchronized(resourceLock) {
        if (stopping) return@synchronized false
        stopping = true
        true
    }

    /** 合并清理阶段错误文本；只公开阶段和消息，不输出规则正文或内置凭据。 */
    private fun appendFailure(primary: String, additional: Throwable, stage: String): String =
        "$primary；$stage 失败：${additional.message ?: "未知错误"}"

    private companion object {
        const val statisticsIntervalMillis = 1_000L
        const val ruleRefreshIntervalMillis = 5 * 60 * 1_000L
    }
}

/** 判断 Root 的全局兜底开关和选中 UID 投影是否一致；路由动作与 DNS 变化不需要重建系统链。 */
private fun RoutingRuleDocument.captureScopeEquals(other: RoutingRuleDocument): Boolean =
    routingContext == other.routingContext && proxyPackages == other.proxyPackages
