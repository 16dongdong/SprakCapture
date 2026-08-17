package app.proxy.client.runtime

import android.content.Context
import android.content.Intent
import android.net.VpnService
import androidx.core.content.ContextCompat
import app.proxy.client.certificate.RootCertificateTrustManager
import app.proxy.client.config.ClientPreferences
import app.proxy.client.domain.ConnectionPhase
import app.proxy.client.domain.ProxyMode
import app.proxy.client.service.ProxyVpnService
import app.proxy.client.service.RootProxyService
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.withTimeout
import java.util.concurrent.atomic.AtomicLong

/** 把界面意图转换为 Android 服务动作，并统一执行内置配置校验与数据面热切换。 */
class ProxyServiceController(private val context: Context) {
    private val preferences = ClientPreferences(context)
    private val certificateTrustManager = RootCertificateTrustManager()

    /** 返回系统 VPN 授权 Intent；返回 null 表示授权已存在，可直接启动。 */
    fun vpnPermissionIntent(): Intent? = VpnService.prepare(context)

    /**
     * 启动用户当前选择的数据面；运行于界面动作协程，只生成代次并投递唯一服务组件。
     * 静态资料只在服务 IO 生命周期解封一次，认证或字段失败会进入可观察 FAILED 而不在界面进程额外复制明文。
     */
    fun start(): Result<Unit> = runCatching {
        check(ProxyRuntime.state.value.phase in setOf(ConnectionPhase.STOPPED, ConnectionPhase.FAILED)) {
            "代理正在切换状态"
        }
        val generation = dispatchStart(preferences.read().mode)
        check(generation > 0) { "代理启动代次生成失败" }
    }

    /** 请求当前数据面停止；停止动作保持幂等，不运行时直接返回成功。 */
    fun stop(): Result<Unit> = runCatching {
        val runtimeState = ProxyRuntime.state.value
        if (!shouldDispatchStop(runtimeState.phase)) return@runCatching
        val mode = runtimeState.mode ?: preferences.read().mode
        StartRequestRegistry.cancel(mode)
        // STOP 只投递给已启动的前台服务；重新用 startForegroundService 创建一个停止服务会触发系统晋升超时。
        dispatchStop(mode)
    }

    /**
     * 有序停止当前服务、等待全部连接与系统资源释放、持久化目标模式并立即启动新数据面。
     * 超时、清理失败或新服务启动失败均返回失败，禁止两个模式同时持有 TUN/iptables。
     */
    suspend fun switchMode(targetMode: ProxyMode): Result<Unit> = runCatching {
        val state = ProxyRuntime.state.value
        if (state.phase == ConnectionPhase.RUNNING && state.mode == targetMode) return@runCatching
        check(state.phase !in setOf(ConnectionPhase.STARTING, ConnectionPhase.STOPPING)) { "代理正在切换状态" }
        if (state.phase == ConnectionPhase.RUNNING) {
            stop().getOrThrow()
            val stoppedState = awaitState(modeSwitchTimeoutMillis, "旧代理模式未在 45 秒内完成资源回收") {
                it in setOf(ConnectionPhase.STOPPED, ConnectionPhase.FAILED)
            }
            check(stoppedState.phase == ConnectionPhase.STOPPED) {
                stoppedState.error ?: "当前代理资源清理失败"
            }
        }
        check(preferences.writeMode(targetMode)) { "代理模式保存失败" }
        if (state.phase == ConnectionPhase.RUNNING) {
            val startGeneration = dispatchStart(targetMode)
            val startedState = try {
                awaitState(targetStartTimeoutMillis, "目标代理模式未在 45 秒内完成启动") {
                    isTargetStartCompleted(it)
                }
            } catch (failure: Throwable) {
                // 先取消本代次，再无条件向已知目标组件投递 STOP；迟到 START 会在服务入口和发布 RUNNING 前两次校验代次。
                StartRequestRegistry.cancel(targetMode, startGeneration)
                runCatching { dispatchStop(targetMode) }.onFailure(failure::addSuppressed)
                throw failure
            }
            if (startedState.phase != ConnectionPhase.RUNNING) {
                StartRequestRegistry.cancel(targetMode, startGeneration)
            }
            check(startedState.phase == ConnectionPhase.RUNNING) { startedState.error ?: "目标代理模式启动失败" }
        }
    }

