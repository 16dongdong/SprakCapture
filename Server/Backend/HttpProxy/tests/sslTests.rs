#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use base64::Engine;
use p12_keystore::{Certificate, KeyStore, KeyStoreEntry, PrivateKey, PrivateKeyChain};
use tempfile::tempdir;

use http_proxy_core::{
    ClientCertificateFormat, ClientCertificateImport, ClientCertificateUpdate,
    SslMitmConfiguration, SslMitmManager,
};
use location_core::{LocationPattern, ResolvedLocation};
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};

/// 构造仅填写协议和主机的 HTTPS 匹配规则，避免测试重复协议字段噪声。
fn hostPattern(host: &str) -> LocationPattern {
    LocationPattern {
        protocol: "https".to_owned(),
        host: host.to_owned(),
        port: String::new(),
        path: String::new(),
        query: None,
    }
}

/// 构造已解析 CONNECT 目标，覆盖匹配器需要的全部稳定字段。
fn httpsLocation(host: &str) -> ResolvedLocation {
    ResolvedLocation {
        protocol: "https".to_owned(),
        host: host.to_owned(),
        port: 443,
        path: String::new(),
        query: String::new(),
        display: format!("{host}:443"),
    }
}

/// 首次生成后重新加载应保持同一根指纹，证明用户数据目录只拥有一个稳定 CA。
#[test]
fn reloadPreservesRootIdentity() {
    let directory = tempdir().expect("临时证书目录必须可创建");
    let first = SslMitmManager::load(directory.path()).expect("首次根证书必须生成");
    let second = SslMitmManager::load(directory.path()).expect("已有根证书必须加载");
    assert_eq!(
        first.publicState().ca.fingerprintSha256,
        second.publicState().ca.fingerprintSha256
    );
    assert_eq!(first.exportRootDer(), second.exportRootDer());
}

/// exclude 必须覆盖 include，且 include 为空或全局关闭时不得解密任何 CONNECT。
#[test]
fn excludeTakesPriorityOverInclude() {
    let directory = tempdir().expect("临时证书目录必须可创建");
    let manager = SslMitmManager::load(directory.path()).expect("根证书必须可用");
    manager
        .updateConfiguration(SslMitmConfiguration {
            enabled: true,
            includeLocations: vec![hostPattern("*.example.test")],
            excludeLocations: vec![hostPattern("private.example.test")],
            ..SslMitmConfiguration::default()
        })
        .expect("规则必须有效");
    assert!(manager.shouldIntercept(&httpsLocation("api.example.test")));
    assert!(!manager.shouldIntercept(&httpsLocation("private.example.test")));
    assert!(!manager.shouldIntercept(&httpsLocation("example.invalid")));
}

/// 更换根证书必须改变指纹并清空已签发叶证书，防止混用旧证书链。
#[test]
fn regenerationClearsLeafCache() {
    let directory = tempdir().expect("临时证书目录必须可创建");
    let manager = SslMitmManager::load(directory.path()).expect("根证书必须可用");
    manager
        .updateConfiguration(SslMitmConfiguration {
            enabled: true,
            includeLocations: vec![hostPattern("*")],
            ..SslMitmConfiguration::default()
        })
        .expect("规则必须有效");
    manager
        .primeLeafCertificate("example.test")
        .expect("叶证书必须可签发");
    let oldFingerprint = manager.publicState().ca.fingerprintSha256;
    let state = manager.regenerateRoot().expect("根证书必须可更换");
    assert_ne!(state.ca.fingerprintSha256, oldFingerprint);
    assert_eq!(state.cachedLeafCount, 0);
}

/// 下游 TLS 必须优先协商 HTTP/2 并保留 HTTP/1.1 回退，公开能力列表同时覆盖 HTTP/1.0 语义。
#[test]
fn downstreamTlsAdvertisesModernHttpVersions() {
    let directory = tempdir().expect("临时证书目录必须可创建");
    let manager = SslMitmManager::load(directory.path()).expect("根证书必须可用");
    let configuration = manager
        .downstreamServerConfiguration("api.example.test")
        .expect("下游 TLS 配置必须生成");
    assert_eq!(
        configuration.alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    );
    assert_eq!(
        manager.publicState().supportedHttpVersions,
        vec!["HTTP/1.0", "HTTP/1.1", "HTTP/2"]
    );
}

/// 生成带 ClientAuth 用途的自签名测试身份；返回 PEM、DER 与 PKCS#8 私钥三种导入素材。
fn clientIdentityMaterial() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let key = KeyPair::generate().expect("测试私钥必须生成");
    let mut parameters = CertificateParams::new(vec!["client.example.test".to_owned()])
        .expect("测试证书参数必须有效");
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let certificate = parameters.self_signed(&key).expect("测试证书必须签发");
    (
        certificate.pem().into_bytes(),
        certificate.der().as_ref().to_vec(),
        key.serialize_der(),
    )
}

