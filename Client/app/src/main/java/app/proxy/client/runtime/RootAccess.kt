package app.proxy.client.runtime

import java.util.concurrent.TimeUnit

/** 封装最小 Root 能力探测，避免 UI 和服务分别拼接 su 命令。 */
object RootAccess {
    /** 在三秒内验证 su 命令确实进入 uid 0；超时、拒绝和输出异常均返回 false。 */
    fun isAvailable(): Boolean = runCatching {
        val process = ProcessBuilder("su", "-c", "id -u").redirectErrorStream(true).start()
        if (!process.waitFor(ROOT_CHECK_TIMEOUT_SECONDS, TimeUnit.SECONDS)) {
            process.destroyForcibly()
            return false
        }
        process.exitValue() == 0 && process.inputStream.bufferedReader().use { it.readText().trim() == "0" }
    }.getOrDefault(false)

    private const val ROOT_CHECK_TIMEOUT_SECONDS = 3L
}
