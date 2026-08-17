package app.proxy.client.routing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证 Android UID 投影不会让未列出的 sharedUserId 应用误入 RoutingRule。 */
class PackageUidScopeTest {
    /** 全局数据面排除客户端 UID 时，共享该 UID 的其他包会旁路规则，因此必须拒绝启动。 */
    @Test
    fun sharedApplicationUidCannotBeExcludedFromGlobalScope() {
        val failure = runCatching {
            PackageUidScope.requireExclusiveOwner("app.proxy.client", 11000) {
                setOf("app.proxy.client", "com.example.shared")
            }
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("com.example.shared") == true)
    }

    /** 客户端独占自身 UID 时可以安全排除 Native 出口，不会同时放行其他应用。 */
    @Test
    fun exclusiveApplicationUidCanBeExcludedFromGlobalScope() {
        PackageUidScope.requireExclusiveOwner("app.proxy.client", 11000) { setOf("app.proxy.client") }
    }

    /** 只列出共享 UID 的部分包必须拒绝，否则未列包会继承选中应用规则。 */
    @Test
    fun partialSharedUidSelectionIsRejected() {
        val failure = runCatching {
            PackageUidScope.resolve(
                setOf("com.example.alpha"),
                uidForPackage = { 11001 },
                packagesForUid = { setOf("com.example.alpha", "com.example.beta") },
            )
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("com.example.beta") == true)
    }

    /** 显式列出共享 UID 的全部包时只生成一个 UID，避免重复安装同一 owner 规则。 */
    @Test
    fun completeSharedUidSelectionIsAccepted() {
        val packages = setOf("com.example.alpha", "com.example.beta")
        val selectedUids = PackageUidScope.resolve(
            packages,
            uidForPackage = { 11001 },
            packagesForUid = { packages },
        )

        assertEquals(setOf(11001), selectedUids)
    }

    /** 运行中有未选包加入选中 shared UID 时，下一次包事件校验必须精确拒绝而不是沿用旧 UID。 */
    @Test
    fun runtimeSharedUidAdditionIsRejected() {
        var owners = setOf("com.example.alpha")
        val selectedPackages = setOf("com.example.alpha")
        assertEquals(
            setOf(11001),
            PackageUidScope.resolve(selectedPackages, { 11001 }, { owners }),
        )

        owners = setOf("com.example.alpha", "com.example.unselected")
        val failure = runCatching {
            PackageUidScope.resolve(selectedPackages, { 11001 }, { owners })
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("com.example.unselected") == true)
    }

    /** 选中包更新后迁移到新 UID 时，重新投影必须返回新集合供服务完整重建系统捕获边界。 */
    @Test
    fun runtimeSelectedPackageUidChangeIsObserved() {
        var packageUid = 11001
        val selectedPackages = setOf("com.example.alpha")
        val initial = PackageUidScope.resolve(selectedPackages, { packageUid }, { selectedPackages })

        packageUid = 11002
        val updated = PackageUidScope.resolve(selectedPackages, { packageUid }, { selectedPackages })

        assertEquals(setOf(11001), initial)
        assertEquals(setOf(11002), updated)
    }

    /** 客户端运行中新增共享 UID 包会扩大全局旁路，包广播后的独占性校验必须立即失败。 */
    @Test
    fun runtimeApplicationSharedUidChangeIsRejected() {
        var owners = setOf("app.proxy.client")
        PackageUidScope.requireExclusiveOwner("app.proxy.client", 11000) { owners }

        owners = setOf("app.proxy.client", "com.example.shared")
        val failure = runCatching {
            PackageUidScope.requireExclusiveOwner("app.proxy.client", 11000) { owners }
        }.exceptionOrNull()

        assertTrue(failure?.message?.contains("com.example.shared") == true)
    }
}