/// PEM 与 DER 客户端身份必须按主机规则启用、可更新并可从磁盘完整重载。
#[test]
fn clientCertificatesSupportPemDerAndHostSelection() {
    let directory = tempdir().expect("临时证书目录必须可创建");
    let manager = SslMitmManager::load(directory.path()).expect("根证书必须可用");
    let (certificatePem, certificateDer, privateKeyDer) = clientIdentityMaterial();
    let imported = manager
        .importClientCertificate(ClientCertificateImport {
            name: "接口身份".to_owned(),
            format: ClientCertificateFormat::Pem,
            enabled: true,
            locations: vec![hostPattern("api.example.test")],
            certificateBytes: certificatePem,
            keyBytes: Some({
                let encoded = base64::engine::general_purpose::STANDARD.encode(&privateKeyDer);
                format!("-----BEGIN PRIVATE KEY-----\n{encoded}\n-----END PRIVATE KEY-----\n")
                    .into_bytes()
            }),
            password: String::new(),
        })
        .expect("PEM 身份必须导入");
    assert_eq!(imported.clientCertificates.len(), 1);
    manager
        .upstreamClientConfiguration(Some(&httpsLocation("api.example.test")))
        .expect("命中身份必须形成 TLS 配置");
    let id = imported.clientCertificates[0].id.clone();
    manager
        .updateClientCertificate(
            &id,
            ClientCertificateUpdate {
                name: "更新身份".to_owned(),
                enabled: false,
                locations: vec![hostPattern("api.example.test")],
            },
        )
        .expect("身份元数据必须更新");
    let reloaded = SslMitmManager::load(directory.path()).expect("身份材料必须可重载");
    assert_eq!(
        reloaded.publicState().clientCertificates[0].name,
        "更新身份"
    );
    reloaded.removeClientCertificate(&id).expect("身份必须删除");

    let derDirectory = tempdir().expect("DER 临时目录必须创建");
    let derManager = SslMitmManager::load(derDirectory.path()).expect("DER 根证书必须可用");
    derManager
        .importClientCertificate(ClientCertificateImport {
            name: "DER 身份".to_owned(),
            format: ClientCertificateFormat::Der,
            enabled: true,
            locations: vec![hostPattern("der.example.test")],
            certificateBytes: certificateDer,
            keyBytes: Some(privateKeyDer),
            password: String::new(),
        })
        .expect("DER 身份必须导入");
}

/// 现代 AES 加密的 PKCS#12 容器必须直接导入，错误口令不得生成任何公开身份记录。
#[test]
fn clientCertificatesSupportPasswordProtectedPkcs12() {
    let directory = tempdir().expect("临时证书目录必须可创建");
    let manager = SslMitmManager::load(directory.path()).expect("根证书必须可用");
    let (_, certificateDer, privateKeyDer) = clientIdentityMaterial();
    let mut store = KeyStore::new();
    let chain = PrivateKeyChain::new(
        "client-key",
        PrivateKey::from_der(&privateKeyDer).expect("PKCS#8 私钥必须可解析"),
        [Certificate::from_der(&certificateDer).expect("测试证书必须可解析")],
    );
    store.add_entry("client", KeyStoreEntry::PrivateKeyChain(chain));
    let container = store
        .writer("container-password")
        .write()
        .expect("现代 PKCS#12 容器必须可生成");

    manager
        .importClientCertificate(ClientCertificateImport {
            name: "PFX 身份".to_owned(),
            format: ClientCertificateFormat::Pkcs12,
            enabled: true,
            locations: vec![hostPattern("pfx.example.test")],
            certificateBytes: container.clone(),
            keyBytes: None,
            password: "container-password".to_owned(),
        })
        .expect("正确口令必须完成导入");

    let secondDirectory = tempdir().expect("错误口令测试目录必须创建");
    let secondManager = SslMitmManager::load(secondDirectory.path()).expect("根证书必须可用");
    assert!(
        secondManager
            .importClientCertificate(ClientCertificateImport {
                name: "错误口令".to_owned(),
                format: ClientCertificateFormat::Pkcs12,
                enabled: true,
                locations: vec![hostPattern("pfx.example.test")],
                certificateBytes: container,
                keyBytes: None,
                password: "wrong-password".to_owned(),
            })
            .is_err()
    );
    assert!(secondManager.publicState().clientCertificates.is_empty());
}
