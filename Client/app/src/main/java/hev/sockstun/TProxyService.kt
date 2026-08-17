package hev.sockstun

import androidx.annotation.Keep

/**
 * 保留静态链接进 `libroutesocks.so` 的 HEV 稳定 JNI 类名。
 *
 * 该名称由 vendored HEV 在 `JNI_OnLoad` 中固定注册，改名会导致库加载失败；业务服务仅通过此最小
 * 桥接层传入配置路径和系统 TUN 文件描述符，不复用上游界面或偏好实现。
 */
@Keep
class TProxyService {
    companion object {
        /**
         * 启动 Native tun2socks 工作线程并等待初始化握手。
         * 返回 null 表示已经进入稳定运行点，非空中文字符串是配置、线程或 TUN 初始化的精确失败原因。
         */
        @JvmStatic
        external fun TProxyStartService(configPath: String, tunFileDescriptor: Int): String?

        /** 同步停止 Native 工作线程并回收会话；仅在本进程确认启动过 Native 数据面后调用。 */
        @JvmStatic
        external fun TProxyStopService()

        /** 返回 null 表示工作线程仍在运行，非空中文字符串表示初始化后发生的确定退出原因。 */
        @JvmStatic
        external fun TProxyRuntimeError(): String?

        init {
            // HEV 与统一规则核心共享同一个 SO，重复引用由系统加载器按进程幂等处理，禁止恢复旧独立库。
            System.loadLibrary("routesocks")
        }
    }
}
