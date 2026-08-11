#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::path::Path;

use bytes::Bytes;
use http::{HeaderValue, StatusCode, header::CONTENT_TYPE};
use http_proxy_core::tools::{
    MapLocalConfiguration, MapLocalResponseSource, MapLocalRule, MapLocalTool,
    MapRemoteConfiguration, MapRemoteRule, MapRemoteTarget, MapRemoteTool, MapResponseHeader,
    MapToolError,
};
use location_core::{LocationPattern, ResolvedLocation};

const maximumMapRuleCount: usize = 2_000;
const maximumLocalPathLength: usize = 4_096;
const maximumResponseHeaderCount: usize = 128;
const maximumContentTypeOverrideLength: usize = 512;
const maximumRemotePathLength: usize = 2_048;

/// 构造最小化已解析 HTTP Location，避免每个工具测试重复无关的协议字段。
fn httpLocation(host: &str, port: u16, path: &str) -> ResolvedLocation {
    ResolvedLocation {
        protocol: "http".to_owned(),
        host: host.to_owned(),
        port,
        path: path.to_owned(),
        query: "page=1".to_owned(),
        display: format!("http://{host}:{port}{path}?page=1"),
    }
}

/// 构造单条远端映射规则，测试只关注 Location 到目标模板的转换行为。
fn remoteRule(fromPath: &str, target: MapRemoteTarget) -> MapRemoteRule {
    MapRemoteRule {
        id: "remote-main".to_owned(),
        enabled: true,
        r#from: LocationPattern {
            protocol: "http".to_owned(),
            host: "source.test".to_owned(),
            port: "80".to_owned(),
            path: fromPath.to_owned(),
            query: None,
        },
        to: target,
    }
}

/// 构造目录型 Local 规则，以相同 Location 和规则 ID 覆盖文件、缺失与路径拒绝分支。
fn directoryRule(localPath: String) -> MapLocalRule {
    MapLocalRule {
        id: "local-main".to_owned(),
        enabled: true,
        location: LocationPattern {
            protocol: "http".to_owned(),
            host: "source.test".to_owned(),
            port: "80".to_owned(),
            path: "/*".to_owned(),
            query: None,
        },
        localPath,
        isDirectory: true,
        statusCode: 200,
        responseHeaders: vec![MapResponseHeader {
            name: "x-local-rule".to_owned(),
            value: "active".to_owned(),
        }],
        contentTypeOverride: String::new(),
    }
}

/// Map Remote 必须更新目标四元组、保留 query 和原始 Location，并记录可直接写入 appliedTools 的规则痕迹。
#[test]
fn mapRemoteRewritesTargetAndPreservesOriginalLocation() {
    let tool = MapRemoteTool::new(MapRemoteConfiguration {
        enabled: true,
        rules: vec![remoteRule(
            "/v1/*",
            MapRemoteTarget {
                protocol: "https".to_owned(),
                host: "LOCALHOST".to_owned(),
                port: "8443".to_owned(),
                path: "/v2/*".to_owned(),
            },
        )],
    })
    .expect("远端规则必须有效");
    let source = httpLocation("source.test", 80, "/v1/users");

    let application = tool
        .applyRemote(&source)
        .expect("远端匹配不得失败")
        .expect("规则必须命中");

    assert_eq!(application.originalLocation, source);
    assert_eq!(application.mappedLocation.protocol, "https");
    assert_eq!(application.mappedLocation.host, "localhost");
    assert_eq!(application.mappedLocation.port, 8443);
    assert_eq!(application.mappedLocation.path, "/v2/users");
    assert_eq!(application.mappedLocation.query, "page=1");
    assert_eq!(application.appliedTool, "mapRemote:remote-main");
    assert_eq!(
        application
            .upstreamUri()
            .expect("映射 URI 必须有效")
            .to_string(),
        "https://localhost:8443/v2/users?page=1"
    );
    assert_eq!(
        application.hostHeader().expect("Host 必须有效"),
        HeaderValue::from_static("localhost:8443")
    );
}

