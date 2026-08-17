package app.proxy.client.certificate

import app.proxy.client.domain.EmbeddedClientProfile
import app.proxy.client.routing.SocksRoutingClient
import app.proxy.client.runtime.RootAccess
import java.io.ByteArrayOutputStream
import java.util.concurrent.TimeUnit

/**
 * 协调“账号鉴权下载证书”和“Root 系统信任安装”两个边界。
 * 服务私钥永不离开服务端；客户端只处理公开 DER，并在密码学校验成功后才执行固定 Root 脚本。
 */
class RootCertificateTrustManager(
    private val certificateSource: SocksRoutingClient = SocksRoutingClient(),
    private val installer: RootCertificateInstaller = RootCertificateInstaller(),
) {
    /**
     * 经当前安装包的 SOCKS5 账号下载并安装最新根证书。
     * 调用点必须位于代理数据面建立之后；Root 不可用、鉴权失败或安装失败都会使本次代理启动回滚。
     */
    fun synchronize(profile: EmbeddedClientProfile) {
        check(RootAccess.isAvailable()) { "设备未授予 Root 权限" }
        val der = certificateSource.fetchRootCertificate(profile)
        try {
            installer.install(TrustedRootCertificate.parse(der))
        } finally {
            der.fill(0)
        }
    }

    /** 立即撤销本模块安装的系统信任；只清理具备本模块标记的挂载和精确历史模块目录。 */
    fun remove() {
        check(RootAccess.isAvailable()) { "设备未授予 Root 权限" }
        installer.remove()
    }
}

/**
 * 把单一根证书安装为 APatch/Magisk 兼容模块，并同步到 Android 14+ Conscrypt APEX 挂载命名空间。
 * 所有路径均固定且由本模块独占；脚本不接收节点、账号或私钥，失败返回有界中文阶段信息。
 */
