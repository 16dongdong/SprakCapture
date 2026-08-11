#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use http_proxy_core::{canonicalAuthority, canonicalHostHeader};

/// 默认 HTTPS 端口必须同时从 URI authority 与 Host 中省略，避免 HTTP/2 上游看到文本冲突。
#[test]
fn omitsDefaultHttpsPortFromAuthority() {
    assert_eq!(
        canonicalAuthority("cdn.example.test", 443, 443)
            .expect("HTTPS authority 必须生成")
            .as_str(),
        "cdn.example.test"
    );
}

/// 非默认 HTTPS 端口属于目标身份，URI authority 与 Host 都必须保留该端口。
#[test]
fn preservesNonDefaultHttpsPortInAuthority() {
    assert_eq!(
        canonicalAuthority("cdn.example.test", 8_443, 443)
            .expect("非默认 HTTPS authority 必须生成")
            .as_str(),
        "cdn.example.test:8443"
    );
}

/// 默认 HTTP 端口不进入 Host，避免同一主机因显式端口产生不必要差异。
#[test]
fn omitsDefaultPortFromHostHeader() {
    assert_eq!(
        canonicalHostHeader("example.test", 80, 80)
            .expect("域名必须生成 Host")
            .as_bytes(),
        b"example.test"
    );
}

/// 非默认端口必须进入 Host，确保虚拟主机能够识别实际 authority。
#[test]
fn preservesNonDefaultPortInHostHeader() {
    assert_eq!(
        canonicalHostHeader("example.test", 8_080, 80)
            .expect("域名与端口必须生成 Host")
            .as_bytes(),
        b"example.test:8080"
    );
}

/// IPv6 字面量必须使用方括号，避免地址冒号与端口分隔符产生歧义。
#[test]
fn bracketsIpv6HostHeader() {
    assert_eq!(
        canonicalHostHeader("::1", 8_080, 80)
            .expect("IPv6 与端口必须生成 Host")
            .as_bytes(),
        b"[::1]:8080"
    );
}
