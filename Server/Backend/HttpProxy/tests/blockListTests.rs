#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use location_core::{LocationPattern, ResolvedLocation};

use http_proxy_core::tools::{
    BlockListConfiguration, BlockListDecision, BlockListTool, BlockMode, SyntheticBlockResponse,
};

const maximumResponseBodyBytes: usize = 64 * 1024;

/// 构造固定 HTTP Location，令规则判断测试只覆盖工具语义而不重复代理 URI 解析。
fn location(host: &str) -> ResolvedLocation {
    ResolvedLocation {
        protocol: "http".to_owned(),
        host: host.to_owned(),
        port: 80,
        path: "/api".to_owned(),
        query: String::new(),
        display: format!("http://{host}/api"),
    }
}

/// 验证黑名单命中后产生配置化合成响应，未命中位置继续流向后续流水线阶段。
#[test]
fn blockListBlocksMatchedLocation() {
    let tool = BlockListTool::new(BlockListConfiguration {
        mode: BlockMode::BlockList,
        locations: vec![LocationPattern {
            host: "ads.example.test".to_owned(),
            ..LocationPattern::default()
        }],
        statusCode: 451,
        responseBody: "blocked".to_owned(),
        closeConnection: true,
    })
    .expect("黑名单配置必须有效");
    assert_eq!(
        tool.onRequest(&location("ads.example.test"))
            .expect("匹配必须成功"),
        BlockListDecision::Block(SyntheticBlockResponse {
            statusCode: 451,
            responseBody: "blocked".to_owned(),
            closeConnection: true,
        })
    );
    assert_eq!(
        tool.onRequest(&location("api.example.test"))
            .expect("未命中位置必须可判定"),
        BlockListDecision::Continue
    );
}

/// 验证白名单空列表保持拒绝全部的稳定语义，避免配置丢失时意外放行请求。
#[test]
fn emptyAllowListBlocksAllLocations() {
    let tool = BlockListTool::new(BlockListConfiguration {
        mode: BlockMode::AllowList,
        ..BlockListConfiguration::default()
    })
    .expect("白名单配置必须有效");
    assert!(
        tool.onRequest(&location("api.example.test"))
            .expect("空白名单必须可判定")
            .isBlocked()
    );
}

/// 验证白名单命中位置会保留放行决定并留下工具命中痕迹，而不是被错误当作普通未命中。
#[test]
fn allowListAppliesToMatchedLocation() {
    let tool = BlockListTool::new(BlockListConfiguration {
        mode: BlockMode::AllowList,
        locations: vec![LocationPattern {
            host: "api.example.test".to_owned(),
            ..LocationPattern::default()
        }],
        ..BlockListConfiguration::default()
    })
    .expect("白名单配置必须有效");
    assert_eq!(
        tool.onRequest(&location("api.example.test"))
            .expect("白名单命中必须可判定"),
        BlockListDecision::Applied
    );
}

/// 验证热更新先校验再提交，失败配置不得覆盖正在工作的规则集合。
#[test]
fn rejectedReplacementKeepsPreviousConfiguration() {
    let tool = BlockListTool::default();
    let result = tool.replaceConfiguration(BlockListConfiguration {
        statusCode: 600,
        ..BlockListConfiguration::default()
    });
    assert!(result.is_err());
    assert_eq!(tool.configuration(), BlockListConfiguration::default());
}

/// 验证合成响应正文以字节为单位受 64 KiB 边界约束，避免多字节文本绕过内存上限。
#[test]
fn rejectsResponseBodyOver64KiB() {
    BlockListTool::new(BlockListConfiguration {
        responseBody: "a".repeat(maximumResponseBodyBytes),
        ..BlockListConfiguration::default()
    })
    .expect("恰好 64 KiB 的合成正文必须有效");

    let oversized = "界".repeat((64 * 1024) / "界".len() + 1);
    let error = BlockListTool::new(BlockListConfiguration {
        responseBody: oversized,
        ..BlockListConfiguration::default()
    })
    .err()
    .expect("超过 64 KiB 的合成正文必须被拒绝");

    assert_eq!(
        error,
        http_proxy_core::tools::ToolError::BlockResponseBodyTooLarge
    );
    assert_eq!(error.code(), "toolBlockResponseBodyTooLarge");
    assert_eq!(error.messageKey(), "error.tools.blockResponseBodyTooLarge");
}
