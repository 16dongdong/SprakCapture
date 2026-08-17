use super::*;

/// 验证等待生命周期锁期间的配置提交会使旧健康检查结果失效。
#[test]
fn staleGenerationCannotRecoverAccountService() {
    let observed = MultiAccountConfiguration::default();
    let mut current = observed.clone();
    current.remotePort += 1;
    assert!(!accountRecoveryMatchesCurrentConfiguration(
        4, 5, &observed, &current
    ));
}

/// 验证关闭远程监听后账号数据库服务仍参与恢复；配置相同且代际稳定时必须继续守护回环实例。
#[test]
fn disabledRemoteConfigurationStillRecoversInternalAccountService() {
    let observed = MultiAccountConfiguration::default();
    let current = observed.clone();
    assert!(accountRecoveryMatchesCurrentConfiguration(
        8, 8, &observed, &current
    ));
}

/// 验证显式停止意图阻止故障恢复重新启动代理。
#[test]
fn explicitStopIntentCancelsPendingProxyRestart() {
    assert!(!accountRecoveryMayRestartProxy(false, 12, 12));
    assert!(!accountRecoveryMayRestartProxy(true, 12, 13));
    assert!(accountRecoveryMayRestartProxy(true, 12, 12));
}

/// 验证辅助监听器成功不能掩盖融合 SOCKS 主数据面失败。
#[test]
fn auxiliaryListenerCannotCommitMissingPrimaryDataPlane() {
    assert!(!primaryDataPlaneCommitSucceeded(false, true));
    assert!(primaryDataPlaneCommitSucceeded(true, false));
}

/// 验证故障状态仍可进入资源回收，避免候选辅助监听阻塞配置回滚。
#[test]
fn faultedServiceCanReleaseCandidateResources() {
    assert!(serviceStateCanEnterStop(ServiceState::Faulted));
    assert!(serviceStateCanEnterStop(ServiceState::Running));
    assert!(!serviceStateCanEnterStop(ServiceState::Starting));
}

/// 验证账号服务恢复后的代理重试使用有界指数退避，持续失败不会形成忙循环。
#[test]
fn accountRecoveryRetryDelayIsBounded() {
    assert_eq!(
        nextAccountRecoveryDelay(Duration::from_secs(1)),
        Duration::from_secs(2)
    );
    assert_eq!(
        nextAccountRecoveryDelay(Duration::from_secs(20)),
        Duration::from_secs(30)
    );
    assert_eq!(
        nextAccountRecoveryDelay(Duration::from_secs(30)),
        Duration::from_secs(30)
    );
}

/// 验证恢复错误会与原事务失败一起返回，调用方不会误判旧运行态已经完整恢复。
#[test]
fn configurationRecoveryErrorsArePreserved() {
    let error = mergeConfigurationTransactionError(
        ApiError::internal(ErrorCode::ConfigurationPersistenceFailed),
        vec![
            "恢复配置文件失败".to_owned(),
            "恢复代理数据面失败".to_owned(),
        ],
    );
    assert_eq!(error.code, ErrorCode::ConfigurationPersistenceFailed);
    let message = error.message();
    assert!(message.contains("恢复配置文件失败"));
    assert!(message.contains("恢复代理数据面失败"));
}
