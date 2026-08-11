use std::sync::atomic::{AtomicBool, Ordering};

use crate::proxyService::{ProxyServiceError, ProxyServiceSupervisor};

/// 保存 Desktop 独占的退出状态与代理进程管理器，供窗口、托盘和运行循环共享。
pub struct DesktopState {
    exitRequested: AtomicBool,
    proxyServiceSupervisor: ProxyServiceSupervisor,
}

impl DesktopState {
    /// 创建桌面状态；此时仅保证监督器线程已经建立，子进程首启失败会留在线程内按间隔重试。
    pub const fn new(proxyServiceSupervisor: ProxyServiceSupervisor) -> Self {
        Self {
            exitRequested: AtomicBool::new(false),
            proxyServiceSupervisor,
        }
    }

    /// 原子标记显式退出；返回是否为首个退出请求，以便多个事件源只执行一次回收。
    pub fn beginExit(&self) -> bool {
        !self.exitRequested.swap(true, Ordering::AcqRel)
    }

    /// 判断当前关闭事件是否属于显式退出；普通窗口关闭必须转为隐藏以维持后台服务。
    pub fn isExitRequested(&self) -> bool {
        self.exitRequested.load(Ordering::Acquire)
    }

    /// 停止并回收代理服务；底层操作幂等，允许退出请求与运行循环共同调用。
    pub fn stopProxyService(&self) -> Result<(), ProxyServiceError> {
        self.proxyServiceSupervisor.stop()
    }
}