/// Map Remote 只应用首条命中规则，避免 A→B→A 等规则集循环以及多个规则的不可解释串联。
#[test]
fn mapRemoteAppliesOnlyFirstMatchingRule() {
    let mut secondRule = remoteRule(
        "/*",
        MapRemoteTarget {
            host: "second.test".to_owned(),
            ..MapRemoteTarget::default()
        },
    );
    secondRule.id = "remote-second".to_owned();
    let tool = MapRemoteTool::new(MapRemoteConfiguration {
        enabled: true,
        rules: vec![
            remoteRule(
                "/*",
                MapRemoteTarget {
                    host: "first.test".to_owned(),
                    ..MapRemoteTarget::default()
                },
            ),
            secondRule,
        ],
    })
    .expect("规则 ID 必须唯一");
    let source = httpLocation("source.test", 80, "/api");
    let application = tool
        .applyRemote(&source)
        .expect("匹配不得失败")
        .expect("规则必须命中");

    assert_eq!(application.mappedLocation.host, "first.test");
}

/// 无目标星号或无命中规则时不得改写 URL，防止工具配置关闭后仍影响正常出站。
#[test]
fn mapRemoteLeavesUnmatchedLocationUntouched() {
    let tool = MapRemoteTool::new(MapRemoteConfiguration {
        enabled: true,
        rules: vec![remoteRule(
            "/v1/*",
            MapRemoteTarget {
                host: "mapped.test".to_owned(),
                ..MapRemoteTarget::default()
            },
        )],
    })
    .expect("规则必须有效");

    assert!(
        tool.applyRemote(&httpLocation("source.test", 80, "/v2/users"))
            .expect("匹配不得失败")
            .is_none()
    );
}

/// JSON 文件命中必须形成带正确正文和 MIME 的合成响应，调用方据此短路而不建立上游连接。
#[tokio::test]
async fn mapLocalReturnsMappedFileResponse() {
    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let filePath = directory.path().join("fixture.json");
    tokio::fs::write(&filePath, br#"{"mapped":true}"#)
        .await
        .expect("JSON 夹具必须写入");
    let tool = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![MapLocalRule {
                id: "local-file".to_owned(),
                enabled: true,
                location: LocationPattern {
                    protocol: "http".to_owned(),
                    host: "source.test".to_owned(),
                    port: "80".to_owned(),
                    path: "/fixture".to_owned(),
                    query: None,
                },
                localPath: filePath.to_string_lossy().into_owned(),
                isDirectory: false,
                statusCode: 201,
                responseHeaders: vec![MapResponseHeader {
                    name: "x-mapped".to_owned(),
                    value: "yes".to_owned(),
                }],
                contentTypeOverride: String::new(),
            }],
        },
        directory.path(),
    )
    .expect("文件规则必须有效");

    let resolution = tool
        .resolveLocal(&httpLocation("source.test", 80, "/fixture"))
        .await
        .expect("本地读取必须成功");
    let response = resolution.syntheticResponse().expect("规则必须短路");

    assert_eq!(response.status, StatusCode::CREATED);
    assert_eq!(response.body, Bytes::from_static(br#"{"mapped":true}"#));
    assert_eq!(response.contentType, "application/json");
    assert_eq!(response.headers["x-mapped"], "yes");
    assert_eq!(response.headers[CONTENT_TYPE], "application/json");
    assert_eq!(response.appliedTool, "mapLocal:local-file");
    assert_eq!(response.source, MapLocalResponseSource::File);
}

/// 目录映射必须按 URL path 相对拼接，根路径访问目录时自动回落到 index.html。
#[tokio::test]
async fn mapLocalDirectoryUsesRequestPathAndIndexFile() {
    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let applicationDirectory = directory.path().join("app");
    tokio::fs::create_dir_all(&applicationDirectory)
        .await
        .expect("目录夹具必须创建");
    tokio::fs::write(
        applicationDirectory.join("main.js"),
        b"console.log('mapped');",
    )
    .await
    .expect("脚本夹具必须写入");
    tokio::fs::write(directory.path().join("index.html"), b"<h1>mapped</h1>")
        .await
        .expect("索引夹具必须写入");
    let tool = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![directoryRule(".".to_owned())],
        },
        directory.path(),
    )
    .expect("目录规则必须有效");

    let scriptResolution = tool
        .resolveLocal(&httpLocation("source.test", 80, "/app/main.js"))
        .await
        .expect("目录脚本映射必须成功");
    let script = scriptResolution
        .syntheticResponse()
        .expect("目录规则必须短路");
    assert_eq!(script.body, Bytes::from_static(b"console.log('mapped');"));
    assert!(
        script.contentType.contains("javascript"),
        "脚本 MIME 必须由扩展名推断为 JavaScript 类型"
    );

    let indexResolution = tool
        .resolveLocal(&httpLocation("source.test", 80, "/"))
        .await
        .expect("目录索引映射必须成功");
    let index = indexResolution
        .syntheticResponse()
        .expect("目录规则必须短路");
    assert_eq!(index.body, Bytes::from_static(b"<h1>mapped</h1>"));
}

