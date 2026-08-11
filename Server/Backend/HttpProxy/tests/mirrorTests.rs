#![allow(non_snake_case)]

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri, Version};
use http_proxy_core::{
    PipelineContext, PipelineRequestOutcome, RequestDraft, ResponseDraft, ToolPipeline,
    tools::{MirrorConfiguration, MirrorLayout, MirrorOverflowPolicy, MirrorTool},
};
use location_core::ResolvedLocation;

/// 构造可直接经过镜像流水线的请求上下文，测试只依赖临时目录和内存正文，不启动监听器。
fn mirrorContext() -> PipelineContext {
    PipelineContext::new(
        "127.0.0.1:38421".to_owned(),
        ResolvedLocation {
            protocol: "http".to_owned(),
            host: "mirror.example.test".to_owned(),
            port: 80,
            path: "/assets/logo.txt".to_owned(),
            query: String::new(),
            display: "http://mirror.example.test/assets/logo.txt".to_owned(),
        },
        RequestDraft {
            method: Method::POST,
            uri: "http://mirror.example.test/assets/logo.txt"
                .parse::<Uri>()
                .expect("测试 URI 必须有效"),
            version: Version::HTTP_11,
            headers: HeaderMap::new(),
            body: Some(Bytes::from_static(b"request-body")),
        },
    )
}

/// 验证层级镜像同时写入请求和响应并保留可观察计数；写入只发生在异步队列排空后。
#[tokio::test]
async fn mirrorWritesHierarchicalRequestAndResponse() {
    let directory = tempfile::tempdir().expect("临时镜像目录必须可创建");
    let mirror = Arc::new(
        MirrorTool::new(MirrorConfiguration {
            enabled: true,
            rootDirectory: directory.path().to_string_lossy().into_owned(),
            locations: Vec::new(),
            mirrorRequest: true,
            mirrorResponse: true,
            layout: MirrorLayout::Hierarchical,
            onOverflow: MirrorOverflowPolicy::Drop,
            maxQueueLength: 8,
        })
        .expect("镜像配置必须有效"),
    );
    let pipeline = ToolPipeline::new();
    pipeline
        .register(mirror.clone())
        .expect("镜像工具必须可注册");
    let mut context = mirrorContext();

    assert_eq!(
        pipeline
            .runRequest(&mut context)
            .await
            .expect("镜像请求阶段必须成功"),
        PipelineRequestOutcome::Forward
    );
    context.response = Some(ResponseDraft {
        status: StatusCode::OK,
        version: Version::HTTP_11,
        headers: HeaderMap::new(),
        body: Some(Bytes::from_static(b"response-body")),
    });
    pipeline
        .runResponse(&mut context)
        .await
        .expect("镜像响应阶段必须成功");
    mirror
        .flush(Duration::from_secs(2))
        .await
        .expect("镜像队列必须在限定时间内排空");

    let state = mirror.publicState();
    assert_eq!(state.writtenFiles, 2);
    assert_eq!(state.droppedWrites, 0);
    let written = std::fs::read_dir(directory.path().join("mirror.example.test"))
        .expect("层级主机目录必须存在")
        .count();
    assert!(written >= 1, "主机目录必须包含镜像文件或路径子目录");
}
