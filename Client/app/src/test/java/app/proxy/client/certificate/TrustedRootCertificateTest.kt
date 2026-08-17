package app.proxy.client.certificate

import java.util.Base64
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** 验证服务端根证书到 Android 系统安装材料的确定性转换。 */
class TrustedRootCertificateTest {
    /** 当前抓包根证书必须得到与 OpenSSL subject_hash_old 一致的文件名并保留规范 PEM。 */
    @Test
    fun parsesValidatedCaAndBuildsAndroidSubjectHash() {
        val certificate = TrustedRootCertificate.parse(Base64.getDecoder().decode(certificateBase64))

        assertEquals("740fef3d", certificate.subjectHash)
        assertTrue(certificate.pem.startsWith("-----BEGIN CERTIFICATE-----\n"))
        assertTrue(certificate.pem.endsWith("-----END CERTIFICATE-----\n"))
        assertEquals(64, certificate.fingerprint.length)
    }

    /** 安装与卸载脚本必须更新所有命名空间、纠正 SELinux 标签并识别已删除的历史挂载。 */
    @Test
    fun installationScriptsKeepOwnershipBoundary() {
        val certificate = TrustedRootCertificate.parse(Base64.getDecoder().decode(certificateBase64))
        val installer = RootCertificateInstaller()
        val install = installer.buildInstallScript(certificate)
        val remove = installer.buildRemoveScript()

        assertTrue(install.contains(".device_ca_trust_owner"))
        assertTrue(install.contains("service.sh\" now"))
        assertTrue(install.contains("740fef3d.0"))
        assertTrue(install.contains("CERT_NAME=\$(basename"))
        assertTrue(install.lines().contains("MODULE_PROP"))
        assertTrue(install.lines().contains("CERTIFICATE_PEM"))
        assertTrue(install.lines().contains("SERVICE_SCRIPT"))
        assertTrue(install.lines().contains("-----BEGIN CERTIFICATE-----"))
        assertTrue(install.contains("chcon -R u:object_r:system_file:s0"))
        assertTrue(install.contains("APP_PIDS=\$(ps -A -o PID,UID"))
        assertTrue(install.contains("runtime-cacerts.next"))
        assertTrue(install.contains("LAYOUT_VERSION=2"))
        assertTrue(install.contains("cp -a \"\$SYSTEM_STORE/.\" \"\$NEXT_STORE/\""))
        assertTrue(install.contains(".layoutVersion"))
        assertFalse(install.contains("cp -a \"\$APEX_STORE/.\" \"\$NEXT_STORE/\""))
        assertTrue(install.contains("USER_STORE=\"/data/misc/user/\$USER_ID/cacerts-added\""))
        assertTrue(install.contains("mv -f \"\$USER_NEXT\" \"\$USER_CERT\""))
        assertTrue(remove.contains("cmp -s \"\$CERT_PATH\" \"\$USER_CERT\""))
        assertTrue(install.contains("/device_ca_trust/runtime-cacerts//deleted"))
        assertFalse(install.contains("rm -rf \"\${'$'}RUNTIME_STORE\""))
        assertTrue(remove.contains("同一命名空间只进入一次"))
        assertFalse(install.contains("rootCA.key"))
    }

    private companion object {
        const val certificateBase64 =
            "MIIBrTCCAVKgAwIBAgIUF6LcKPDsXIqtWbOTED1bRDu6Oy8wCgYIKoZIzj0EAwIw" +
                "NDEcMBoGA1UEAwwTTG9jYWwgUHJveHkgUm9vdCBDQTEUMBIGA1UECgwLTG9jYWwg" +
                "UHJveHkwHhcNMjYwODE1MDc1MDIzWhcNMzYwODEzMDc1MDIzWjA0MRwwGgYDVQQD" +
                "DBNMb2NhbCBQcm94eSBSb290IENBMRQwEgYDVQQKDAtMb2NhbCBQcm94eTBZMBMG" +
                "ByqGSM49AgEGCCqGSM49AwEHA0IABJnILakVl4AL3cB55BOuYrviJLD0/yRxOepE" +
                "vE/WGurvO7eV5fMijtPhNgPb4MuNHw5EV6tuE66EJqQ2q7H0nPajQjBAMA4GA1Ud" +
                "DwEB/wQEAwIBhjAdBgNVHQ4EFgQUnzZTkkp2q/JHe/HIDkgBC4h6BKYwDwYDVR0T" +
                "AQH/BAUwAwEB/zAKBggqhkjOPQQDAgNJADBGAiEA9SY79DpMsRUcmUcE8jtPSCud" +
                "kuN3QcyChTdUaHzp1FYCIQDJ3UOrb5ki7X74ZprVyc3CWTXgCv3TnM7M/5WU55/2" +
                "yw=="
    }
}
