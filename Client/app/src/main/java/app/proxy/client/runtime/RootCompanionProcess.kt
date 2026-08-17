package app.proxy.client.runtime

import android.content.Context
import android.os.Build
import java.io.DataInputStream
import java.io.DataOutputStream
import java.util.UUID
import java.util.concurrent.TimeUnit

/**
 * 管理单个 Root `app_process` 数据面及匿名管道协议。
 * 类实例拥有进程和两个流；任一超时或协议错误都会终止精确会话，禁止遗留持有透明端口的 Root 进程。
 */
class RootCompanionProcess private constructor(
    private val process: Process,
    private val sessionId: String,
) {
    private val input = DataInputStream(process.inputStream.buffered())
    private val output = DataOutputStream(process.outputStream.buffered())
    private var stopped = false

    /** 查询五项流量统计；伴随进程退出、超时或结构不完整时抛错，由服务统一拆链。 */
    @Synchronized
    fun stats(): LongArray {
        checkRunning()
        output.writeByte(commandStats)
        output.flush()
        readSuccess()
        return LongArray(statisticsFieldCount) { input.readLong() }
    }

    /** 原子替换 Root Native 规则；失败时旧规则继续生效，异常交由服务停止整个数据面。 */
    @Synchronized
    fun updateRules(routingText: String) {
        checkRunning()
        output.writeByte(commandUpdateRules)
        output.writeUtf8Frame(routingText, maximumRulesBytes)
        output.flush()
        readSuccess()
    }

    /** 请求同步停止并等待 Root 进程退出；协议失败时强制按会话标识回收，不留下透明监听器。 */
    @Synchronized
    fun stop() {
        if (stopped) return
        stopped = true
        val gracefulFailure = runCatching {
            if (process.isAlive) {
                output.writeByte(commandStop)
                output.flush()
                readSuccess()
            }
        }.exceptionOrNull()
        if (!process.waitFor(stopTimeoutSeconds, TimeUnit.SECONDS)) terminateSession(sessionId)
        if (process.isAlive) error("Root 伴随进程停止超时")
        gracefulFailure?.let { throw it }
    }

    /** 校验进程仍存活；退出时仅返回固定诊断，不把 stderr、命令行或连接资料带入 UI。 */
    private fun checkRunning() {
        check(!stopped && process.isAlive) { "Root 数据面进程已经退出" }
    }

    /** 在有界等待内读取状态帧；超时会强制回收会话，防止服务线程永久阻塞。 */
    private fun readSuccess() {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(operationTimeoutSeconds)
        while (input.available() == 0 && process.isAlive && System.nanoTime() < deadline) Thread.sleep(pollIntervalMillis)
        if (input.available() == 0) {
            terminateSession(sessionId)
            error("Root 数据面响应超时")
        }
        if (input.readUnsignedByte() != statusSuccess) error(input.readUTF())
    }

    companion object {
        /**
         * 从 APK 当前安装路径启动 Root 数据面并通过匿名管道发送配置和规则。
         * 启动帧确认前不会返回，任何失败都会按随机会话标识清理刚创建的进程。
         */
        fun start(context: Context, configurationText: String, routingText: String): RootCompanionProcess {
            val sessionId = UUID.randomUUID().toString()
            val applicationInfo = context.applicationInfo
            val primaryAbi = Build.SUPPORTED_ABIS.firstOrNull() ?: error("设备没有可用 ABI")
            // 发布 APK 使用 extractNativeLibs=false，库不会出现在 nativeLibraryDir；Android linker 支持从已对齐 APK
            // 条目直接加载。该路径不含连接资料，也避免把 SO 复制到可被清理的运行目录。
            val nativeLibrary = "${applicationInfo.sourceDir}!/lib/$primaryAbi/libroutesocks.so"
            val command = buildCommand(applicationInfo.sourceDir, nativeLibrary, sessionId)
            val process = ProcessBuilder("su", "-c", command).start()
            discardErrors(process)
            val companion = RootCompanionProcess(process, sessionId)
            return runCatching {
                companion.output.writeInt(protocolMagic)
                companion.output.writeUtf8Frame(configurationText, maximumConfigurationBytes)
                companion.output.writeUtf8Frame(routingText, maximumRulesBytes)
                companion.output.flush()
                companion.readSuccess()
                companion
            }.getOrElse { failure ->
                terminateSession(sessionId)
                throw failure
            }
        }

        /** 构造只含安装路径和随机会话标识的 shell 命令；所有连接资料均由 stdin 传输。 */
        internal fun buildCommand(apkPath: String, nativeLibrary: String, sessionId: String): String =
            "exec env CLASSPATH=${shellQuote(apkPath)} app_process /system/bin " +
                "app.proxy.client.runtime.RootCompanionMain ${shellQuote(nativeLibrary)} ${shellQuote(sessionId)}"

        /** 写入有界 UTF-8 帧；配置超限在进入 Root 进程前失败。 */
        private fun DataOutputStream.writeUtf8Frame(value: String, maximumBytes: Int) {
            val bytes = value.toByteArray(Charsets.UTF_8)
            require(bytes.size in 1..maximumBytes) { "Root 数据面输入长度无效" }
            writeInt(bytes.size)
            write(bytes)
            bytes.fill(0)
        }

        /** 持续排空系统错误流但不保留内容，避免 app_process 诊断阻塞且不让路径进入应用状态。 */
        private fun discardErrors(process: Process) {
            Thread({ process.errorStream.use { stream -> while (stream.read() >= 0) {} } }, "root-companion-stderr").apply {
                isDaemon = true
                start()
            }
        }

        /** 仅终止带随机会话标识的伴随进程；命令不含节点、规则或凭据。 */
        private fun terminateSession(sessionId: String) {
            require(sessionId.matches(Regex("[0-9a-f-]{36}"))) { "Root 伴随进程会话标识无效" }
            val pattern = "app.proxy.client.runtime.RootCompanionMain .* $sessionId$"
            runCatching {
                ProcessBuilder("su", "-c", "pkill -f ${shellQuote(pattern)}").start().waitFor(3, TimeUnit.SECONDS)
            }
        }

        /** 使用 POSIX 单引号规则保护 APK 路径和会话标识，拒绝 shell 元字符改变命令边界。 */
        private fun shellQuote(value: String): String = "'${value.replace("'", "'\\''")}'"

        private const val protocolMagic = 0x5350524B
        private const val maximumConfigurationBytes = 16 * 1024
        private const val maximumRulesBytes = 1024 * 1024
        private const val statusSuccess = 0
        private const val commandStats = 1
        private const val commandUpdateRules = 2
        private const val commandStop = 3
        private const val statisticsFieldCount = 5
        private const val operationTimeoutSeconds = 10L
        private const val stopTimeoutSeconds = 5L
        private const val pollIntervalMillis = 10L
    }
}
