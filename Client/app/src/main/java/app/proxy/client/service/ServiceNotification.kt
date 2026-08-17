package app.proxy.client.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import app.proxy.client.MainActivity
import app.proxy.client.R
import app.proxy.client.domain.ProxyMode

/** 创建两种数据面共用的前台通知，确保后台运行状态具有唯一且可返回的系统入口。 */
object ServiceNotification {
    const val NOTIFICATION_ID = 1207
    private const val CHANNEL_ID = "proxyRuntime"

    /** 初始化低打扰通知渠道；重复调用由系统按同一渠道标识幂等处理。 */
    fun ensureChannel(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "代理运行状态", NotificationManager.IMPORTANCE_LOW),
        )
    }

    /** 构造当前模式的常驻通知；标题与正文只描述运行状态，不接收或显示静态连接资料。 */
    fun create(context: Context, mode: ProxyMode): Notification {
        val launchIntent = Intent(context, MainActivity::class.java).addFlags(
            Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP,
        )
        val pendingIntent = PendingIntent.getActivity(
            context,
            0,
            launchIntent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val title = if (mode == ProxyMode.ROOT) "Root 透明代理运行中" else "VPN 全局代理运行中"
        return NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.app_icon)
            .setContentTitle(title)
            .setContentText("代理服务正在运行")
            .setContentIntent(pendingIntent)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .build()
    }
}
