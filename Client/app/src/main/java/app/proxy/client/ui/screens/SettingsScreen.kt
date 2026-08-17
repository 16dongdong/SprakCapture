package app.proxy.client.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Security
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import app.proxy.client.domain.ConnectionPhase
import app.proxy.client.domain.ProxyMode
import app.proxy.client.ui.ClientUiState
import app.proxy.client.ui.components.GlassCard

/**
 * 展示 `uiState` 中的数据面能力，并通过回调提交模式切换或 Root 复检操作。
 * 静态连接资料不进入 UI 状态，应用范围由服务端规则控制，因此本页只保留数据面能力开关。
 */
@Composable
fun SettingsScreen(
    uiState: ClientUiState,
    onModeChange: (ProxyMode) -> Unit,
    onCertificateTrustChange: (Boolean) -> Unit,
    onRootCapabilityRefresh: () -> Unit,
) {
    val editable = uiState.runtime.phase !in setOf(ConnectionPhase.STARTING, ConnectionPhase.STOPPING)
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 20.dp, vertical = 18.dp),
    ) {
        Text("设置", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
        Text("数据面与设备能力", color = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.height(18.dp))
        GlassCard(Modifier.fillMaxWidth()) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(18.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.Rounded.Security, null, tint = MaterialTheme.colorScheme.primary)
                Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
                    Text("Root 透明代理", fontWeight = FontWeight.SemiBold)
                    Text(
                        if (uiState.rootInspectionFinished && uiState.rootAvailable) "设备已授权 Root，可使用 iptables 模式"
                        else if (uiState.rootInspectionFinished) "未获得 Root，继续使用标准 VPN 模式"
                        else "正在检查设备能力",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Text(
                        "Root 通过特权透明数据面转发 IPv4 TCP、UDP 和指定 DNS；IPv6 当前明确阻断防止直连泄漏。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(
                    checked = uiState.settings.mode == ProxyMode.ROOT,
                    enabled = editable && uiState.rootAvailable,
                    onCheckedChange = { enabled -> onModeChange(if (enabled) ProxyMode.ROOT else ProxyMode.VPN) },
                )
            }
            if (uiState.rootInspectionFinished && !uiState.rootAvailable) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(end = 10.dp, bottom = 6.dp),
                    horizontalArrangement = Arrangement.End,
                ) {
                    TextButton(onClick = onRootCapabilityRefresh) { Text("重新检查 Root") }
                }
            }
        }
        Spacer(Modifier.height(14.dp))
        GlassCard(Modifier.fillMaxWidth()) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(18.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Icon(Icons.Rounded.Security, null, tint = MaterialTheme.colorScheme.primary)
                Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
                    Text("证书信任", fontWeight = FontWeight.SemiBold)
                    Text(
                        when {
                            uiState.certificateTrustUpdating -> "正在同步并应用当前抓包根证书"
                            !uiState.rootAvailable -> "授予 Root 后可开启，客户端无需额外安装步骤"
                            uiState.settings.certificateTrustEnabled -> "代理启动后自动鉴权下载、安装并保持证书同步"
                            else -> "开启后由代理通道安全获取并信任当前抓包根证书"
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(
                    checked = uiState.settings.certificateTrustEnabled,
                    enabled = editable && uiState.rootAvailable && !uiState.certificateTrustUpdating,
                    onCheckedChange = onCertificateTrustChange,
                )
            }
        }
        Spacer(Modifier.height(24.dp))
    }
}
