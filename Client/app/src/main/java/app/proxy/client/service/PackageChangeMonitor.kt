package app.proxy.client.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeoutOrNull

/**
 * 把 Android 包安装、卸载和变更广播折叠为单一生命周期信号。
 * VPN 与 owner iptables 都按 UID 捕获流量；运行期包关系变化必须触发重新校验，不能等待规则 ETag 改变。
 */
internal class PackageChangeMonitor(private val context: Context) {
    private val changes = Channel<Unit>(Channel.CONFLATED)
    private val registration = ReceiverRegistration()
    private val receiver = object : BroadcastReceiver() {
        /** 广播只负责投递无敏感信息的合并信号；包名和 UID 必须在重建事务内重新查询。 */
        override fun onReceive(context: Context?, intent: Intent?) {
            changes.trySend(Unit)
        }
    }

    /** 注册系统包广播；重复注册属于服务生命周期错误并立即失败。 */
    fun start() {
        val filter = IntentFilter().apply {
            addAction(Intent.ACTION_PACKAGE_ADDED)
            addAction(Intent.ACTION_PACKAGE_REMOVED)
            addAction(Intent.ACTION_PACKAGE_CHANGED)
            addDataScheme("package")
        }
        registration.start {
            val unregisterAction = { context.unregisterReceiver(receiver) }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                context.registerReceiver(receiver, filter, Context.RECEIVER_EXPORTED)
            } else {
                @Suppress("DEPRECATION")
                context.registerReceiver(receiver, filter)
            }
            unregisterAction
        }
    }

    /**
     * 在最多 `timeoutMillis` 内等待一次包关系变化。
     * 返回 false 表示规则刷新截止时间已到；协程取消会原样向上抛出，不能伪装成超时。
     */
    suspend fun awaitChange(timeoutMillis: Long): Boolean {
        require(timeoutMillis > 0) { "包变更等待时间必须大于零" }
        return withTimeoutOrNull(timeoutMillis) {
            changes.receive()
            true
        } == true
    }

    /** 注销广播；未启动时保持幂等，供启动失败和系统销毁共用。 */
    fun stop() {
        registration.stop()
    }
}

/**
 * 隔离广播注册句柄的纯 Kotlin 生命周期。
 * 只有注册成功后才保存注销动作；停止会先清空所有权再调用系统，避免异常路径重复注销同一 Receiver。
 */
internal class ReceiverRegistration {
    private var unregister: (() -> Unit)? = null

    /** 执行注册事务并保存唯一注销动作；重复启动直接失败且不会覆盖原句柄。 */
    fun start(register: () -> (() -> Unit)) {
        check(unregister == null) { "包变更监听器已经启动" }
        unregister = register()
    }

    /** 释放注册句柄；未启动时保持幂等，注销失败仍不允许再次操作已失效的系统句柄。 */
    fun stop() {
        val activeUnregister = unregister ?: return
        unregister = null
        activeUnregister()
    }
}
