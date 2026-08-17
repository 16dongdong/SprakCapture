package app.proxy.client.ui.components

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.LocalContentColor
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import java.util.Locale

/** 绘制轻量动态色斑和半透明底色，为所有页面提供一致的 iOS 式空间层级。 */
@Composable
fun AuroraBackground(content: @Composable BoxScope.() -> Unit) {
    val transition = rememberInfiniteTransition(label = "背景流动")
    val drift by transition.animateFloat(
        initialValue = -0.08f,
        targetValue = 0.08f,
        animationSpec = infiniteRepeatable(tween(8_000), RepeatMode.Reverse),
        label = "色斑位移",
    )
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(
                Brush.verticalGradient(
                    listOf(
                        MaterialTheme.colorScheme.background,
                        MaterialTheme.colorScheme.primary.copy(alpha = 0.06f),
                        MaterialTheme.colorScheme.background,
                    ),
                ),
            ),
    ) {
        Canvas(Modifier.fillMaxSize().blur(42.dp)) {
            drawCircle(
                color = Color(0xFF0A84FF).copy(alpha = 0.16f),
                radius = size.minDimension * 0.38f,
                center = Offset(size.width * (0.82f + drift), size.height * 0.18f),
            )
            drawCircle(
                color = Color(0xFF30D5C8).copy(alpha = 0.12f),
                radius = size.minDimension * 0.32f,
                center = Offset(size.width * (0.12f - drift), size.height * 0.72f),
            )
        }
        content()
    }
}

/** 提供统一圆角、描边和透明表面的毛玻璃卡片；内容保持清晰，不对前景本身做模糊。 */
@Composable
fun GlassCard(modifier: Modifier = Modifier, content: @Composable BoxScope.() -> Unit) {
    val shape = RoundedCornerShape(24.dp)
    Box(
        modifier = modifier
            .clip(shape)
            .background(MaterialTheme.colorScheme.surface.copy(alpha = 0.78f))
            .border(1.dp, MaterialTheme.colorScheme.onSurface.copy(alpha = 0.08f), shape),
    ) {
        CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onSurface) {
            content()
        }
    }
}

/** 把字节值格式化为紧凑的二进制单位；负数按零处理以避免底层异常污染 UI。 */
fun formatBytes(bytes: Long, perSecond: Boolean = false): String {
    val value = bytes.coerceAtLeast(0)
    val units = arrayOf("B", "KB", "MB", "GB", "TB")
    var scaled = value.toDouble()
    var unitIndex = 0
    while (scaled >= 1024.0 && unitIndex < units.lastIndex) {
        scaled /= 1024.0
        unitIndex++
    }
    val number = if (unitIndex == 0) value.toString() else String.format(Locale.ROOT, "%.1f", scaled)
    return "$number ${units[unitIndex]}${if (perSecond) "/s" else ""}"
}
