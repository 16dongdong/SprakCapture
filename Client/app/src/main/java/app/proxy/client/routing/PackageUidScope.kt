package app.proxy.client.routing

/**
 * 把 proxy_app 包集合投影为 Android UID，并验证 sharedUserId 不会扩大应用规则范围。
 * Android VPN 与 owner iptables 都按 UID 工作；同 UID 的未列包无法在数据面再次区分，因此必须在启动前拒绝。
 */
object PackageUidScope {
    /**
     * 确认必须从全局捕获中排除的客户端 UID 只归当前包所有。
     * VPN 与 owner iptables 都只能按 UID 排除 Native 出口；若同 UID 还有其他包，继续运行会让这些包静默直连。
     */
    fun requireExclusiveOwner(
        applicationPackage: String,
        applicationUid: Int,
        packagesForUid: (Int) -> Set<String>,
    ) {
        val owningPackages = packagesForUid(applicationUid)
        require(owningPackages == setOf(applicationPackage)) {
            val unexpectedPackages = (owningPackages - applicationPackage).sorted().joinToString()
            "客户端 UID $applicationUid 还属于其他应用，无法安全排除全局流量：$unexpectedPackages"
        }
    }

    /**
     * 解析全部选中包并核对每个 UID 的完整包集合。
     * `uidForPackage` 或 `packagesForUid` 查询失败直接向上抛出；发现未列包时返回精确 UID 与包名错误。
     */
    fun resolve(
        selectedPackages: Set<String>,
        uidForPackage: (String) -> Int,
        packagesForUid: (Int) -> Set<String>,
    ): Set<Int> {
        val selectedUids = selectedPackages.map(uidForPackage).toSortedSet()
        selectedUids.forEach { uid ->
            val owningPackages = packagesForUid(uid)
            require(owningPackages.isNotEmpty()) { "系统没有返回 UID $uid 的应用列表" }
            val unselectedPackages = owningPackages - selectedPackages
            require(unselectedPackages.isEmpty()) {
                "proxy_app 必须包含共享 UID $uid 的全部应用，缺少：${unselectedPackages.sorted().joinToString()}"
            }
        }
        return selectedUids
    }
}
