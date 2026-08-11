#![allow(non_snake_case)]

use location_core::{
    LocationError, LocationMatchOptions, LocationPattern, ResolvedLocation, locationMatches,
    validateLocationPattern,
};

/// 创建稳定的 HTTP 候选，减少每个匹配用例重复拼装无关字段。
fn candidate(host: &str, port: u16, path: &str) -> ResolvedLocation {
    ResolvedLocation {
        protocol: "http".to_owned(),
        host: host.to_owned(),
        port,
        path: path.to_owned(),
        query: "page=1&mode=full".to_owned(),
        display: format!("http://{host}:{port}{path}"),
    }
}

/// 验证精确字段与默认 host 大小写规则。
#[test]
fn exactLocationMatchesCaseInsensitiveHost() {
    let pattern = LocationPattern {
        protocol: "HTTP".to_owned(),
        host: "API.Example.COM".to_owned(),
        port: "80".to_owned(),
        path: "/v1/items".to_owned(),
        query: None,
    };
    assert!(
        locationMatches(
            &pattern,
            &candidate("api.example.com", 80, "/v1/items"),
            LocationMatchOptions::default()
        )
        .expect("精确 Location 应合法")
    );
}

/// 验证 SOCKS 使用与 HTTP 相同的 Location 过滤边界，使会话投影可被录制忽略规则精确控制。
#[test]
fn socksLocationUsesUnifiedMatcher() {
    let pattern = LocationPattern {
        protocol: "socks".to_owned(),
        host: "*.example.com".to_owned(),
        port: "443".to_owned(),
        path: "/".to_owned(),
        query: None,
    };
    let location = ResolvedLocation {
        protocol: "socks".to_owned(),
        host: "api.example.com".to_owned(),
        port: 443,
        path: "/".to_owned(),
        query: String::new(),
        display: "socks5://api.example.com:443".to_owned(),
    };
    assert!(
        locationMatches(&pattern, &location, LocationMatchOptions::default())
            .expect("SOCKS Location 应合法")
    );
}

/// 验证透明 TCP/TLS 事务可沿用统一 Location 匹配器，录制规则无需伪装成 HTTP 或 SOCKS。
#[test]
fn transparentTransportLocationsUseUnifiedMatcher() {
    for protocol in ["tcp", "tls"] {
        let pattern = LocationPattern {
            protocol: protocol.to_owned(),
            host: "*.fixture.local".to_owned(),
            port: "443".to_owned(),
            path: String::new(),
            query: None,
        };
        let location = ResolvedLocation {
            protocol: protocol.to_owned(),
            host: "stream.fixture.local".to_owned(),
            port: 443,
            path: String::new(),
            query: String::new(),
            display: format!("{protocol}://stream.fixture.local:443"),
        };
        assert!(
            locationMatches(&pattern, &location, LocationMatchOptions::default())
                .expect("透明传输 Location 应合法")
        );
    }
}

/// 验证子域通配必须至少包含一级子域，不能误命中根域。
#[test]
fn wildcardSubdomainExcludesApexHost() {
    let pattern = LocationPattern {
        host: "*.example.com".to_owned(),
        ..LocationPattern::default()
    };
    assert!(
        locationMatches(
            &pattern,
            &candidate("a.example.com", 80, "/"),
            LocationMatchOptions::default()
        )
        .expect("子域规则应合法")
    );
    assert!(
        !locationMatches(
            &pattern,
            &candidate("example.com", 80, "/"),
            LocationMatchOptions::default()
        )
        .expect("根域候选应合法")
    );
}

/// 验证问号只消费一个字符，星号可消费剩余主机段。
#[test]
fn wildcardQuestionConsumesOneCharacter() {
    let pattern = LocationPattern {
        host: "api-?.*.example.com".to_owned(),
        ..LocationPattern::default()
    };
    assert!(
        locationMatches(
            &pattern,
            &candidate("api-a.eu.example.com", 80, "/"),
            LocationMatchOptions::default()
        )
        .expect("主机通配规则应合法")
    );
    assert!(
        !locationMatches(
            &pattern,
            &candidate("api-ab.eu.example.com", 80, "/"),
            LocationMatchOptions::default()
        )
        .expect("主机候选应合法")
    );
}

/// 验证无通配路径按目录边界做前缀匹配。
#[test]
fn pathPrefixUsesSegmentBoundary() {
    let pattern = LocationPattern {
        path: "/api".to_owned(),
        ..LocationPattern::default()
    };
    assert!(
        locationMatches(
            &pattern,
            &candidate("example.com", 80, "/api/v1"),
            LocationMatchOptions::default()
        )
        .expect("路径规则应合法")
    );
    assert!(
        !locationMatches(
            &pattern,
            &candidate("example.com", 80, "/apix"),
            LocationMatchOptions::default()
        )
        .expect("路径候选应合法")
    );
}

/// 验证显式路径通配符可匹配任意后缀层级。
#[test]
fn pathWildcardMatchesNestedSuffix() {
    let pattern = LocationPattern {
        path: "/api/*/items/?".to_owned(),
        ..LocationPattern::default()
    };
    assert!(
        locationMatches(
            &pattern,
            &candidate("example.com", 80, "/api/v1/items/7"),
            LocationMatchOptions::default()
        )
        .expect("路径通配规则应合法")
    );
}

