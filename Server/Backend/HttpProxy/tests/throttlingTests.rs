#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::time::{Duration, Instant};

use http::{HeaderMap, Method, Uri, Version};
use location_core::{LocationPattern, ResolvedLocation};

use http_proxy_core::{
    PipelineContext, PipelineDirective, PipelineTool, RequestDraft, ResponseDraft, ToolRegistration,
};

use http_proxy_core::tools::{
    ThrottleChunkAction, ThrottleDirection, ThrottlePacer, ThrottlePlan, ThrottlePreset,
    ThrottleProfile, ThrottlingConfiguration, ThrottlingError, ThrottlingTool,
    builtInThrottlePresets,
};

const builtInPresetCount: usize = 3;
const maximumPublicPresetCount: usize = 64;
const maximumUserPresetCount: usize = maximumPublicPresetCount - builtInPresetCount;
const maximumSafeJavaScriptInteger: u64 = 9_007_199_254_740_991;

/// 构造固定位置测试输入，使规则覆盖不依赖真实网络监听器。
fn location(host: &str) -> ResolvedLocation {
    ResolvedLocation {
        protocol: "http".to_owned(),
        host: host.to_owned(),
        port: 80,
        path: "/asset.bin".to_owned(),
        query: String::new(),
        display: format!("http://{host}/asset.bin"),
    }
}

