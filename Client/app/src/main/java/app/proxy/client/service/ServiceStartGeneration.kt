package app.proxy.client.service

import android.content.Intent
import app.proxy.client.config.ClientPreferences
import app.proxy.client.domain.ProxyMode
import app.proxy.client.runtime.ProxyServiceController
import app.proxy.client.runtime.StartRequestRegistry

/**
 * 从显式 ACTION_START 读取并校验进程内启动代次。
 * 缺失、非正数或已经取消时返回 null，服务只结束本次实例，防止排队 Intent 在界面超时后迟到启动数据面。
 */
internal fun Intent.activeStartGenerationOrNull(mode: ProxyMode): Long? {
    val generation = getLongExtra(ProxyServiceController.EXTRA_START_GENERATION, invalidStartGeneration)
    return generation.takeIf { it > 0 && StartRequestRegistry.isActive(mode, it) }
}

private const val invalidStartGeneration = -1L

/**
 * 判断 START_STICKY 空 Intent 是否属于当前服务模式。
 * 运行意图和持久模式必须同时匹配；读取损坏通过 Result 返回，调用方应发布失败并结束旧组件。
 */
internal fun ClientPreferences.shouldRestore(mode: ProxyMode): Result<Boolean> = runCatching {
    shouldRestoreService(desiredRunning(), read().mode, mode)
}

/** 纯逻辑恢复判定；持久模式只允许对应的一个 START_STICKY 服务恢复。 */
internal fun shouldRestoreService(desiredRunning: Boolean, persistedMode: ProxyMode, serviceMode: ProxyMode): Boolean =
    desiredRunning && persistedMode == serviceMode