    /**
     * 原子切换证书信任意图。
     * 运行中的代理会先完整停止；关闭时立即撤销系统信任，开启时由随后启动的数据面通过认证通道下载证书。
     * 持久化、Root 操作或重启失败会恢复原意图并尝试恢复原数据面，避免开关显示与实际状态分叉。
     */
    suspend fun setCertificateTrustEnabled(enabled: Boolean): Result<Unit> = runCatching {
        if (enabled) check(RootAccess.isAvailable()) { "设备未授予 Root 权限" }
        val previousSettings = preferences.read()
        if (previousSettings.certificateTrustEnabled == enabled) return@runCatching
        val runtimeState = ProxyRuntime.state.value
        check(runtimeState.phase !in setOf(ConnectionPhase.STARTING, ConnectionPhase.STOPPING)) { "代理正在切换状态" }
        val wasRunning = runtimeState.phase == ConnectionPhase.RUNNING
        val activeMode = runtimeState.mode ?: previousSettings.mode
        try {
            if (wasRunning) stopAndAwait()
            check(preferences.writeCertificateTrustEnabled(enabled)) { "证书信任设置保存失败" }
            if (!enabled) certificateTrustManager.remove()
            if (wasRunning) startAndAwait(activeMode)
        } catch (failure: Throwable) {
            runCatching { preferences.writeCertificateTrustEnabled(previousSettings.certificateTrustEnabled) }
                .onSuccess { restored -> if (!restored) failure.addSuppressed(IllegalStateException("证书信任设置恢复失败")) }
                .onFailure(failure::addSuppressed)
            if (wasRunning && ProxyRuntime.state.value.phase != ConnectionPhase.RUNNING) {
                runCatching { startAndAwait(activeMode) }.onFailure(failure::addSuppressed)
            }
            throw failure
        }
    }

    /** 停止当前数据面并等待资源完全回收；FAILED 不是成功停止。 */
    private suspend fun stopAndAwait() {
        stop().getOrThrow()
        val stopped = awaitState(modeSwitchTimeoutMillis, "代理未在 45 秒内完成资源回收") {
            it in setOf(ConnectionPhase.STOPPED, ConnectionPhase.FAILED)
        }
        check(stopped.phase == ConnectionPhase.STOPPED) { stopped.error ?: "代理资源清理失败" }
    }

    /** 启动指定模式并等待真实 RUNNING；超时或失败会取消代次并投递 STOP。 */
    private suspend fun startAndAwait(mode: ProxyMode) {
        val generation = dispatchStart(mode)
        val started = try {
            awaitState(targetStartTimeoutMillis, "代理未在 45 秒内完成启动", ::isTargetStartCompleted)
        } catch (failure: Throwable) {
            StartRequestRegistry.cancel(mode, generation)
            runCatching { dispatchStop(mode) }.onFailure(failure::addSuppressed)
            throw failure
        }
        check(started.phase == ConnectionPhase.RUNNING) { started.error ?: "代理启动失败" }
    }

    /**
     * 为目标模式创建唯一启动代次并投递前台服务 Intent。
     * 投递失败会撤销该代次，服务只接受仍处于活动状态的 token，避免超时后的迟到 START 建立隐形代理。
     */
    private fun dispatchStart(mode: ProxyMode): Long {
        val generation = StartRequestRegistry.create(mode)
        val serviceClass = serviceClass(mode)
        try {
            ContextCompat.startForegroundService(
                context,
                Intent(context, serviceClass)
                    .setAction(ACTION_START)
                    .putExtra(EXTRA_START_GENERATION, generation),
            )
            return generation
        } catch (failure: Throwable) {
            StartRequestRegistry.cancel(mode, generation)
            throw failure
        }
    }

