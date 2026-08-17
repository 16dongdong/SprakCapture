package app.proxy.client.config

import android.annotation.SuppressLint
import android.content.Context
import app.proxy.client.domain.ClientSettings
import app.proxy.client.domain.ProxyMode
import java.security.KeyStore

/** 只持久化用户选择的数据面与运行意图；节点、凭据和规则不允许被本地偏好覆盖。 */
@SuppressLint("UseKtx")
class ClientPreferences(context: Context) {
    private val preferences = context.getSharedPreferences(PREFERENCES_NAME, Context.MODE_PRIVATE)

    init {
        removeRetiredLocalConfiguration()
    }

    /** 读取当前数据面设置快照；枚举损坏时抛出异常，调用方必须报告并阻止启动。 */
    fun read(): ClientSettings {
        val persistedMode = preferences.getString(KEY_MODE, ProxyMode.VPN.name)
            ?: throw IllegalStateException("代理模式配置为空")
        val mode = runCatching { ProxyMode.valueOf(persistedMode) }
            .getOrElse { failure -> throw IllegalStateException("代理模式配置损坏", failure) }
        return ClientSettings(
            mode = mode,
            certificateTrustEnabled = preferences.getBoolean(KEY_CERTIFICATE_TRUST, false),
        )
    }

    /** 保存代理模式；热切换控制器保证旧数据面已回收，写盘失败直接返回 false。 */
    fun writeMode(mode: ProxyMode): Boolean = preferences.edit().putString(KEY_MODE, mode.name).commit()

    /**
     * 持久化用户是否要求 Root 信任当前抓包根证书。
     * 本字段只表达意图，证书正文始终在代理通道建立后按账号鉴权下载，禁止写入偏好或安装包。
     */
    fun writeCertificateTrustEnabled(enabled: Boolean): Boolean =
        preferences.edit().putBoolean(KEY_CERTIFICATE_TRUST, enabled).commit()

    /** 记录用户是否期望代理持续运行；仅供 Android 重建 START_STICKY 服务时恢复，不替代真实状态。 */
    fun writeDesiredRunning(enabled: Boolean): Boolean =
        preferences.edit().putBoolean(KEY_DESIRED_RUNNING, enabled).commit()

    /** 读取持久运行意图；默认关闭，安装或升级不会未经用户操作自动建立 VPN。 */
    fun desiredRunning(): Boolean = preferences.getBoolean(KEY_DESIRED_RUNNING, false)

    /**
     * 删除旧版应用范围、账号密文及其 Keystore 密钥。
     * 云规则和打包凭据成为唯一来源后继续保留这些值会形成隐蔽第二入口；清理失败直接阻止客户端读取设置。
     */
    private fun removeRetiredLocalConfiguration() {
        val containsRetiredValues = retiredPreferenceKeys.any(preferences::contains)
        if (containsRetiredValues) {
            val editor = preferences.edit()
            retiredPreferenceKeys.forEach(editor::remove)
            check(editor.commit()) { "旧版本地代理配置清理失败" }
        }
        val keyStore = KeyStore.getInstance(androidKeyStoreProvider).apply { load(null) }
        if (keyStore.containsAlias(retiredCredentialKeyAlias)) keyStore.deleteEntry(retiredCredentialKeyAlias)
    }

    private companion object {
        const val PREFERENCES_NAME = "proxyClientSettings"
        const val KEY_MODE = "proxyMode"
        const val KEY_DESIRED_RUNNING = "desiredRunning"
        const val KEY_CERTIFICATE_TRUST = "certificateTrustEnabled"
        const val androidKeyStoreProvider = "AndroidKeyStore"
        const val retiredCredentialKeyAlias = "proxyClientAccountCredentials"
        val retiredPreferenceKeys = setOf("globalProxy", "selectedPackages", "accountUsername", "accountPassword")
    }
}
