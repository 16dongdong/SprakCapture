use super::*;

/// 生产默认关闭远程入口但保留明确监听配置；开发 Vite 不依赖该开关。
#[test]
fn defaultsToDisabledRemoteRemotePort() {
    let configuration = MultiAccountConfiguration::default();
    assert!(!configuration.enabled);
    assert_eq!(configuration.remoteHost, "0.0.0.0");
    assert_eq!(configuration.remotePort, 19_090);
}

/// 公共监听允许网卡地址，但端口零会使配置替换在启动子进程前失败。
#[test]
fn validatesManagementAddressBeforeSpawn() {
    let mut configuration = MultiAccountConfiguration::default();
    assert_eq!(
        configuration.publicAddress().expect("默认配置有效"),
        "0.0.0.0:19090".parse::<SocketAddr>().expect("固定地址有效")
    );
    configuration.remotePort = 0;
    assert_eq!(
        configuration.publicAddress().expect_err("零端口必须拒绝"),
        "远程管理端口不能为 0"
    );
}

/// 关闭远程管理时允许系统分配实际端口，保证内部账号服务仍能为桌面映射和 SOCKS 校验运行。
#[test]
fn acceptsSystemAssignedPublicPortForInternalService() {
    let response = StartupResponse {
        publicAddress: "127.0.0.1:43127".parse().expect("测试公开端点有效"),
        internalAddress: "127.0.0.1:43128".parse().expect("测试内部端点有效"),
        serviceInstanceId: "instance".to_owned(),
    };
    assert!(isStartupEndpointValid(
        "127.0.0.1:0".parse().expect("随机端口请求有效"),
        &response
    ));
    assert!(!isStartupEndpointValid(
        "127.0.0.1:19090".parse().expect("固定端口请求有效"),
        &response
    ));
}

/// 内部令牌每次生成都具有 256 位随机源且 Base64URL 结果不会暴露填充字符。
#[test]
fn generatesDistinctProcessLocalTokens() {
    let first = randomInternalToken();
    let second = randomInternalToken();
    assert_eq!(first.len(), 43);
    assert_eq!(second.len(), 43);
    assert_ne!(first, second);
    assert!(!first.contains('='));
}

/// 实时摘要拒绝额外字段，防止累计流量、账号或连接标识误入主控制面长期快照。
#[test]
fn summaryRejectsNonOverviewFields() {
    let valid = serde_json::json!({
        "onlineAccounts": 2,
        "activeConnections": 3,
        "uploadBytesPerSecond": 1024,
        "downloadBytesPerSecond": 2048
    });
    assert!(serde_json::from_value::<MultiAccountSummary>(valid.clone()).is_ok());
    let mut invalid = valid.as_object().expect("摘要对象").clone();
    invalid.insert("uploadedBytes".to_owned(), serde_json::json!(4096));
    assert!(serde_json::from_value::<MultiAccountSummary>(invalid.into()).is_err());
}
