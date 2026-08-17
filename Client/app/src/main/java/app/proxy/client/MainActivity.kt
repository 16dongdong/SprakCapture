package app.proxy.client

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import app.proxy.client.ui.ClientApplication
import app.proxy.client.ui.theme.ClientTheme

/**
 * 承载客户端唯一的 Android Activity，并把窗口生命周期交给 Compose 根节点。
 *
 * 运行上下文：系统从启动器创建 Activity 后先启用边到边绘制，再装载主题与应用导航根节点。
 * 本层不持有代理进程或 VPN 生命周期，避免界面重建导致底层服务被重复启动。创建失败由 Android
 * Activity 生命周期直接报告，不在入口层吞掉异常。
 */
class MainActivity : ComponentActivity() {
    /**
     * 初始化窗口与 Compose 内容。
     *
     * `savedInstanceState` 由系统提供并交给父类恢复窗口状态；当前入口没有业务失败返回值，任何
     * 初始化异常保持原始堆栈上抛，便于在开发阶段定位工程或主题配置错误。
     */
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            ClientTheme {
                ClientApplication()
            }
        }
    }
}