class RootCertificateInstaller(
    private val commandRunner: RootCommandRunner = RootCommandRunner(),
) {
    /** 写入模块、迁移精确旧模块并立即应用到当前 zygote；任一步失败均返回失败而不伪造已信任。 */
    fun install(certificate: TrustedRootCertificate) {
        commandRunner.execute(buildInstallScript(certificate))
    }

    /** 卸载当前会话挂载和持久模块；非本模块挂载没有所有权标记时不会被触碰。 */
    fun remove() {
        commandRunner.execute(buildRemoveScript())
    }

    /** 生成固定安装事务；证书哈希已由 X.509 主体计算，PEM 不参与 shell 插值解析。 */
    internal fun buildInstallScript(certificate: TrustedRootCertificate): String {
        require(certificate.subjectHash.matches(Regex("^[0-9a-f]{8}$"))) { "根证书系统索引无效" }
        return """
            set -eu
            MODDIR='$moduleDirectory'
            LEGACY='$legacyModuleDirectory'
            CERT_NAME='${certificate.subjectHash}.0'
            FINGERPRINT='${certificate.fingerprint}'
            LAYOUT_VERSION='$runtimeStoreLayoutVersion'
            APEX_STORE=/apex/com.android.conscrypt/cacerts
            MARKER=.device_ca_trust_owner
            BUSYBOX=/data/adb/ap/bin/busybox
            [ -x "${'$'}BUSYBOX" ] || BUSYBOX=/data/adb/magisk/busybox
            enter_mount() { if [ -x "${'$'}BUSYBOX" ]; then "${'$'}BUSYBOX" nsenter "${'$'}@"; else nsenter "${'$'}@"; fi; }
            rm -rf "${'$'}MODDIR/system/etc/security/cacerts"
            mkdir -p "${'$'}MODDIR/system/etc/security/cacerts"
            cat > "${'$'}MODDIR/module.prop" <<'MODULE_PROP'
            id=device_ca_trust
            name=设备证书信任
            version=1
            versionCode=2
            author=local
            description=同步当前抓包根证书到 Android 系统信任存储
            MODULE_PROP
            cat > "${'$'}MODDIR/system/etc/security/cacerts/${'$'}CERT_NAME" <<'CERTIFICATE_PEM'
            $certificatePlaceholder
            CERTIFICATE_PEM
            printf '%s\n' "${'$'}FINGERPRINT" > "${'$'}MODDIR/.certificateFingerprint"
            cat > "${'$'}MODDIR/service.sh" <<'SERVICE_SCRIPT'
            #!/system/bin/sh
            set -eu
            MODDIR=${'$'}{0%/*}
            APEX_STORE=/apex/com.android.conscrypt/cacerts
            SYSTEM_STORE=/system/etc/security/cacerts
            RUNTIME_STORE="${'$'}MODDIR/runtime-cacerts"
            MARKER=.device_ca_trust_owner
            LAYOUT_VERSION=$runtimeStoreLayoutVersion
            CERT_NAME=${'$'}(basename "${'$'}MODDIR/system/etc/security/cacerts/"*.0)
            BUSYBOX=/data/adb/ap/bin/busybox
            [ -x "${'$'}BUSYBOX" ] || BUSYBOX=/data/adb/magisk/busybox
            enter_mount() { if [ -x "${'$'}BUSYBOX" ]; then "${'$'}BUSYBOX" nsenter "${'$'}@"; else nsenter "${'$'}@"; fi; }
            if [ "${'$'}{1:-boot}" = boot ]; then
              while [ "${'$'}(getprop sys.boot_completed)" != 1 ]; do sleep 1; done
              # 重启后旧挂载命名空间已消失，此时才能安全回收上次轮换保留的目录代际。
              rm -rf "${'$'}MODDIR"/runtime-cacerts.previous.* "${'$'}MODDIR"/runtime-cacerts.next.*
            fi
            EXPECTED_FINGERPRINT=${'$'}(cat "${'$'}MODDIR/.certificateFingerprint")
            INSTALLED_FINGERPRINT=${'$'}(cat "${'$'}RUNTIME_STORE/.certificateFingerprint" 2>/dev/null || true)
            INSTALLED_LAYOUT=${'$'}(cat "${'$'}RUNTIME_STORE/.layoutVersion" 2>/dev/null || true)
            if [ "${'$'}INSTALLED_FINGERPRINT" != "${'$'}EXPECTED_FINGERPRINT" ] || [ "${'$'}INSTALLED_LAYOUT" != "${'$'}LAYOUT_VERSION" ]; then
              NEXT_STORE="${'$'}MODDIR/runtime-cacerts.next.${'$'}${'$'}"
              PREVIOUS_STORE="${'$'}MODDIR/runtime-cacerts.previous.${'$'}${'$'}"
              rm -rf "${'$'}NEXT_STORE"
              mkdir -p "${'$'}NEXT_STORE"
              # 活跃 APEX 目录可能已经被本模块上一代运行目录覆盖；继续复制它会只留下自定义 CA，
              # 从而删除 Android 原有信任根并使正常 HTTPS 统一报证书错误。系统目录由 Root 框架叠加模块文件，
              # 仍保留完整平台根集合，因此必须以它作为新代际的权威基线。
              test -d "${'$'}SYSTEM_STORE"
              cp -a "${'$'}SYSTEM_STORE/." "${'$'}NEXT_STORE/"
              cp "${'$'}MODDIR/system/etc/security/cacerts/"*.0 "${'$'}NEXT_STORE/"
              cp "${'$'}MODDIR/.certificateFingerprint" "${'$'}NEXT_STORE/.certificateFingerprint"
              printf '%s\n' "${'$'}LAYOUT_VERSION" > "${'$'}NEXT_STORE/.layoutVersion"
              : > "${'$'}NEXT_STORE/${'$'}MARKER"
              chown -R root:root "${'$'}NEXT_STORE"
              chmod 0755 "${'$'}NEXT_STORE"
              chmod 0644 "${'$'}NEXT_STORE/"*.0 "${'$'}NEXT_STORE/${'$'}MARKER" "${'$'}NEXT_STORE/.certificateFingerprint" "${'$'}NEXT_STORE/.layoutVersion"
              # APEX 信任目录必须保持 system_file 标签；restorecon 会把 data 分区文件恢复成 adb_data_file，
              # 应用进程即使看见挂载也会因 SELinux 拒绝读取证书。
              chcon -R u:object_r:system_file:s0 "${'$'}NEXT_STORE"
              # 活跃命名空间仍可能引用旧目录 inode；先换路径再叠加新绑定，绝不删除或清空正在读取的目录。
              if [ -d "${'$'}RUNTIME_STORE" ]; then mv "${'$'}RUNTIME_STORE" "${'$'}PREVIOUS_STORE"; fi
              mv "${'$'}NEXT_STORE" "${'$'}RUNTIME_STORE"
            fi
            chcon -R u:object_r:system_file:s0 "${'$'}RUNTIME_STORE"
            USER_ID=${'$'}(am get-current-user)
            USER_STORE="/data/misc/user/${'$'}USER_ID/cacerts-added"
            USER_CERT="${'$'}USER_STORE/${'$'}CERT_NAME"
            USER_NEXT="${'$'}USER_STORE/.${'$'}CERT_NAME.next.${'$'}${'$'}"
            # Chromium 使用 Android KeyChain 的用户证书来源判定本地信任；只挂载 Conscrypt 系统目录时，
            # Chrome Root Store 仍会把动态抓包证书判为未知机构。用户目录采用同一公开 CA 的原子副本，
            # 私钥始终不在设备侧，KEYCHAIN_CHANGED 使已运行浏览器刷新信任缓存。
            mkdir -p "${'$'}USER_STORE"
            chown system:system "${'$'}USER_STORE"
            chmod 0755 "${'$'}USER_STORE"
            if ! cmp -s "${'$'}MODDIR/system/etc/security/cacerts/${'$'}CERT_NAME" "${'$'}USER_CERT"; then
              cp "${'$'}MODDIR/system/etc/security/cacerts/${'$'}CERT_NAME" "${'$'}USER_NEXT"
              chown system:system "${'$'}USER_NEXT"
              chmod 0644 "${'$'}USER_NEXT"
              restorecon -F "${'$'}USER_NEXT"
              mv -f "${'$'}USER_NEXT" "${'$'}USER_CERT"
              restorecon -F "${'$'}USER_CERT"
            fi
            replace_namespace_store() {
              PID=${'$'}1
              [ -r "/proc/${'$'}PID/ns/mnt" ] || return 1
              # 单次 nsenter 内完成识别、卸载、绑定和验证；逐动作启动 nsenter 会在进程较多的设备上超时。
              enter_mount -t "${'$'}PID" -m -- sh -c '
                APEX_STORE=/apex/com.android.conscrypt/cacerts
                RUNTIME_STORE=/data/adb/modules/device_ca_trust/runtime-cacerts
                CERT_NAME='"${'$'}CERT_NAME"'
                LAYOUT_VERSION='"${'$'}LAYOUT_VERSION"'
                [ -d "${'$'}APEX_STORE" ] || exit 1
                if test "${'$'}(cat "${'$'}APEX_STORE/.certificateFingerprint" 2>/dev/null)" = '${certificate.fingerprint}' &&
                  test "${'$'}(cat "${'$'}APEX_STORE/.layoutVersion" 2>/dev/null)" = "${'$'}LAYOUT_VERSION"; then
                  exit 0
                fi
                # 旧版目录已被删除时必须先揭掉悬空挂载；当前模块挂载直接叠加新目录，避免并发 TLS 读取看到空窗。
                while grep -F " ${'$'}APEX_STORE " /proc/self/mountinfo | tail -n 1 |
                  grep -E "(/tproxy_ca/runtime-cacerts|/device_ca_trust/runtime-cacerts//deleted)" >/dev/null 2>&1; do
                  umount "${'$'}APEX_STORE" || exit 1
                done
                mount --bind "${'$'}RUNTIME_STORE" "${'$'}APEX_STORE" || exit 1
                test -f "${'$'}APEX_STORE/${'$'}CERT_NAME"
              '
            }
            SEEN_NAMESPACES=' '
            remember_namespace() {
              PID=${'$'}1
              NS=${'$'}(readlink "/proc/${'$'}PID/ns/mnt" 2>/dev/null || true)
              [ -n "${'$'}NS" ] || return 1
              case "${'$'}SEEN_NAMESPACES" in *" ${'$'}NS "*) return 1 ;; esac
              SEEN_NAMESPACES="${'$'}SEEN_NAMESPACES${'$'}NS "
              return 0
            }
            # PID 1 与所有 zygote 是未来进程的信任来源，任一失败都必须阻止开关伪报成功。
            REQUIRED_PIDS="1 ${'$'}(pidof system_server zygote64 zygote 2>/dev/null || true) ${'$'}(ps -A -o PID,NAME | awk '${'$'}2 ~ /_zygote${'$'}/ {print ${'$'}1}')"
            for PID in ${'$'}REQUIRED_PIDS; do
              remember_namespace "${'$'}PID" || continue
              replace_namespace_store "${'$'}PID"
            done
            # 已运行的 Chrome/WebView 不会重新继承 zygote 挂载；逐个更新现存命名空间，进程退出竞态可忽略。
            APP_PIDS=${'$'}(ps -A -o PID,UID | awk '${'$'}2 ~ /^u[0-9]+_[ai]/ || (${'$'}2 ~ /^[0-9]+${'$'}/ && ${'$'}2 >= 10000) {print ${'$'}1}')
            for PID in ${'$'}APP_PIDS; do
              remember_namespace "${'$'}PID" || continue
              replace_namespace_store "${'$'}PID" 2>/dev/null || true
            done
            am broadcast -a android.security.action.KEYCHAIN_CHANGED >/dev/null 2>&1 || true
            SERVICE_SCRIPT
            chmod 0755 "${'$'}MODDIR/service.sh"
            chown -R root:root "${'$'}MODDIR"
            if [ -d "${'$'}LEGACY" ]; then
              rm -rf "${'$'}LEGACY"
            fi
            sh "${'$'}MODDIR/service.sh" now
        """.trimIndent().replace(certificatePlaceholder, certificate.pem.trimEnd())
    }

    /** 生成所有权感知的卸载事务；仅目标挂载可见本模块 marker 时才执行 umount。 */
    internal fun buildRemoveScript(): String = """
        set -eu
        MODDIR='$moduleDirectory'
        LEGACY='$legacyModuleDirectory'
        APEX_STORE=/apex/com.android.conscrypt/cacerts
        SYSTEM_STORE=/system/etc/security/cacerts
        MARKER=.device_ca_trust_owner
        BUSYBOX=/data/adb/ap/bin/busybox
        [ -x "${'$'}BUSYBOX" ] || BUSYBOX=/data/adb/magisk/busybox
        enter_mount() { if [ -x "${'$'}BUSYBOX" ]; then "${'$'}BUSYBOX" nsenter "${'$'}@"; else nsenter "${'$'}@"; fi; }
        CERT_PATH=${'$'}(find "${'$'}MODDIR/system/etc/security/cacerts" -maxdepth 1 -type f -name '*.0' 2>/dev/null | head -n 1)
        USER_ID=${'$'}(am get-current-user)
        USER_STORE="/data/misc/user/${'$'}USER_ID/cacerts-added"
        if [ -n "${'$'}CERT_PATH" ]; then
          CERT_NAME=${'$'}{CERT_PATH##*/}
          USER_CERT="${'$'}USER_STORE/${'$'}CERT_NAME"
          # 只删除与本模块证书逐字节一致的用户信任，避免哈希文件名碰撞时误删用户自行安装的 CA。
          if cmp -s "${'$'}CERT_PATH" "${'$'}USER_CERT"; then rm -f "${'$'}USER_CERT"; fi
        fi
        SEEN_NAMESPACES=' '
        for NS_PATH in /proc/[0-9]*/ns/mnt; do
          PID=${'$'}{NS_PATH#/proc/}; PID=${'$'}{PID%/ns/mnt}
          NS=${'$'}(readlink "${'$'}NS_PATH" 2>/dev/null || true)
          [ -n "${'$'}NS" ] || continue
          case "${'$'}SEEN_NAMESPACES" in *" ${'$'}NS "*) continue ;; esac
          SEEN_NAMESPACES="${'$'}SEEN_NAMESPACES${'$'}NS "
          # 同一命名空间只进入一次，避免大量 Android 进程导致关闭开关长时间阻塞。
          enter_mount -t "${'$'}PID" -m -- sh -c '
            APEX_STORE=/apex/com.android.conscrypt/cacerts
            SYSTEM_STORE=/system/etc/security/cacerts
            while grep -F " ${'$'}APEX_STORE " /proc/self/mountinfo | tail -n 1 |
              grep -E "/(device_ca_trust|tproxy_ca)/runtime-cacerts" >/dev/null 2>&1; do
              umount "${'$'}APEX_STORE" || break
            done
            if test -e "${'$'}SYSTEM_STORE/.device_ca_trust_owner"; then umount "${'$'}SYSTEM_STORE" 2>/dev/null || true; fi
          ' 2>/dev/null || true
        done
        rm -rf "${'$'}MODDIR" "${'$'}LEGACY"
        am broadcast -a android.security.action.KEYCHAIN_CHANGED >/dev/null 2>&1 || true
    """.trimIndent()

    private companion object {
        const val moduleDirectory = "/data/adb/modules/device_ca_trust"
        const val legacyModuleDirectory = "/data/adb/modules/tproxy_ca"
        const val runtimeStoreLayoutVersion = "2"
        // 先完成模板去缩进再替换 PEM，避免动态多行文本改变 trimIndent 的最小缩进并破坏 heredoc。
        const val certificatePlaceholder = "__DEVICE_CA_CERTIFICATE_PEM__"
    }
}

