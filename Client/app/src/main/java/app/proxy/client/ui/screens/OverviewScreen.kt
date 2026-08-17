package app.proxy.client.ui.screens

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.ArrowDownward
import androidx.compose.material.icons.rounded.ArrowUpward
import androidx.compose.material.icons.rounded.PowerSettingsNew
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.proxy.client.domain.ConnectionPhase
import app.proxy.client.domain.ProxyMode
import app.proxy.client.domain.userVisibleProxyError
import app.proxy.client.ui.ClientUiState
import app.proxy.client.ui.components.GlassCard
import app.proxy.client.ui.components.formatBytes

/**
 * 展示 `uiState` 中的连接状态与实时流量，并把用户启停操作交给对应回调。
 * 页面不直接执行系统 I/O；启动失败由 ViewModel 更新为失败状态并展示精确原因。
 */
@Composable
fun OverviewScreen(uiState: ClientUiState, onConnect: () -> Unit, onDisconnect: () -> Unit) {
    val running = uiState.runtime.phase == ConnectionPhase.RUNNING
    val busy = uiState.runtime.phase in setOf(ConnectionPhase.STARTING, ConnectionPhase.STOPPING)
    val statusColor by animateColorAsState(
        if (running) Color(0xFF34C759) else MaterialTheme.colorScheme.primary,
        label = "连接状态色",
    )
    val orbScale by animateFloatAsState(if (running) 1.08f else 1f, label = "连接状态缩放")
    Column(
        modifier = Modifier.fillMaxSize().padding(horizontal = 20.dp, vertical = 18.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("连接", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
        Text(
            text = modeLabel(uiState),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(36.dp))
        Box(
            modifier = Modifier
                .size(172.dp)
                .scale(orbScale)
                .background(statusColor.copy(alpha = 0.12f), CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            Button(
                modifier = Modifier.size(132.dp),
                shape = CircleShape,
                enabled = !busy,
                colors = ButtonDefaults.buttonColors(containerColor = statusColor),
                onClick = if (running) onDisconnect else onConnect,
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Icon(Icons.Rounded.PowerSettingsNew, contentDescription = null, modifier = Modifier.size(34.dp))
                    Spacer(Modifier.height(8.dp))
                    Text(statusLabel(uiState.runtime.phase), fontWeight = FontWeight.SemiBold)
                }
            }
        }
        Spacer(Modifier.height(30.dp))
        GlassCard(Modifier.fillMaxWidth()) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(vertical = 22.dp),
                horizontalArrangement = Arrangement.SpaceEvenly,
            ) {
                TrafficMetric(
                    title = "实时上行",
                    value = formatBytes(uiState.runtime.uploadBytesPerSecond, true),
                    icon = { Icon(Icons.Rounded.ArrowUpward, null, tint = Color(0xFF0A84FF)) },
                )
                TrafficMetric(
                    title = "实时下行",
                    value = formatBytes(uiState.runtime.downloadBytesPerSecond, true),
                    icon = { Icon(Icons.Rounded.ArrowDownward, null, tint = Color(0xFF30B0C7)) },
                )
            }
        }
        uiState.runtime.error?.let { error ->
            Spacer(Modifier.height(16.dp))
            GlassCard(Modifier.fillMaxWidth()) {
                Text(
                    text = userVisibleProxyError(error, "代理数据面运行失败"),
                    modifier = Modifier.padding(16.dp),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
        uiState.runtime.diagnostic?.let { diagnostic ->
            Spacer(Modifier.height(12.dp))
            GlassCard(Modifier.fillMaxWidth()) {
                Text(
                    text = userVisibleProxyError(diagnostic, "云规则更新失败，已继续使用上次有效规则"),
                    modifier = Modifier.padding(16.dp),
                    color = MaterialTheme.colorScheme.tertiary,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
    }
}

/** 根据数据面生成不包含节点资料的精确副标题；Root 明确展示 IPv4 TCP/UDP 透明转发边界。 */
private fun modeLabel(uiState: ClientUiState): String {
    return if (uiState.settings.mode == ProxyMode.ROOT) "Root IPv4 TCP/UDP 透明代理 · 云规则" else "VPN 双栈代理 · 云规则"
}

/** 显示单个流量指标；图标由调用方提供以保持上下行颜色语义。 */
@Composable
private fun TrafficMetric(title: String, value: String, icon: @Composable () -> Unit) {
    Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        icon()
        Text(title, color = MaterialTheme.colorScheme.onSurfaceVariant, fontSize = 12.sp)
        Text(value, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center)
    }
}

/** 把生命周期枚举转换为按钮短文案，失败态允许用户直接重试。 */
private fun statusLabel(phase: ConnectionPhase): String = when (phase) {
    ConnectionPhase.STOPPED -> "连接"
    ConnectionPhase.STARTING -> "连接中"
    ConnectionPhase.RUNNING -> "断开"
    ConnectionPhase.STOPPING -> "断开中"
    ConnectionPhase.FAILED -> "重试"
}
