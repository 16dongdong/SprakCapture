package app.proxy.client.ui

import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Home
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.viewmodel.compose.viewModel
import app.proxy.client.domain.ProxyMode
import app.proxy.client.ui.components.AuroraBackground
import app.proxy.client.ui.screens.OverviewScreen
import app.proxy.client.ui.screens.SettingsScreen

/**
 * 组织连接/设置导航、系统授权和一次性提示。
 * `clientViewModel` 持有跨页面代理状态；授权拒绝或业务失败由状态消息展示，本组合函数不伪造成功结果。
 */
@Composable
fun ClientApplication(clientViewModel: ClientViewModel = viewModel()) {
    val context = LocalContext.current
    val uiState by clientViewModel.uiState.collectAsState()
    var destination by rememberSaveable { mutableStateOf(ClientDestination.OVERVIEW) }
    val snackbarHostState = remember { SnackbarHostState() }
    var vpnPermissionContinuation by remember { mutableStateOf<(() -> Unit)?>(null) }
    val vpnPermissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val continuation = vpnPermissionContinuation
        vpnPermissionContinuation = null
        if (result.resultCode == Activity.RESULT_OK) continuation?.invoke()
    }
    val notificationPermissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { }
    LaunchedEffect(Unit) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }
    LaunchedEffect(uiState.message) {
        uiState.message?.let {
            snackbarHostState.showSnackbar(it)
            clientViewModel.consumeMessage()
        }
    }
    AuroraBackground {
        Scaffold(
            containerColor = Color.Transparent,
            contentColor = MaterialTheme.colorScheme.onBackground,
            snackbarHost = { SnackbarHost(snackbarHostState) },
            bottomBar = {
                NavigationBar(containerColor = MaterialTheme.colorScheme.surface.copy(alpha = 0.88f)) {
                    ClientDestination.entries.forEach { item ->
                        NavigationBarItem(
                            selected = destination == item,
                            onClick = { destination = item },
                            icon = { Icon(item.icon, contentDescription = item.label) },
                            label = { Text(item.label) },
                        )
                    }
                }
            },
        ) { contentPadding ->
            Box(Modifier.fillMaxSize().padding(contentPadding)) {
                when (destination) {
                    ClientDestination.OVERVIEW -> OverviewScreen(
                        uiState = uiState,
                        onConnect = {
                            if (uiState.settings.mode == ProxyMode.VPN) {
                                val permissionIntent = clientViewModel.vpnPermissionIntent()
                                if (permissionIntent == null) clientViewModel.startProxy()
                                else {
                                    vpnPermissionContinuation = clientViewModel::startProxy
                                    vpnPermissionLauncher.launch(permissionIntent)
                                }
                            } else {
                                clientViewModel.startProxy()
                            }
                        },
                        onDisconnect = clientViewModel::stopProxy,
                    )
                    ClientDestination.SETTINGS -> SettingsScreen(
                        uiState = uiState,
                        onModeChange = { mode ->
                            val switchingFromRoot = mode == ProxyMode.VPN &&
                                uiState.runtime.phase == app.proxy.client.domain.ConnectionPhase.RUNNING
                            val permissionIntent = if (switchingFromRoot) clientViewModel.vpnPermissionIntent() else null
                            if (permissionIntent == null) {
                                clientViewModel.setMode(mode)
                            } else {
                                vpnPermissionContinuation = { clientViewModel.setMode(mode) }
                                vpnPermissionLauncher.launch(permissionIntent)
                            }
                        },
                        onCertificateTrustChange = clientViewModel::setCertificateTrustEnabled,
                        onRootCapabilityRefresh = clientViewModel::refreshRootCapability,
                    )
                }
            }
        }
    }
}

/** 定义底部导航的固定页面集合；代理模式属于设置开关，不额外创建冗余页面。 */
private enum class ClientDestination(val label: String, val icon: ImageVector) {
    OVERVIEW("连接", Icons.Rounded.Home),
    SETTINGS("设置", Icons.Rounded.Settings),
}
