use std::{
    future::Future,
    sync::{Arc, Mutex},
};

use tokio_util::task::TaskTracker;

/// 保存任务句柄和强制中止状态，使任务创建与停止切换具有单一同步边界。
struct ProxyTaskState {
    aborting: bool,
    abortHandles: Vec<tokio::task::AbortHandle>,
}

/// 统一跟踪 HTTP 派生任务及其终止句柄，保证优雅排空超时后仍能强制释放连接任务。
///
/// 运行上下文：所有 CONNECT、TLS 与响应流任务通过该对象启动。正常停机先关闭并等待，超时路径调用
/// `abortAllAndWait`。失败语义：互斥锁中毒说明运行时内部状态已损坏，因此直接终止而不伪造成功。
#[derive(Clone)]
pub(crate) struct ProxyTaskTracker {
    tracker: TaskTracker,
    state: Arc<Mutex<ProxyTaskState>>,
}

impl ProxyTaskTracker {
    /// 创建空任务集合；任务句柄只属于当前代理实例，不跨监听器共享。
    pub(crate) fn new() -> Self {
        Self {
            tracker: TaskTracker::new(),
            state: Arc::new(Mutex::new(ProxyTaskState {
                aborting: false,
                abortHandles: Vec::new(),
            })),
        }
    }

    /// 启动并跟踪任务；强制中止开始后直接丢弃 future，避免新任务逃过已完成的句柄扫描。
    ///
    /// 运行上下文：任务创建与 `abortAll` 使用同一状态锁，锁内操作不等待 I/O，因此不会把异步阻塞带入同步临界区。
    pub(crate) fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut state = self.state.lock().expect("代理任务状态锁不得中毒");
        if state.aborting {
            return;
        }
        state
            .abortHandles
            .retain(|abortHandle| !abortHandle.is_finished());
        let joinHandle = self.tracker.spawn(task);
        state.abortHandles.push(joinHandle.abort_handle());
    }

    /// 关闭任务集合；调用方随后必须等待 `wait`，或在超时时调用 `abortAllAndWait` 完成强制析构。
    pub(crate) fn close(&self) {
        self.tracker.close();
    }

    /// 等待全部已跟踪任务结束；只有任务集合关闭且为空时才返回。
    pub(crate) async fn wait(&self) {
        self.tracker.wait().await;
        self.state
            .lock()
            .expect("代理任务状态锁不得中毒")
            .abortHandles
            .clear();
    }

    /// 原子切换到强制中止状态并终止当前任务；返回后任何晚到的 spawn 都只会丢弃 future。
    fn abortAll(&self) {
        let mut state = self.state.lock().expect("代理任务状态锁不得中毒");
        state.aborting = true;
        self.tracker.close();
        for abortHandle in state.abortHandles.drain(..) {
            abortHandle.abort();
        }
    }

    /// 先给已收到取消信号的任务有限调度机会提交取消终态，再中止残留 future 并等待析构。
    ///
    /// 运行上下文：调用方必须先取消数据面令牌。有限轮次只让已就绪的录制状态更新完成，不等待
    /// 任何网络 I/O、客户端关闭或业务超时；仍未退出的任务随后被同步 abort。
    pub(crate) async fn abortAllAndWait(&self) {
        self.tracker.close();
        for _ in 0..8 {
            if self.tracker.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        // 即使任务已经自行退出也切换为 aborting，阻止持有旧上下文的晚到 spawn 逃出生命周期。
        self.abortAll();
        self.tracker.wait().await;
        self.state
            .lock()
            .expect("代理任务状态锁不得中毒")
            .abortHandles
            .clear();
    }
}