    /** 向已知模式组件投递停止动作；热切换超时路径不依赖可能尚未更新的 ProxyRuntime 状态。 */
    private fun dispatchStop(mode: ProxyMode) {
        context.startService(Intent(context, serviceClass(mode)).setAction(ACTION_STOP))
    }

    /** 把模式映射到唯一 Android 服务组件，启动与停止必须共用该映射避免交叉投递。 */
    private fun serviceClass(mode: ProxyMode): Class<*> =
        if (mode == ProxyMode.ROOT) RootProxyService::class.java else ProxyVpnService::class.java

    /** 等待生命周期到达指定状态；超时转换为稳定中文原因，避免把协程内部异常直接展示给用户。 */
    private suspend fun awaitState(
        timeoutMillis: Long,
        timeoutMessage: String,
        predicate: (ConnectionPhase) -> Boolean,
    ) = try {
        withTimeout(timeoutMillis) { ProxyRuntime.state.first { predicate(it.phase) } }
    } catch (_: TimeoutCancellationException) {
        throw IllegalStateException(timeoutMessage)
    }

    companion object {
        const val ACTION_START = "app.proxy.client.action.START"
        const val ACTION_STOP = "app.proxy.client.action.STOP"
        const val EXTRA_START_GENERATION = "app.proxy.client.extra.START_GENERATION"
        // 规则请求最多占用 15 秒，Root 进程和 iptables 仍需有界回收；45 秒覆盖完整最坏生命周期。
        const val modeSwitchTimeoutMillis = 45_000L
        const val targetStartTimeoutMillis = 45_000L
    }
}

/**
 * 保存当前进程最近一次服务启动代次。
 * token 只用于排序 Android Intent 与超时取消，不含配置；新代次会撤销两种模式的旧请求以保持互斥。
 */
internal object StartRequestRegistry {
    private val nextGeneration = AtomicLong(0)
    private val activeRequests = mutableMapOf<ProxyMode, Long>()

    /** 创建正数且进程内唯一的代次；溢出到非正数时从1重新开始并仍清空旧请求。 */
    @Synchronized
    fun create(mode: ProxyMode): Long {
        var generation = nextGeneration.incrementAndGet()
        if (generation <= 0) {
            nextGeneration.set(1)
            generation = 1
        }
        activeRequests.clear()
        activeRequests[mode] = generation
        return generation
    }

    /** 精确取消一个模式代次；旧超时回调不得误删随后创建的新请求。 */
    @Synchronized
    fun cancel(mode: ProxyMode, generation: Long) {
        if (activeRequests[mode] == generation) activeRequests.remove(mode)
    }

    /** 取消目标模式当前代次，供用户主动停止路径阻止仍在队列中的 START。 */
    @Synchronized
    fun cancel(mode: ProxyMode) {
        activeRequests.remove(mode)
    }

    /** 服务入口和 RUNNING 提交点都调用本函数；false 表示请求已超时、被停止或被新代次取代。 */
    @Synchronized
    fun isActive(mode: ProxyMode, generation: Long): Boolean = activeRequests[mode] == generation
}

/** 热切换只有在目标服务真正运行或明确失败时结束等待，STARTING 绝不视作切换成功。 */
internal fun isTargetStartCompleted(phase: ConnectionPhase): Boolean =
    phase in setOf(ConnectionPhase.RUNNING, ConnectionPhase.FAILED)

/** STOPPED/FAILED 已没有可释放的数据面，不创建只为停止而存在的新服务实例。 */
internal fun shouldDispatchStop(phase: ConnectionPhase): Boolean =
    phase in setOf(ConnectionPhase.STARTING, ConnectionPhase.RUNNING, ConnectionPhase.STOPPING)
