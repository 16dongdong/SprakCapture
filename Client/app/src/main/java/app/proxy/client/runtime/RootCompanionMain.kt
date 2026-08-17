package app.proxy.client.runtime

import app.proxy.client.service.buildRootCleanupCommand
import java.io.DataInputStream
import java.io.DataOutputStream
import java.util.concurrent.TimeUnit
import kotlin.system.exitProcess

/**
 * 作为 `su` 启动的最小 Root 数据面入口，持有透明 socket 所需的内核能力。
 * 配置、规则和控制命令只走继承的匿名管道；命令行仅包含无秘密的 Native 库路径与会话标识。
 */
object RootCompanionMain {
    /**
     * 校验启动参数和匿名管道帧后启动 Native；协议损坏、Native 失败或输入结束都会停止数据面并退出非零状态。
     * `arguments[0]` 是 APK 私有目录中的 Native 绝对路径，`arguments[1]` 仅用于宿主进程精确回收。
     */
    @JvmStatic
    fun main(arguments: Array<String>) {
        if (arguments.size != expectedArgumentCount) return
        System.setProperty(NativeRuntime.rootCompanionLibraryProperty, arguments[0])
        val input = DataInputStream(System.`in`.buffered())
        val output = DataOutputStream(System.out.buffered())
        var started = false
        try {
            check(input.readInt() == protocolMagic) { "Root 伴随进程启动协议无效" }
            val configuration = input.readBoundedUtf8(maximumConfigurationBytes)
            val rules = input.readBoundedUtf8(maximumRulesBytes)
            NativeRuntime.start(configuration, rules, rootMode = true)
            started = true
            output.writeSuccess()
            runCommandLoop(input, output)
        } catch (failure: Throwable) {
            output.writeFailure(failure)
        } finally {
            if (started) runCatching(NativeRuntime::stop)
            cleanupTransparentRules()
        }
        // app_process 不会替应用入口自动结束由 Native 创建过线程的 VM；协议生命周期完成后显式退出，
        // 确保热切换和停止不会遗留空壳 Root 进程。
        exitProcess(0)
    }

    /**
     * 伴随进程把控制管道 EOF 视为宿主应用已退出，并独立清除全部自有 iptables 链。
     * 该清理不依赖 Android Service 回调，因此系统杀进程、划掉任务或崩溃后也不会遗留影响全机网络的规则。
     */
    private fun cleanupTransparentRules() {
        val process = ProcessBuilder("sh", "-c", buildRootCleanupCommand()).redirectErrorStream(true).start()
        val outputDrainer = Thread(
            { process.inputStream.use { stream -> while (stream.read() >= 0) {} } },
            "root-cleanup-output",
        ).apply { isDaemon = true; start() }
        if (!process.waitFor(cleanupTimeoutSeconds, TimeUnit.SECONDS)) {
            process.destroyForcibly()
            process.waitFor()
        }
        outputDrainer.join(outputJoinTimeoutMillis)
    }

    /**
     * 串行处理统计、规则替换和停止命令；未知命令立即失败并结束进程，防止两端协议失步后继续处理秘密资料。
     */
    private fun runCommandLoop(input: DataInputStream, output: DataOutputStream) {
        while (true) {
            when (input.readUnsignedByte()) {
                commandStats -> {
                    val statistics = NativeRuntime.stats()
                    output.writeByte(statusSuccess)
                    statistics.forEach(output::writeLong)
                    output.flush()
                }
                commandUpdateRules -> {
                    NativeRuntime.updateRules(input.readBoundedUtf8(maximumRulesBytes))
                    output.writeSuccess()
                }
                commandStop -> {
                    NativeRuntime.stop()
                    output.writeSuccess()
                    return
                }
                else -> error("Root 伴随进程收到未知命令")
            }
        }
    }

    /** 按固定上限读取 UTF-8 帧；长度异常或编码无效时抛错并终止伴随进程。 */
    private fun DataInputStream.readBoundedUtf8(maximumBytes: Int): String {
        val length = readInt()
        require(length in 1..maximumBytes) { "Root 伴随进程输入长度无效" }
        val bytes = ByteArray(length)
        readFully(bytes)
        return bytes.toString(Charsets.UTF_8).also { bytes.fill(0) }
    }

    /** 写入无正文成功帧并立即刷新，调用方只有收到该帧后才允许安装透明链。 */
    private fun DataOutputStream.writeSuccess() {
        writeByte(statusSuccess)
        flush()
    }

    /** 写入固定、脱敏且有界的失败帧；节点、规则正文与凭据不会进入桌面或移动端诊断。 */
    private fun DataOutputStream.writeFailure(failure: Throwable) {
        runCatching {
            writeByte(statusFailure)
            writeUTF(failure.message?.take(maximumFailureCharacters) ?: "Root 数据面运行失败")
            flush()
        }
    }

    private const val expectedArgumentCount = 2
    private const val protocolMagic = 0x5350524B
    private const val maximumConfigurationBytes = 16 * 1024
    private const val maximumRulesBytes = 1024 * 1024
    private const val maximumFailureCharacters = 256
    private const val statusSuccess = 0
    private const val statusFailure = 1
    private const val commandStats = 1
    private const val commandUpdateRules = 2
    private const val commandStop = 3
    private const val cleanupTimeoutSeconds = 15L
    private const val outputJoinTimeoutMillis = 1_000L
}
