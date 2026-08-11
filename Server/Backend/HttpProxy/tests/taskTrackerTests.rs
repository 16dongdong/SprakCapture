#![allow(non_snake_case)]

#[path = "../src/taskTracker.rs"]
#[allow(dead_code)]
mod taskTracker;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use taskTracker::ProxyTaskTracker;

/// future 被丢弃时记录析构完成，使测试能够区分“已请求中止”和“资源已经释放”。
struct TaskDropMarker(Arc<AtomicBool>);

impl Drop for TaskDropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// 强制中止必须等待现有任务析构，并拒绝在中止切换之后到达的新任务。
#[tokio::test]
async fn abortWaitsForTrackedTasksAndRejectsLateSpawns() {
    let tracker = ProxyTaskTracker::new();
    let runningTaskDropped = Arc::new(AtomicBool::new(false));
    let runningMarker = TaskDropMarker(runningTaskDropped.clone());
    tracker.spawn(async move {
        let _runningMarker = runningMarker;
        std::future::pending::<()>().await;
    });

    tracker.abortAllAndWait().await;
    assert!(runningTaskDropped.load(Ordering::SeqCst));

    let lateTaskDropped = Arc::new(AtomicBool::new(false));
    let lateMarker = TaskDropMarker(lateTaskDropped.clone());
    tracker.spawn(async move {
        let _lateMarker = lateMarker;
        std::future::pending::<()>().await;
    });
    assert!(lateTaskDropped.load(Ordering::SeqCst));
}