/// 原始或 percent 编码的 `..` 均必须合成为 403，且不因目标文件是否存在而泄露目录外信息。
#[tokio::test]
async fn mapLocalRejectsDirectoryTraversal() {
    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let tool = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![directoryRule(".".to_owned())],
        },
        directory.path(),
    )
    .expect("目录规则必须有效");

    for path in [
        "/../secret.json",
        "/%2e%2e/secret.json",
        "/%2E%2E/secret.json",
    ] {
        let resolution = tool
            .resolveLocal(&httpLocation("source.test", 80, path))
            .await
            .expect("路径拒绝必须为合成响应");
        let response = resolution.syntheticResponse().expect("命中规则必须短路");
        assert_eq!(response.status, StatusCode::FORBIDDEN);
        assert_eq!(response.source, MapLocalResponseSource::PathTraversal);
    }
}

/// 缺失映射文件必须返回 404 而非回源，确保启用 Map Local 的匹配规则具备确定性短路语义。
#[tokio::test]
async fn mapLocalMissingFileReturnsSyntheticNotFound() {
    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let tool = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![directoryRule(".".to_owned())],
        },
        directory.path(),
    )
    .expect("目录规则必须有效");

    let resolution = tool
        .resolveLocal(&httpLocation("source.test", 80, "/absent.json"))
        .await
        .expect("缺失文件必须合成响应");
    let response = resolution.syntheticResponse().expect("命中规则必须短路");
    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert_eq!(response.source, MapLocalResponseSource::Missing);
}

/// 规则配置中的相对路径只能相对显式 mappingRoot；工作目录变化不得改变既有工具实例的解析根。
#[test]
fn resolvesRelativeRulePathAgainstExplicitMappingRoot() {
    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let tool = MapLocalTool::new(MapLocalConfiguration::default(), directory.path())
        .expect("空配置必须有效");
    assert_eq!(
        tool.resolveRulePath("fixtures/one.json"),
        directory.path().join(Path::new("fixtures/one.json"))
    );
}

/// 验证两类映射规则在公开契约上都接受恰好 2,000 条，规则顺序和 ID 校验不改变边界语义。
#[test]
fn acceptsMaximumMapRuleCount() {
    let remoteRules = (0..maximumMapRuleCount)
        .map(|index| {
            let mut rule = remoteRule("/*", MapRemoteTarget::default());
            rule.id = format!("remote-{index}");
            rule
        })
        .collect();
    MapRemoteTool::new(MapRemoteConfiguration {
        enabled: true,
        rules: remoteRules,
    })
    .expect("2,000 条远程映射规则必须有效");

    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let localRules = (0..maximumMapRuleCount)
        .map(|index| {
            let mut rule = directoryRule(".".to_owned());
            rule.id = format!("local-{index}");
            rule
        })
        .collect();
    MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: localRules,
        },
        directory.path(),
    )
    .expect("2,000 条本地映射规则必须有效");
}