/// 验证端口列表、闭区间与区间外候选。
#[test]
fn portListAndRangeMatchExpectedCandidates() {
    let pattern = LocationPattern {
        port: "80, 8000-8100,443".to_owned(),
        ..LocationPattern::default()
    };
    for port in [80, 443, 8_000, 8_100] {
        assert!(
            locationMatches(
                &pattern,
                &candidate("example.com", port, "/"),
                LocationMatchOptions::default()
            )
            .expect("端口规则应合法")
        );
    }
    assert!(
        !locationMatches(
            &pattern,
            &candidate("example.com", 8_101, "/"),
            LocationMatchOptions::default()
        )
        .expect("端口候选应合法")
    );
}

/// 验证所有空字段构成“任何位置”规则。
#[test]
fn emptyPatternMatchesAnyValidLocation() {
    assert!(
        locationMatches(
            &LocationPattern::default(),
            &candidate("any.example", 12_345, "/deep/path"),
            LocationMatchOptions::default()
        )
        .expect("空规则应合法")
    );
}

/// 验证查询普通文本使用子串语义，通配表达式使用完整匹配语义。
#[test]
fn querySupportsSubstringAndWildcard() {
    let substringPattern = LocationPattern {
        query: Some("mode=full".to_owned()),
        ..LocationPattern::default()
    };
    let wildcardPattern = LocationPattern {
        query: Some("page=?&mode=*".to_owned()),
        ..LocationPattern::default()
    };
    let candidate = candidate("example.com", 80, "/");
    assert!(
        locationMatches(
            &substringPattern,
            &candidate,
            LocationMatchOptions::default()
        )
        .expect("查询子串规则应合法")
    );
    assert!(
        locationMatches(
            &wildcardPattern,
            &candidate,
            LocationMatchOptions::default()
        )
        .expect("查询通配规则应合法")
    );
}

/// 验证默认路径规范化忽略多余尾斜杠。
#[test]
fn pathNormalizationIgnoresTrailingSlash() {
    let pattern = LocationPattern {
        path: "/api/".to_owned(),
        ..LocationPattern::default()
    };
    assert!(
        locationMatches(
            &pattern,
            &candidate("example.com", 80, "/api"),
            LocationMatchOptions::default()
        )
        .expect("路径规则应合法")
    );
}

/// 验证 IPv6 方括号只影响展示，不影响字面地址匹配。
#[test]
fn ipv6LiteralMatchesWithOptionalBrackets() {
    let pattern = LocationPattern {
        host: "[2001:db8::1]".to_owned(),
        ..LocationPattern::default()
    };
    assert!(
        locationMatches(
            &pattern,
            &candidate("2001:db8::1", 80, "/"),
            LocationMatchOptions::default()
        )
        .expect("IPv6 规则应合法")
    );
}

/// 验证调用方可显式启用 host 大小写敏感语义。
#[test]
fn hostCaseSensitivityOptionIsHonored() {
    let pattern = LocationPattern {
        host: "API.example.com".to_owned(),
        ..LocationPattern::default()
    };
    assert!(
        !locationMatches(
            &pattern,
            &candidate("api.example.com", 80, "/"),
            LocationMatchOptions {
                caseSensitiveHost: true,
                normalizePath: true,
            }
        )
        .expect("大小写敏感规则应合法")
    );
}

/// 验证无效协议、端口、主机和路径返回可机器判别的稳定错误。
#[test]
fn invalidPatternsReturnStructuredErrors() {
    let cases = [
        (
            LocationPattern {
                protocol: "ftp".to_owned(),
                ..LocationPattern::default()
            },
            LocationError::InvalidProtocol,
        ),
        (
            LocationPattern {
                host: "http://example.com".to_owned(),
                ..LocationPattern::default()
            },
            LocationError::InvalidHost,
        ),
        (
            LocationPattern {
                host: "example.com?mode=full".to_owned(),
                ..LocationPattern::default()
            },
            LocationError::InvalidHost,
        ),
        (
            LocationPattern {
                host: "[example.com]".to_owned(),
                ..LocationPattern::default()
            },
            LocationError::InvalidHost,
        ),
        (
            LocationPattern {
                port: "9000-8000".to_owned(),
                ..LocationPattern::default()
            },
            LocationError::InvalidPort,
        ),
        (
            LocationPattern {
                path: "api".to_owned(),
                ..LocationPattern::default()
            },
            LocationError::InvalidPath,
        ),
    ];
    for (pattern, expectedError) in cases {
        assert_eq!(validateLocationPattern(&pattern), Err(expectedError));
    }
}

/// 验证数据面候选不能携带通配符、URL 或零端口等未解析输入。
#[test]
fn invalidResolvedCandidateReturnsStructuredError() {
    let invalidCandidate = ResolvedLocation {
        protocol: "http".to_owned(),
        host: "*.example.com".to_owned(),
        port: 80,
        path: "/".to_owned(),
        query: String::new(),
        display: String::new(),
    };
    assert_eq!(
        locationMatches(
            &LocationPattern::default(),
            &invalidCandidate,
            LocationMatchOptions::default()
        ),
        Err(LocationError::InvalidCandidate)
    );
}
