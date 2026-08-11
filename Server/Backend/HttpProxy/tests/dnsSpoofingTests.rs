#![allow(non_snake_case)]

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};

use capture_core::{RecordingConfiguration, RecordingSession};
use http_proxy_core::{
    DnsSpoofingConfiguration, DnsSpoofingError, DnsSpoofingRule, DnsSpoofingTool, HttpProxyConfig,
    HttpProxyDependencies, SslMitmConfiguration, SslMitmManager, ToolPipeline,
    startHttpProxyWithPluginsAndDns,
};
use location_core::LocationPattern;
use plugin_host::PluginHost;
use rcgen::generate_simple_self_signed;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

/// 创建只作用于测试域名的 DNS 工具，真实出站目标固定为本机。
fn testDnsTool(hostPattern: &str) -> Arc<DnsSpoofingTool> {
    Arc::new(
        DnsSpoofingTool::new(DnsSpoofingConfiguration {
            enabled: true,
            rules: vec![DnsSpoofingRule {
                id: "dns-e2e".to_owned(),
                enabled: true,
                hostPattern: hostPattern.to_owned(),
                ipAddress: Ipv4Addr::LOCALHOST.to_string(),
            }],
        })
        .expect("DNS 测试规则必须有效"),
    )
}

/// 创建端口随机、超时有界的 HTTP 代理配置，防止并行测试争用固定端口。
fn testProxyConfig() -> HttpProxyConfig {
    HttpProxyConfig {
        listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
        listenPort: 0,
        connectTimeoutMilliseconds: 1_000,
        requestTimeoutMilliseconds: 2_000,
        shutdownTimeoutMilliseconds: 1_000,
        ..HttpProxyConfig::default()
    }
}

/// DNS 映射必须只改变实际连接 IP，发送给上游的 HTTP Host 仍保持请求域名和端口。
#[tokio::test]
async fn dnsSpoofingRoutesRealHttpTrafficAndPreservesHost() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("DNS 测试上游必须绑定");
    let upstreamAddress = upstream.local_addr().expect("上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.expect("上游必须收到映射连接");
        let mut requestBytes = [0_u8; 4 * 1024];
        let byteCount = stream
            .read(&mut requestBytes)
            .await
            .expect("上游请求必须可读");
        let request = String::from_utf8_lossy(&requestBytes[..byteCount]);
        assert!(request.starts_with("GET /dns-check HTTP/1.1\r\n"));
        assert!(request.lines().any(|line| {
            line.eq_ignore_ascii_case(&format!(
                "host: mapped.fixture.test:{}",
                upstreamAddress.port()
            ))
        }));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\nConnection: close\r\n\r\ndns-mapped",
            )
            .await
            .expect("上游响应必须写入");
    });
    let captureDirectory = tempfile::tempdir().expect("录制目录必须创建");
    let capture = RecordingSession::new(RecordingConfiguration {
        spillDirectory: captureDirectory.path().to_path_buf(),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("录制会话必须创建");
    let certificateDirectory = tempfile::tempdir().expect("证书目录必须创建");
    let ssl = SslMitmManager::load(certificateDirectory.path()).expect("SSL 管理器必须创建");
    let proxy = startHttpProxyWithPluginsAndDns(
        testProxyConfig(),
        HttpProxyDependencies {
            capture,
            ssl,
            pipeline: ToolPipeline::new(),
            pluginHost: PluginHost::disabled(),
            dnsSpoofing: testDnsTool("*.fixture.test"),
        },
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://{}", proxy.boundAddress()))
                .expect("代理地址必须有效"),
        )
        .build()
        .expect("测试客户端必须创建");

    let response = client
        .get(format!(
            "http://mapped.fixture.test:{}/dns-check",
            upstreamAddress.port()
        ))
        .send()
        .await
        .expect("DNS 映射请求必须成功");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.text().await.expect("响应正文必须可读"),
        "dns-mapped"
    );
    upstreamTask.await.expect("上游任务必须完成");
    proxy.stop().await.expect("代理必须有序停止");
}