/// 验证 Map Local 与 Map Remote 的单字段恰好处于公开资源边界时仍能保存，避免上限产生非对称拒绝。
#[test]
fn acceptsMapFieldResourceLimits() {
    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let mut localRule = directoryRule("a".repeat(maximumLocalPathLength));
    localRule.responseHeaders = (0..maximumResponseHeaderCount)
        .map(|index| MapResponseHeader {
            name: format!("x-map-{index}"),
            value: "enabled".to_owned(),
        })
        .collect();
    localRule.contentTypeOverride = "a".repeat(maximumContentTypeOverrideLength);
    MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![localRule],
        },
        directory.path(),
    )
    .expect("Map Local 字段处于公开上限时必须有效");

    MapRemoteTool::new(MapRemoteConfiguration {
        enabled: true,
        rules: vec![remoteRule(
            "/*",
            MapRemoteTarget {
                path: format!("/{}", "a".repeat(maximumRemotePathLength - 1)),
                ..MapRemoteTarget::default()
            },
        )],
    })
    .expect("Map Remote 目标路径处于公开上限时必须有效");
}

/// 验证超额规则在建立去重集合前直接拒绝，避免无界配置先按规则数分配内存。
#[test]
fn rejectsMapRuleCountOverResourceLimit() {
    let remoteError = MapRemoteTool::new(MapRemoteConfiguration {
        enabled: true,
        rules: vec![remoteRule("/*", MapRemoteTarget::default()); maximumMapRuleCount + 1],
    })
    .err()
    .expect("超额远程规则必须被拒绝");
    assert_eq!(remoteError, MapToolError::RuleLimitExceeded);

    let directory = tempfile::tempdir().expect("临时映射根必须创建");
    let localError = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![directoryRule(".".to_owned()); maximumMapRuleCount + 1],
        },
        directory.path(),
    )
    .err()
    .expect("超额本地规则必须被拒绝");
    assert_eq!(localError, MapToolError::RuleLimitExceeded);
    assert_eq!(remoteError.code(), "mapRuleLimitExceeded");
    assert_eq!(remoteError.messageKey(), "error.map.ruleLimitExceeded");
}

/// 验证 Map Local 的路径、响应头数量和类型覆盖分别受公开表单的资源边界约束。
#[test]
fn rejectsMapLocalFieldsOverPublicResourceLimits() {
    let directory = tempfile::tempdir().expect("临时映射根必须创建");

    let localPathError = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![directoryRule("a".repeat(maximumLocalPathLength + 1))],
        },
        directory.path(),
    )
    .err()
    .expect("超长本地路径必须被拒绝");
    assert_eq!(localPathError, MapToolError::LocalPathTooLong);

    let mut tooManyHeadersRule = directoryRule(".".to_owned());
    tooManyHeadersRule.responseHeaders = (0..=maximumResponseHeaderCount)
        .map(|index| MapResponseHeader {
            name: format!("x-map-{index}"),
            value: "enabled".to_owned(),
        })
        .collect();
    let responseHeadersError = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![tooManyHeadersRule],
        },
        directory.path(),
    )
    .err()
    .expect("超额响应头必须被拒绝");
    assert_eq!(
        responseHeadersError,
        MapToolError::ResponseHeaderLimitExceeded
    );

    let mut contentTypeRule = directoryRule(".".to_owned());
    contentTypeRule.contentTypeOverride = "a".repeat(maximumContentTypeOverrideLength + 1);
    let contentTypeError = MapLocalTool::new(
        MapLocalConfiguration {
            enabled: true,
            rules: vec![contentTypeRule],
        },
        directory.path(),
    )
    .err()
    .expect("超长类型覆盖必须被拒绝");
    assert_eq!(contentTypeError, MapToolError::ContentTypeTooLong);
}

/// 验证 Map Remote 目标路径不超过公开协议的 2,048 字节，避免返回快照被前端拒绝。
#[test]
fn rejectsMapRemoteTargetPathOverPublicResourceLimit() {
    let error = MapRemoteTool::new(MapRemoteConfiguration {
        enabled: true,
        rules: vec![remoteRule(
            "/*",
            MapRemoteTarget {
                path: format!("/{}", "a".repeat(maximumRemotePathLength)),
                ..MapRemoteTarget::default()
            },
        )],
    })
    .err()
    .expect("超长目标路径必须被拒绝");

    assert_eq!(error, MapToolError::RemotePathTooLong);
    assert_eq!(error.code(), "mapRemotePathTooLong");
    assert_eq!(error.messageKey(), "error.map.remotePathTooLong");
}
