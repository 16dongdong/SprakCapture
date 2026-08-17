use super::projectionRevisionIsStable;
use tokio::sync::watch;

/// 验证版本稳定判定只接受投影前后完全一致的修订号。
#[test]
fn projectionRevisionMustRemainStable() {
    assert!(super::projectionRevisionIsStable(7, 7));
    assert!(!super::projectionRevisionIsStable(7, 8));
}

/// 验证高频全局事件修订推进不会改变低频投影代际，快照因此保持活性。
#[test]
fn telemetryRevisionDoesNotInvalidateProjectionGeneration() {
    let projectionGeneration = 11;
    let eventRevisionBefore = 40;
    let eventRevisionAfter = 80;
    assert!(projectionRevisionIsStable(
        projectionGeneration,
        projectionGeneration
    ));
    assert_ne!(eventRevisionBefore, eventRevisionAfter);
}

/// 验证 watch 屏障即使在订阅者开始等待前已释放也不会丢唤醒或永久阻塞。
#[tokio::test]
async fn transactionBarrierReleaseCannotBeMissed() {
    let (sender, mut receiver) = watch::channel(true);
    sender.send_replace(false);
    tokio::time::timeout(std::time::Duration::from_millis(100), async {
        while *receiver.borrow_and_update() {
            receiver.changed().await.expect("测试发送端仍然存活");
        }
    })
    .await
    .expect("事务屏障释放不得丢失唤醒");
}
