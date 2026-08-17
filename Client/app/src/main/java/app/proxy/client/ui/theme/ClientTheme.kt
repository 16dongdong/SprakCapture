package app.proxy.client.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val lightColors = lightColorScheme(
    primary = Color(0xFF0A84FF),
    onPrimary = Color.White,
    background = Color(0xFFF2F2F7),
    surface = Color(0xFFFFFFFF),
    onBackground = Color(0xFF1C1C1E),
    onSurface = Color(0xFF1C1C1E),
    onSurfaceVariant = Color(0xFF636366),
)

private val darkColors = darkColorScheme(
    primary = Color(0xFF0A84FF),
    onPrimary = Color.White,
    background = Color(0xFF000000),
    surface = Color(0xFF1C1C1E),
    onBackground = Color(0xFFF2F2F7),
    onSurface = Color(0xFFF2F2F7),
    onSurfaceVariant = Color(0xFFAEAEB2),
    surfaceVariant = Color(0xFF2C2C2E),
)

/**
 * 统一应用的明暗配色与排版入口，为后续 iOS 风格层级和毛玻璃组件提供稳定色彩语义。
 *
 * 运行上下文：根 Activity 包裹全部 Compose 页面时调用；`darkTheme` 默认跟随系统且允许预览覆盖，
 * `content` 是待应用主题的界面树。主题构建没有可恢复失败分支，配置错误由 Compose 直接抛出。
 */
@Composable
fun ClientTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    MaterialTheme(
        colorScheme = if (darkTheme) darkColors else lightColors,
        content = content,
    )
}