/// 构造最小流水线上下文，验证节流计划在两个阶段的传递边界。
fn context(host: &str) -> PipelineContext {
    PipelineContext::new(
        "127.0.0.1:9000".to_owned(),
        location(host),
        RequestDraft {
            method: Method::GET,
            uri: "http://scope.test/asset.bin"
                .parse::<Uri>()
                .expect("valid URI"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: None,
        },
    )
}

/// 构造低速小 MTU 测试配置，以便稳定观察令牌桶分块与等待行为。
fn lowRateProfile() -> ThrottleProfile {
    ThrottleProfile {
        downloadBytesPerSecond: 128,
        uploadBytesPerSecond: 128,
        latencyMilliseconds: 0,
        latencyJitterMilliseconds: 0,
        reliabilityPercent: 100,
        mtu: 64,
    }
}

/// 构造唯一且有效的用户预设，供公开快照数量边界测试复用。
fn userPreset(index: usize) -> ThrottlePreset {
    let profile = ThrottleProfile::default();
    ThrottlePreset {
        id: format!("user-{index}"),
        name: format!("用户预设 {index}"),
        downloadBytesPerSecond: profile.downloadBytesPerSecond,
        uploadBytesPerSecond: profile.uploadBytesPerSecond,
        latencyMilliseconds: profile.latencyMilliseconds,
        latencyJitterMilliseconds: profile.latencyJitterMilliseconds,
        reliabilityPercent: profile.reliabilityPercent,
        mtu: profile.mtu,
    }
}

#[test]
/// 验证预设解析和计划快照不会被后续热更新反向修改。
fn planUsesSelectedPresetAndSnapshot() {
    let tool = ThrottlingTool::new(ThrottlingConfiguration {
        enabled: true,
        activePresetId: Some("3g".to_owned()),
        ..ThrottlingConfiguration::default()
    })
    .expect("built-in preset is valid");
    let plan = tool
        .planFor(&location("api.test"))
        .expect("location resolves")
        .expect("enabled tool creates plan");
    assert_eq!(plan.profile().downloadBytesPerSecond, 400 * 1024);

    tool.updateConfiguration(ThrottlingConfiguration::default())
        .expect("disabled configuration is valid");
    assert_eq!(plan.profile().downloadBytesPerSecond, 400 * 1024);
    assert!(
        tool.planFor(&location("api.test"))
            .expect("disabled location resolves")
            .is_none()
    );
}

#[test]
/// 验证位置范围只影响命中目标，避免对无关主机施加节流。
fn planSkipsUnmatchedLocation() {
    let tool = ThrottlingTool::new(ThrottlingConfiguration {
        enabled: true,
        locations: vec![LocationPattern {
            host: "slow.test".to_owned(),
            ..LocationPattern::default()
        }],
        ..ThrottlingConfiguration::default()
    })
    .expect("scope is valid");
    assert!(
        tool.planFor(&location("fast.test"))
            .expect("unmatched location resolves")
            .is_none()
    );
    assert!(
        tool.planFor(&location("slow.test"))
            .expect("matched location resolves")
            .is_some()
    );
}

#[tokio::test]
/// 验证工具在请求和响应阶段均产生可观察事务标志与计划。
async fn pipelineAdapterMarksBothPhases() {
    let tool = ThrottlingTool::new(ThrottlingConfiguration {
        enabled: true,
        ..ThrottlingConfiguration::default()
    })
    .expect("default profile is valid");
    let mut context = context("scope.test");
    assert!(matches!(
        tool.onRequest(&mut context).await.expect("request hook"),
        PipelineDirective::Applied
    ));
    assert!(context.flags.throttled);
    context.response = Some(ResponseDraft {
        status: http::StatusCode::OK,
        version: Version::HTTP_11,
        headers: HeaderMap::new(),
        body: None,
    });
    assert!(matches!(
        tool.onResponse(&mut context).await.expect("response hook"),
        PipelineDirective::Applied
    ));
    assert_eq!(
        tool.registration(),
        ToolRegistration::new(
            http_proxy_core::ToolId::Throttling,
            vec![
                http_proxy_core::ToolPhase::Request,
                http_proxy_core::ToolPhase::Response
            ],
            true,
        )
    );
}

#[tokio::test]
/// 验证调度器按 MTU 拆分并在桶耗尽后等待令牌补充。
async fn pacerSplitsFramesAndWaitsForTokens() {
    let plan = ThrottlePlan::new(lowRateProfile()).expect("profile is valid");
    let mut pacer = plan
        .createPacerWithSeed(ThrottleDirection::Download, 1)
        .expect("pacer is valid");
    let first = pacer
        .nextChunk(128)
        .await
        .expect("first chunk")
        .expect("first chunk exists");
    assert_eq!(first.byteCount, 64);
    assert_eq!(first.action, ThrottleChunkAction::Forward);
    let startedAt = Instant::now();
    let second = pacer
        .nextChunk(64)
        .await
        .expect("second chunk")
        .expect("second chunk exists");
    assert_eq!(second.byteCount, 64);
    assert!(startedAt.elapsed() >= Duration::from_millis(350));
}

#[tokio::test]
/// 验证可靠性为零时将分块决策显式暴露为丢弃。
async fn pacerExposesReliabilityDecision() {
    let mut profile = lowRateProfile();
    profile.reliabilityPercent = 0;
    let mut pacer =
        ThrottlePacer::newWithSeed(profile, ThrottleDirection::Upload, 7).expect("pacer is valid");
    assert_eq!(
        pacer
            .nextChunk(64)
            .await
            .expect("chunk")
            .expect("chunk exists")
            .action,
        ThrottleChunkAction::Drop
    );
}

#[test]
/// 验证无效速率、可靠性和未知预设在写入前被拒绝。
fn rejectsInvalidConfiguration() {
    let invalidRate = ThrottlingConfiguration {
        custom: ThrottleProfile {
            uploadBytesPerSecond: 0,
            ..ThrottleProfile::default()
        },
        ..ThrottlingConfiguration::default()
    };
    assert!(ThrottlingTool::new(invalidRate).is_err());
    let invalidReliability = ThrottlingConfiguration {
        custom: ThrottleProfile {
            reliabilityPercent: 101,
            ..ThrottleProfile::default()
        },
        ..ThrottlingConfiguration::default()
    };
    assert!(ThrottlingTool::new(invalidReliability).is_err());
    let unknownPreset = ThrottlingConfiguration {
        activePresetId: Some("missing".to_owned()),
        ..ThrottlingConfiguration::default()
    };
    assert!(ThrottlingTool::new(unknownPreset).is_err());
}

/// 验证上传与下载速率均不超过 JavaScript 安全整数，防止公开 JSON 快照改变数值语义。
#[test]
fn constrainsRatesToJavaScriptSafeInteger() {
    ThrottleProfile {
        downloadBytesPerSecond: maximumSafeJavaScriptInteger,
        uploadBytesPerSecond: maximumSafeJavaScriptInteger,
        ..ThrottleProfile::default()
    }
    .validate()
    .expect("JavaScript 安全整数上限必须有效");

    for profile in [
        ThrottleProfile {
            downloadBytesPerSecond: maximumSafeJavaScriptInteger + 1,
            ..ThrottleProfile::default()
        },
        ThrottleProfile {
            uploadBytesPerSecond: maximumSafeJavaScriptInteger + 1,
            ..ThrottleProfile::default()
        },
    ] {
        assert_eq!(profile.validate(), Err(ThrottlingError::InvalidRate));
    }
}

/// 验证公开快照固定为 3 个内置预设加至多 61 个用户预设，恰好 64 项时仍可被前端协议接受。
#[test]
fn constrainsUserPresetsToPublicSnapshotLimit() {
    assert_eq!(builtInThrottlePresets().len(), builtInPresetCount);

    let atLimit = ThrottlingTool::new(ThrottlingConfiguration {
        userPresets: (0..maximumUserPresetCount).map(userPreset).collect(),
        ..ThrottlingConfiguration::default()
    })
    .expect("61 个用户预设必须有效");
    assert_eq!(
        atLimit.publicState().presets.len(),
        maximumPublicPresetCount
    );

    let error = ThrottlingTool::new(ThrottlingConfiguration {
        userPresets: (0..=maximumUserPresetCount).map(userPreset).collect(),
        ..ThrottlingConfiguration::default()
    })
    .err()
    .expect("第 62 个用户预设必须被拒绝");
    assert_eq!(error, ThrottlingError::TooManyUserPresets);
    assert_eq!(error.code(), "throttlingTooManyUserPresets");
    assert_eq!(error.messageKey(), "error.throttling.tooManyUserPresets");
}
