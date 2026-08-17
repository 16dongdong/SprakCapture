package app.proxy.client.runtime

import app.proxy.client.domain.ConnectionPhase
import app.proxy.client.domain.ProxyMode
import app.proxy.client.domain.ProxyRuntimeState
import app.proxy.client.domain.TrafficSnapshot
import app.proxy.client.domain.userVisibleProxyError
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** 维护同进程服务与 Compose 之间的唯一运行状态流，避免 Activity 重建重置代理状态。 */
object ProxyRuntime {
    private val mutableState = MutableStateFlow(ProxyRuntimeState())
    val state: StateFlow<ProxyRuntimeState> = mutableState.asStateFlow()

    /** 发布启动中状态并清除上一轮错误；调用者必须在实际创建数据面前执行。 */
    fun markStarting(mode: ProxyMode) {
        mutableState.value = ProxyRuntimeState(phase = ConnectionPhase.STARTING, mode = mode)
    }

    /** 发布运行状态；启动时间只在首次进入运行态时记录，便于页面稳定显示持续时长。 */
    fun markRunning(mode: ProxyMode) {
        val previous = mutableState.value
        mutableState.value = previous.copy(
            phase = ConnectionPhase.RUNNING,
            mode = mode,
            startedAtMillis = previous.startedAtMillis ?: System.currentTimeMillis(),
            error = null,
        )
    }

    /** 更新非致命规则同步诊断；只有当前运行数据面可以写入，成功同步传 null 清除旧提示。 */
    fun updateDiagnostic(mode: ProxyMode, diagnostic: String?) {
        val previous = mutableState.value
        if (previous.phase != ConnectionPhase.RUNNING || previous.mode != mode) return
        mutableState.value = previous.copy(
            diagnostic = diagnostic?.let {
                userVisibleProxyError(it, "云规则更新失败，已继续使用上次有效规则")
            },
        )
    }

    /** 发布停止中状态；保留统计直到资源完成回收，避免按钮反馈与真实生命周期错位。 */
    fun markStopping() {
        mutableState.value = mutableState.value.copy(phase = ConnectionPhase.STOPPING, error = null)
    }

    /** 在数据面和系统资源均释放后重置状态；停止完成不携带上一轮累计流量。 */
    fun markStopped() {
        mutableState.value = ProxyRuntimeState()
    }

    /** 发布不可恢复失败及精确原因；服务必须先完成自身回滚再调用本函数。 */
    fun markFailed(mode: ProxyMode, reason: String) {
        mutableState.value = ProxyRuntimeState(
            phase = ConnectionPhase.FAILED,
            mode = mode,
            error = userVisibleProxyError(reason, "代理数据面运行失败"),
        )
    }

    /** 更新累计与实时速率；仅运行中的相同数据面可写入，防止迟到采样污染新会话。 */
    fun updateTraffic(mode: ProxyMode, traffic: TrafficSnapshot) {
        val previous = mutableState.value
        if (previous.phase != ConnectionPhase.RUNNING || previous.mode != mode) return
        mutableState.value = previous.copy(
            uploadBytes = traffic.uploadBytes,
            downloadBytes = traffic.downloadBytes,
            uploadBytesPerSecond = traffic.uploadBytesPerSecond,
            downloadBytesPerSecond = traffic.downloadBytesPerSecond,
        )
    }
}
