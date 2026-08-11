#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

// 分类函数保持为后端内部实现；测试包含同一源文件取得私有项访问权，避免为测试扩大生产公共 API。
include!("../src/socksHttpInspection.rs");

use http_proxy_core::SslMitmConfiguration;
use location_core::LocationPattern;
use tempfile::tempdir;

/// 验证 SOCKS5 目标与 Host 一致的 origin-form 请求会进入 HTTP 事务处理器。
#[test]
fn recognizesMatchingHttpRequest() {
    let request = b"GET /resource HTTP/1.1\r\nHost: www.clash.com\r\n\r\n";
    assert_eq!(
        classifyHttpPrefix(request, "www.clash.com", 80, false, 64 * 1024),
        TunnelClassification::Http { logicalHost: None }
    );
}

/// 验证目标不一致的 GET 保持原始 TCP，避免误把任意文本流改写为 HTTP 代理请求。
#[test]
fn keepsMismatchedHttpPayloadAsRawTcp() {
    let request = b"GET / HTTP/1.1\r\nHost: www.clash.com\r\n\r\n";
    assert_eq!(
        classifyHttpPrefix(request, "127.0.0.1", 19081, false, 64 * 1024),
        TunnelClassification::Raw
    );
}

/// 验证内核固定路由的透明 GET 使用 Host 恢复域名，不再因 CDN 或 DNS 视图与原始 IP 不一致退化为 TCP。
#[test]
fn recognizesRoutePinnedHttpAndReturnsLogicalHost() {
    let request = b"GET / HTTP/1.1\r\nHost: cdn.fixture.invalid:19081\r\n\r\n";
    assert_eq!(
        classifyHttpPrefix(request, "127.0.0.1", 19081, true, 64 * 1024),
        TunnelClassification::Http {
            logicalHost: Some("cdn.fixture.invalid".to_owned())
        }
    );
}

/// 验证 HTTP/2 prior knowledge 前言后已附带 SETTINGS 帧时仍保持 HTTP 分类，而不是因窥视字节超过前言长度退化为 TCP。
#[test]
fn recognizesHttp2PrefaceWithFollowingFrame() {
    let mut prefix = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
    prefix.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
    assert_eq!(
        classifyHttpPrefix(&prefix, "127.0.0.1", 80, true, 64 * 1024),
        TunnelClassification::Http { logicalHost: None }
    );
}

/// 验证 absolute-form URI 与 Host 头均必须绑定 CONNECT 目标，防止 SOCKS5 隧道被重定向到其他上游。
#[test]
fn keepsCrossTargetAbsoluteRequestAsRawTcp() {
    let request = b"GET http://other.example/ HTTP/1.1\r\nHost: example.com\r\n\r\n";
    assert_eq!(
        classifyHttpPrefix(request, "example.com", 80, false, 64 * 1024),
        TunnelClassification::Raw
    );
}

/// 验证默认端口与显式端口按 HTTP authority 语义比较，避免字符串比较误判。
#[test]
fn matchesHttpAuthorityPortSemantics() {
    let defaultPort = Authority::from_str("example.com").expect("默认端口 authority 必须有效");
    let explicitPort =
        Authority::from_str("example.com:8080").expect("显式端口 authority 必须有效");
    assert!(authorityMatchesTarget(&defaultPort, "example.com", 80));
    assert!(authorityMatchesTarget(&explicitPort, "example.com", 8080));
    assert!(!authorityMatchesTarget(&defaultPort, "example.com", 8080));
}

/// 验证完整头部边界不吞掉正文首字节，分类器只用边界前内容做协议判断。
#[test]
fn findsHeaderBoundaryWithoutConsumingBody() {
    let request = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\nbody";
    assert_eq!(findHeaderEnd(request), Some(request.len() - 4));
}

/// 验证 TLS ClientHello 始终保留 HTTPS 分类；只有命中 SSL 规则的目标进入解密处理器。
#[test]
fn recognizesOnlyConfiguredHttpsTunnel() {
    let directory = tempdir().expect("测试证书目录必须创建");
    let manager = SslMitmManager::load(directory.path()).expect("测试 SSL 管理器必须初始化");
    manager
        .updateConfiguration(SslMitmConfiguration {
            enabled: true,
            includeLocations: vec![LocationPattern {
                protocol: "https".to_owned(),
                host: "example.com".to_owned(),
                port: "443".to_owned(),
                path: String::new(),
                query: None,
            }],
            excludeLocations: Vec::new(),
            maxCachedCertificates: 16,
            useClientSni: true,
        })
        .expect("测试 SSL 规则必须有效");
    let clientHelloRecord = [0x16, 0x03, 0x03, 0x00, 0x40, 0x01];
    assert_eq!(
        classifyTlsPrefix(
            &clientHelloRecord,
            "example.com",
            443,
            false,
            &manager,
            64 * 1024
        ),
        TunnelClassification::Https { logicalHost: None }
    );
    assert_eq!(
        classifyTlsPrefix(
            &clientHelloRecord,
            "other.example",
            443,
            false,
            &manager,
            64 * 1024
        ),
        TunnelClassification::RawTls { logicalHost: None }
    );
}
