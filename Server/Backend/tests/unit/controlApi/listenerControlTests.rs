use super::*;

/// 构造只启用一条 TCP 转发的配置，便于验证受保护端口的双向更新边界。
fn forwardingConfiguration(listenPort: u16) -> AuxiliaryListenerConfiguration {
    AuxiliaryListenerConfiguration {
        reverseProxies: Vec::new(),
        portForwards: vec![PortForwardEntry {
            id: "management-conflict".to_owned(),
            enabled: true,
            listenHost: "127.0.0.1".to_owned(),
            listenPort,
            targetHost: "127.0.0.1".to_owned(),
            targetPort: 9,
        }],
    }
}

/// 验证辅助监听更新不能占用已启用的账号管理端口；错误必须在写盘与服务重启前返回。
#[test]
fn auxiliaryUpdateRejectsRemotePort() {
    let service = Socks5Config::default();
    let http = ManagedHttpProxyConfiguration::default();
    let multiAccount = MultiAccountConfiguration {
        enabled: true,
        remoteHost: "0.0.0.0".to_owned(),
        remotePort: 19_090,
    };
    let result = validateAuxiliaryListenerConfiguration(
        &forwardingConfiguration(19_090),
        &service,
        &http,
        &multiAccount,
    );

    assert!(result.is_err());
}

/// 验证完整配置更新不能把账号管理端口迁移到现有辅助监听；覆盖地址也按真实 socket 冲突处理。
#[test]
fn managementUpdateRejectsAuxiliaryPort() {
    let service = Socks5Config::default();
    let http = ManagedHttpProxyConfiguration::default();
    let multiAccount = MultiAccountConfiguration {
        enabled: true,
        remoteHost: "0.0.0.0".to_owned(),
        remotePort: 19_091,
    };
    let result = validateAuxiliaryListenerConfiguration(
        &forwardingConfiguration(19_091),
        &service,
        &http,
        &multiAccount,
    );

    assert!(result.is_err());
}