/// DNS 映射后的 HTTPS 上游连接必须继续使用原域名作为 SNI，并完成真实 MITM 明文转发。
#[tokio::test]
async fn dnsSpoofingPreservesTlsSniDuringHttpsInterception() {
    let mappedHost = "secure.fixture.test";
    let certified =
        generate_simple_self_signed(vec![mappedHost.to_owned()]).expect("HTTPS 测试证书必须生成");
    let upstreamCertificate: CertificateDer<'static> = certified.cert.der().clone();
    let privateKey = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let serverConfiguration = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![upstreamCertificate.clone()], privateKey)
        .expect("HTTPS 测试服务配置必须有效");
    let acceptor = TlsAcceptor::from(Arc::new(serverConfiguration));
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("HTTPS 测试上游必须绑定");
    let upstreamAddress = upstream.local_addr().expect("HTTPS 上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        // CONNECT 解密路径先建立一次可达性探针再由 HTTPS 客户端池执行 TLS；测试服务必须忽略探针关闭。
        let mut tlsStream = loop {
            let (stream, _) = upstream.accept().await.expect("HTTPS 上游必须收到连接");
            if let Ok(tlsStream) = acceptor.accept(stream).await {
                break tlsStream;
            }
        };
        assert_eq!(tlsStream.get_ref().1.server_name(), Some(mappedHost));
        let mut requestBytes = [0_u8; 4 * 1024];
        let byteCount = tlsStream
            .read(&mut requestBytes)
            .await
            .expect("HTTPS 上游请求必须可读");
        let request = String::from_utf8_lossy(&requestBytes[..byteCount]);
        assert!(request.starts_with("GET /secure-dns HTTP/1.1\r\n"));
        assert!(request.lines().any(|line| {
            line.eq_ignore_ascii_case(&format!("host: {mappedHost}:{}", upstreamAddress.port()))
        }));
        tlsStream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\nConnection: close\r\n\r\ntls-mapped",
            )
            .await
            .expect("HTTPS 上游响应必须写入");
    });

    let captureDirectory = tempfile::tempdir().expect("录制目录必须创建");
    let capture = RecordingSession::new(RecordingConfiguration {
        spillDirectory: captureDirectory.path().to_path_buf(),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("录制会话必须创建");
    let certificateDirectory = tempfile::tempdir().expect("证书目录必须创建");
    let ssl = SslMitmManager::loadWithUpstreamRoots(
        certificateDirectory.path(),
        vec![upstreamCertificate],
    )
    .expect("SSL 管理器必须创建");
    ssl.updateConfiguration(SslMitmConfiguration {
        enabled: true,
        includeLocations: vec![LocationPattern {
            protocol: "https".to_owned(),
            host: mappedHost.to_owned(),
            port: upstreamAddress.port().to_string(),
            path: String::new(),
            query: None,
        }],
        excludeLocations: Vec::new(),
        maxCachedCertificates: 16,
        useClientSni: true,
    })
    .expect("HTTPS 解密规则必须有效");
    let clientRoot = reqwest::Certificate::from_pem(&ssl.exportRootPem())
        .expect("代理根证书必须可供测试客户端信任");
    let proxy = startHttpProxyWithPluginsAndDns(
        testProxyConfig(),
        HttpProxyDependencies {
            capture,
            ssl,
            pipeline: ToolPipeline::new(),
            pluginHost: PluginHost::disabled(),
            dnsSpoofing: testDnsTool(mappedHost),
        },
        CancellationToken::new(),
    )
    .await
    .expect("HTTPS 代理必须启动");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::all(format!("http://{}", proxy.boundAddress()))
                .expect("代理地址必须有效"),
        )
        .tls_certs_only([clientRoot])
        .build()
        .expect("HTTPS 测试客户端必须创建");

    let response = client
        .get(format!(
            "https://{mappedHost}:{}/secure-dns",
            upstreamAddress.port()
        ))
        .send()
        .await
        .expect("DNS 映射后的 HTTPS 请求必须成功");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.text().await.expect("HTTPS 正文必须可读"),
        "tls-mapped"
    );
    upstreamTask.await.expect("HTTPS 上游任务必须完成");
    proxy.stop().await.expect("HTTPS 代理必须有序停止");
}

/// 配置热更新必须原子生效，非法 IP 更新失败后仍保留此前可用规则。
#[test]
fn dnsSpoofingRejectsInvalidUpdateWithoutChangingActiveRules() {
    let tool = testDnsTool("first.fixture.test");
    let error = tool
        .replaceConfiguration(DnsSpoofingConfiguration {
            enabled: true,
            rules: vec![DnsSpoofingRule {
                id: "invalid".to_owned(),
                enabled: true,
                hostPattern: "second.fixture.test".to_owned(),
                ipAddress: "999.1.1.1".to_owned(),
            }],
        })
        .expect_err("非法 IP 必须拒绝");

    assert_eq!(error, DnsSpoofingError::InvalidIpAddress);
    assert_eq!(
        tool.resolveIp("first.fixture.test"),
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
    );
    assert_eq!(tool.resolveIp("second.fixture.test"), None);
}

/// 关闭总开关或单条规则时必须立即恢复系统解析路径，避免停用后仍残留映射结果。
#[test]
fn dnsSpoofingDisableStateReturnsControlToSystemResolver() {
    let tool = testDnsTool("disabled.fixture.test");
    tool.replaceConfiguration(DnsSpoofingConfiguration {
        enabled: false,
        rules: vec![DnsSpoofingRule {
            id: "disabled-by-master".to_owned(),
            enabled: true,
            hostPattern: "disabled.fixture.test".to_owned(),
            ipAddress: Ipv4Addr::LOCALHOST.to_string(),
        }],
    })
    .expect("关闭总开关的规则仍应是有效配置");
    assert_eq!(tool.resolveIp("disabled.fixture.test"), None);

    tool.replaceConfiguration(DnsSpoofingConfiguration {
        enabled: true,
        rules: vec![DnsSpoofingRule {
            id: "disabled-by-rule".to_owned(),
            enabled: false,
            hostPattern: "disabled.fixture.test".to_owned(),
            ipAddress: Ipv4Addr::LOCALHOST.to_string(),
        }],
    })
    .expect("关闭单条规则仍应是有效配置");
    assert_eq!(tool.resolveIp("disabled.fixture.test"), None);
}

/// 多条规则同时命中时只使用列表首项，保证界面拖动后的优先级具有确定结果。
#[test]
fn dnsSpoofingUsesFirstMatchingRule() {
    let tool = DnsSpoofingTool::new(DnsSpoofingConfiguration {
        enabled: true,
        rules: vec![
            DnsSpoofingRule {
                id: "first".to_owned(),
                enabled: true,
                hostPattern: "*.fixture.test".to_owned(),
                ipAddress: "127.0.0.1".to_owned(),
            },
            DnsSpoofingRule {
                id: "second".to_owned(),
                enabled: true,
                hostPattern: "mapped.fixture.test".to_owned(),
                ipAddress: "127.0.0.2".to_owned(),
            },
        ],
    })
    .expect("首条命中测试配置必须有效");

    assert_eq!(
        tool.resolveIp("mapped.fixture.test"),
        Some("127.0.0.1".parse().expect("测试 IP 必须有效"))
    );
}
