use super::*;

/// 两个连接引用同一账号控制器时，第二次预留必须排在第一次完整字节预算之后。
#[test]
fn sharesDirectionalScheduleAcrossConnections() {
    let controller = Arc::new(AccountTrafficController {
        upload: Mutex::new(DirectionSchedule::new(1_000)),
        download: Mutex::new(DirectionSchedule::new(-1)),
        policyRevision: Mutex::new(1),
        activeLeases: AtomicU64::new(2),
    });
    let firstConnection = controller.clone();
    let secondConnection = controller;
    let first = firstConnection
        .upload
        .lock()
        .reserve(500)
        .expect("有限速率必须返回排期");
    let second = secondConnection
        .upload
        .lock()
        .reserve(500)
        .expect("有限速率必须返回排期");
    assert!(second.duration_since(first) >= Duration::from_millis(500));
}

/// 重复心跳的同修订策略不得清空已有时间债务，只有新修订才能替换排期。
#[test]
fn preservesScheduleUntilPolicyRevisionChanges() {
    let response = AuthenticationResponse {
        serviceInstanceId: "instance".to_owned(),
        accountId: "account".to_owned(),
        leaseId: "lease".to_owned(),
        username: "user".to_owned(),
        policyRevision: 3,
        maxUploadBytesPerSecond: 1_000,
        maxDownloadBytesPerSecond: 1_000,
    };
    let controller = AccountTrafficController::new(&response);
    let first = controller
        .upload
        .lock()
        .reserve(1_000)
        .expect("有限速率必须返回排期");
    controller.update(3, 1_000, 1_000);
    let preserved = controller
        .upload
        .lock()
        .reserve(1)
        .expect("有限速率必须返回排期");
    assert!(preserved.duration_since(first) >= Duration::from_secs(1));
    controller.update(4, -1, -1);
    assert!(controller.upload.lock().reserve(1).is_none());
}

/// 首个大包只能使用一秒突发信用，超过一秒额度的剩余部分必须等待后再整体交付。
#[test]
fn largeInitialReservationWaitsBeyondOneSecondCredit() {
    let mut schedule = DirectionSchedule::new(1_000);
    let now = Instant::now();
    let scheduled = schedule.reserve(3_000).expect("有限速率必须返回排期");
    assert!(scheduled.duration_since(now) >= Duration::from_millis(1_990));
}
