package app.proxy.client.ui

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import app.proxy.client.config.ClientPreferences
import app.proxy.client.domain.ClientSettings
import app.proxy.client.domain.ProxyMode
import app.proxy.client.domain.ProxyRuntimeState
import app.proxy.client.domain.userVisibleProxyError
import app.proxy.client.runtime.ProxyRuntime
import app.proxy.client.runtime.ProxyServiceController
import app.proxy.client.runtime.RootAccess
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** 汇总数据面设置和服务状态，Compose 页面只发送领域意图而不直接操作系统服务。 */
class ClientViewModel(application: Application) : AndroidViewModel(application) {
    private val preferences = ClientPreferences(application)
    private val serviceController = ProxyServiceController(application)
    private var modeSwitchJob: Job? = null
    private var certificateTrustJob: Job? = null
    private val mutableUiState = MutableStateFlow(ClientUiState())
    val uiState: StateFlow<ClientUiState> = mutableUiState.asStateFlow()

    init {
        loadSettings()
        observeRuntime()
        inspectRootCapability()
    }

    /** 返回系统 VPN 授权请求；调用者在结果成功后再次执行 `startProxy`。 */
    fun vpnPermissionIntent() = serviceController.vpnPermissionIntent()

    /** 启动当前配置的数据面；失败消息写入单次 UI 提示，不改变已持久化设置。 */
    fun startProxy() {
        serviceController.start().onFailure(::publishFailure)
    }

    /** 请求当前数据面停止；控制器保持幂等，失败时保留正在运行的真实状态。 */
    fun stopProxy() {
        serviceController.stop().onFailure(::publishFailure)
    }

    /**
     * 切换 VPN/Root 模式；运行中会等待旧数据面完整回收后立即启动目标模式。
     * Root 不可用或已有切换任务时拒绝执行，避免并发服务争用 TUN 与 iptables。
     */
    fun setMode(mode: ProxyMode) {
        if (mode == ProxyMode.ROOT && !mutableUiState.value.rootAvailable) {
            publishFailure(IllegalStateException("设备未授予 Root 权限"))
            return
        }
        if (modeSwitchJob?.isActive == true) return
        modeSwitchJob = viewModelScope.launch {
            val switchResult = serviceController.switchMode(mode)
            val failure = switchResult.exceptionOrNull()
            if (failure == null) {
                mutableUiState.update {
                    it.copy(settings = it.settings.copy(mode = mode), message = "已切换到${mode.displayName()}模式")
                }
            } else {
                // 控制器可能已持久化目标模式但启动失败；重新读取可避免开关显示旧值并在下次启动使用另一模式。
                val persistedSettings = withContext(Dispatchers.IO) { runCatching(preferences::read).getOrNull() }
                if (persistedSettings != null) {
                    mutableUiState.update { it.copy(settings = persistedSettings) }
                }
                publishFailure(failure)
            }
        }
    }

    /**
     * 切换证书信任并显示事务进度。
     * 控制器负责停机、Root 清理与重启；失败后重新读取权威偏好，界面不会保留未生效的开关值。
     */
    fun setCertificateTrustEnabled(enabled: Boolean) {
        if (enabled && !mutableUiState.value.rootAvailable) {
            publishFailure(IllegalStateException("设备未授予 Root 权限"))
            return
        }
        if (certificateTrustJob?.isActive == true) return
        certificateTrustJob = viewModelScope.launch {
            mutableUiState.update { it.copy(certificateTrustUpdating = true) }
            try {
                serviceController.setCertificateTrustEnabled(enabled).getOrThrow()
                val settings = withContext(Dispatchers.IO) { preferences.read() }
                mutableUiState.update {
                    it.copy(
                        settings = settings,
                        message = if (enabled) "证书信任已开启" else "证书信任已关闭",
                    )
                }
            } catch (failure: Throwable) {
                val settings = withContext(Dispatchers.IO) { runCatching(preferences::read).getOrNull() }
                if (settings != null) mutableUiState.update { it.copy(settings = settings) }
                publishFailure(failure)
            } finally {
                mutableUiState.update { it.copy(certificateTrustUpdating = false) }
            }
        }
    }

    /** 清除已展示的一次性消息；运行状态错误由 ProxyRuntime 独立保留，不受本函数影响。 */
    fun consumeMessage() {
        mutableUiState.update { it.copy(message = null) }
    }

    /**
     * 重新执行有界 Root 探测，供用户在 Root 管理器完成首次授权后立即刷新开关。
     * 探测在 IO 线程运行；拒绝或超时只更新能力状态，不改变已经保存的代理模式。
     */
    fun refreshRootCapability() {
        inspectRootCapability()
    }

    /** 从偏好读取数据面设置；读取失败会精确展示且不伪造默认配置已经持久化。 */
    private fun loadSettings() {
        viewModelScope.launch {
            runCatching {
                val settings = withContext(Dispatchers.IO) { preferences.read() }
                mutableUiState.update { it.copy(settings = settings) }
            }.onFailure {
                mutableUiState.update { current ->
                    current.copy(message = userVisibleProxyError(it.message, "客户端配置读取失败"))
                }
            }
        }
    }

    /** 持续合并服务状态；同进程 StateFlow 可跨 Activity 重建保持最新生命周期。 */
    private fun observeRuntime() {
        viewModelScope.launch {
            ProxyRuntime.state.collectLatest { runtime ->
                mutableUiState.update { it.copy(runtime = runtime) }
            }
        }
    }

    /** 在 IO 线程进行有界 Root 探测，结果只影响模式开关而不主动弹出授权窗口。 */
    private fun inspectRootCapability() {
        viewModelScope.launch {
            val available = withContext(Dispatchers.IO) { RootAccess.isAvailable() }
            mutableUiState.update { it.copy(rootAvailable = available, rootInspectionFinished = true) }
        }
    }

    /** 将异常转换为中文可见消息；空异常信息使用稳定通用文本且不吞掉失败状态。 */
    private fun publishFailure(failure: Throwable) {
        mutableUiState.update { it.copy(message = userVisibleProxyError(failure.message, "操作失败")) }
    }
}

/** 返回模式的稳定中文短名，仅用于用户完成切换后的即时反馈。 */
private fun ProxyMode.displayName(): String = if (this == ProxyMode.ROOT) "Root 透明代理" else "VPN"

/** Compose 根节点消费的不可变状态；页面导航属于临时 UI 状态，不写入此领域快照。 */
data class ClientUiState(
    val settings: ClientSettings = ClientSettings(),
    val runtime: ProxyRuntimeState = ProxyRuntimeState(),
    val rootAvailable: Boolean = false,
    val rootInspectionFinished: Boolean = false,
    val certificateTrustUpdating: Boolean = false,
    val message: String? = null,
)
