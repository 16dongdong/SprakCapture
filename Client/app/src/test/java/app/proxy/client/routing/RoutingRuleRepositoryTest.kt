package app.proxy.client.routing

import app.proxy.client.domain.AccountCredentials
import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.domain.EmbeddedNode
import java.io.File
import java.nio.file.Files
import java.util.ArrayDeque
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证云规则更新是“获取、校验、原子保存”整体事务，坏版本不得中断当前代理。 */
class RoutingRuleRepositoryTest {
    /** 已有有效缓存时，后续无效正文必须保留旧文档和 ETag，并只返回非致命诊断。 */
    @Test
    fun invalidUpdateKeepsLastValidatedRule() {
        val directory = Files.createTempDirectory("routingRepositoryTest").toFile()
        try {
            val responses = ArrayDeque<RoutingFetchResponse>().apply {
                add(RoutingFetchResponse.Content("\"valid-1\"", validRule))
                add(RoutingFetchResponse.Content("\"broken-2\"", "[RoutingRule]\nFINAL,UNKNOWN"))
                add(RoutingFetchResponse.NotModified)
            }
            val requestedEtags = mutableListOf<String?>()
            val repository = RoutingRuleRepository(directory) { _, etag ->
                requestedEtags += etag
                responses.removeFirst()
            }

            val initial = repository.refresh(profile)
            val rejected = repository.refresh(profile)
            val unchanged = repository.refresh(profile)

            assertTrue(initial.changed)
            assertFalse(rejected.changed)
            assertNotNull(rejected.diagnostic)
            assertEquals(initial.document.text, rejected.document.text)
            assertEquals(initial.document.text, unchanged.document.text)
            assertEquals(listOf(null, "\"valid-1\"", "\"valid-1\""), requestedEtags)
        } finally {
            assertTrue(directory.deleteRecursively())
        }
    }

    /**
     * 旧分文件缓存没有共同代次标识，即使两个文件都可解析也不得携带旧 ETag 发起请求。
     */
    @Test
    fun obsoleteSplitCacheIsDeletedInsteadOfMigrated() {
        val directory = Files.createTempDirectory("routingRepositoryLegacyTest").toFile()
        try {
            File(directory, "routing.txt").writeText(validRule, Charsets.UTF_8)
            File(directory, "routing.etag").writeText("\"possibly-mismatched\"", Charsets.UTF_8)
            val requestedEtags = mutableListOf<String?>()
            val repository = RoutingRuleRepository(directory) { _, etag ->
                requestedEtags += etag
                RoutingFetchResponse.Content("\"fresh\"", validRule)
            }

            assertTrue(repository.refresh(profile).changed)
            assertEquals(listOf<String?>(null), requestedEtags)
            assertFalse(File(directory, "routing.txt").exists())
            assertFalse(File(directory, "routing.etag").exists())
            assertTrue(File(directory, "routing.snapshot").isFile)
        } finally {
            assertTrue(directory.deleteRecursively())
        }
    }

    /** 单文件快照被截断后必须整体失效，下次请求不得发送无法证明所属正文的 ETag。 */
    @Test
    fun corruptedSnapshotForcesUnconditionalRefresh() {
        val directory = Files.createTempDirectory("routingRepositoryCorruptionTest").toFile()
        try {
            val requestedEtags = mutableListOf<String?>()
            var responseEtag = "\"first\""
            val repository = RoutingRuleRepository(directory) { _, etag ->
                requestedEtags += etag
                RoutingFetchResponse.Content(responseEtag, validRule)
            }
            repository.refresh(profile)
            val snapshot = File(directory, "routing.snapshot")
            snapshot.writeBytes(snapshot.readBytes().copyOf(7))
            responseEtag = "\"second\""

            assertTrue(repository.refresh(profile).changed)
            assertEquals(listOf(null, null), requestedEtags)
            assertTrue(snapshot.length() > 7)
        } finally {
            assertTrue(directory.deleteRecursively())
        }
    }

    /** ETag 会原样回填 HTTP 请求，可见 ASCII 以外的控制字符必须在快照提交前拒绝。 */
    @Test
    fun responseEtagWithControlCharactersIsRejected() {
        val directory = Files.createTempDirectory("routingRepositoryEtagTest").toFile()
        try {
            val repository = RoutingRuleRepository(directory) { _, _ ->
                RoutingFetchResponse.Content("\"bad\"\t", validRule)
            }

            val failure = runCatching { repository.refresh(profile) }.exceptionOrNull()
            assertTrue(failure?.message?.contains("ETag 无效") == true)
            assertFalse(File(directory, "routing.snapshot").exists())
        } finally {
            assertTrue(directory.deleteRecursively())
        }
    }

    private companion object {
        val profile = EmbeddedClientProfile(
            EmbeddedNode("127.0.0.1", 1080),
            AccountCredentials("user", "pass"),
            "http://127.0.0.1:19090/api/v1/client/routing.txt",
        )
        val validRule = "[RoutingRule]\n[GRoutingRule]\nFINAL,PROXY\n[proxy_app]\n[DNS]\nPRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\n"
    }
}
