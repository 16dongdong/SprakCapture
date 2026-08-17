package app.proxy.client.service

import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import app.proxy.client.routing.PackageUidScope
import app.proxy.client.routing.RoutingRuleDocument

/** 保存一次经过 shared UID 完整性校验的系统包投影；集合变化要求重建系统捕获边界。 */
internal data class PackageScopeSnapshot(val selectedUids: Set<Int>)

/**
 * 在应用进程内把服务端包名解析为当前 Android 用户的 UID 投影。
 * 查询结果只代表调用瞬间；服务必须配合包广播重新解析，不能跨安装、卸载或更新长期缓存。
 */
internal class PackageScopeResolver(private val context: Context) {
    /**
     * 解析当前规则的 UID 集合并验证客户端自身排除边界。
     * 包不存在、shared UID 未完整列出或客户端 UID 不独占时抛出精确异常，调用方必须停止数据面。
     */
    fun resolve(rules: RoutingRuleDocument): PackageScopeSnapshot {
        if (rules.hasGlobalRules) {
            PackageUidScope.requireExclusiveOwner(
                applicationPackage = context.packageName,
                applicationUid = context.applicationInfo.uid,
                packagesForUid = ::packagesForUid,
            )
        }
        val selectedUids = PackageUidScope.resolve(
            rules.proxyPackages,
            uidForPackage = ::applicationUid,
            packagesForUid = ::packagesForUid,
        )
        require(context.applicationInfo.uid !in selectedUids) { "服务端规则不能代理客户端自身或共享 UID 应用" }
        return PackageScopeSnapshot(selectedUids)
    }

    /** 把服务端包名转换为当前用户空间 UID；包不存在表示规则范围已经失效。 */
    private fun applicationUid(packageName: String): Int = try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.packageManager.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0)).uid
        } else {
            @Suppress("DEPRECATION")
            context.packageManager.getApplicationInfo(packageName, 0).uid
        }
    } catch (failure: PackageManager.NameNotFoundException) {
        throw IllegalArgumentException("服务端规则中的应用不存在：$packageName", failure)
    }

    /** 查询 UID 当前拥有的全部包；空集合由共享 UID 校验器拒绝，避免卸载竞态扩大捕获范围。 */
    private fun packagesForUid(uid: Int): Set<String> =
        context.packageManager.getPackagesForUid(uid)?.toSet().orEmpty()
}