/** 在有界时间内通过 Root shell 执行固定事务；输出只用于本地诊断并限制大小。 */
class RootCommandRunner {
    /**
     * 把脚本写入 su 的标准输入，避免证书正文出现在进程命令行。
     * 超时会强制结束进程；非零退出只返回稳定状态码，不把设备路径或 shell 输出展示给用户。
     */
    fun execute(script: String) {
        val process = ProcessBuilder("su", "-c", "sh").redirectErrorStream(true).start()
        process.outputStream.bufferedWriter(Charsets.UTF_8).use { writer -> writer.write(script) }
        val output = ByteArrayOutputStream()
        val reader = Thread {
            process.inputStream.use { input ->
                val buffer = ByteArray(4096)
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    // Root 工具可能输出超过诊断预算；必须继续排空管道，否则子进程会因管道写满而伪超时。
                    val retained = minOf(count, maximumOutputBytes - output.size())
                    if (retained > 0) output.write(buffer, 0, retained)
                }
            }
        }.apply { name = "root-command-output"; start() }
        if (!process.waitFor(commandTimeoutSeconds, TimeUnit.SECONDS)) {
            process.destroyForcibly()
            reader.join(readerJoinTimeoutMillis)
            throw IllegalStateException("证书信任操作超时")
        }
        reader.join(readerJoinTimeoutMillis)
        check(process.exitValue() == 0) { "证书信任操作失败，状态码 ${process.exitValue()}" }
    }

    private companion object {
        // 首次同步需要逐个进入已运行应用的挂载命名空间；设备应用较多时 30 秒会误杀仍在正常推进的安装事务。
        const val commandTimeoutSeconds = 60L
        const val readerJoinTimeoutMillis = 2_000L
        const val maximumOutputBytes = 16 * 1024
    }
}
