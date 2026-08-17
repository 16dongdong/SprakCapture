use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::{
    closePreference::{ClosePreferenceError, ClosePreferenceStore, MainWindowCloseAction},
    proxyService::{ProxyServiceError, ProxyServiceSupervisor},
};

const closePromptIdle: u8 = 0;
const closePromptOpen: u8 = 1;
const closePromptResolving: u8 = 2;

/// 保存 Desktop 独占的退出状态与代理进程管理器，供窗口、托盘和运行循环共享。
pub struct DesktopState {
    exitRequested: AtomicBool,
    closePromptPhase: AtomicU8,
    closePreferenceStore: ClosePreferenceStore,
    proxyServiceSupervisor: ProxyServiceSupervisor,
}

impl DesktopState {
    /// 创建桌面状态；此时仅保证监督器线程已经建立，子进程首启失败会留在线程内按间隔重试。
    pub const fn new(
        proxyServiceSupervisor: ProxyServiceSupervisor,
        closePreferenceStore: ClosePreferenceStore,
    ) -> Self {
        Self {
            exitRequested: AtomicBool::new(false),
            closePromptPhase: AtomicU8::new(closePromptIdle),
            closePreferenceStore,
            proxyServiceSupervisor,
        }
    }

    /// 尝试打开自绘关闭询问；重复系统关闭事件只保留一个待处理请求。
    pub fn beginMainWindowClosePrompt(&self) -> bool {
        self.closePromptPhase
            .compare_exchange(
                closePromptIdle,
                closePromptOpen,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// 返回当前是否存在待展示的关闭询问；WebView 首次挂载用它补偿事件监听注册竞态。
    pub fn isMainWindowClosePromptOpen(&self) -> bool {
        self.closePromptPhase.load(Ordering::Acquire) == closePromptOpen
    }

    /// 独占关闭选择提交；过期或重复提交返回 `false`，禁止执行第二次窗口生命周期动作。
    pub fn beginMainWindowCloseResolution(&self) -> bool {
        self.closePromptPhase
            .compare_exchange(
                closePromptOpen,
                closePromptResolving,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// 原子取消尚未提交的关闭询问；若确认已经开始则返回 `false`，避免取消覆盖执行中的生命周期动作。
    pub fn cancelMainWindowClosePrompt(&self) -> bool {
        self.closePromptPhase
            .compare_exchange(
                closePromptOpen,
                closePromptIdle,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// 关闭动作失败时重新开放同一询问，使界面保留错误并允许再次确认。
    pub fn reopenMainWindowClosePrompt(&self) {
        self.closePromptPhase
            .store(closePromptOpen, Ordering::Release);
    }

    /// 结束本次关闭询问；取消和成功动作都恢复为空闲状态。
    pub fn finishMainWindowClosePrompt(&self) {
        self.closePromptPhase
            .store(closePromptIdle, Ordering::Release);
    }

    /// 返回用户已记住的主窗口关闭动作；未记住时由关闭事件显示首次选择对话框。
    pub fn rememberedMainWindowCloseAction(
        &self,
    ) -> Result<Option<MainWindowCloseAction>, ClosePreferenceError> {
        self.closePreferenceStore.rememberedAction()
    }

    /// 持久化用户勾选“记住”时的动作；失败时不更新内存，使下一次关闭仍会询问。
    pub fn rememberMainWindowCloseAction(
        &self,
        action: MainWindowCloseAction,
    ) -> Result<(), ClosePreferenceError> {
        self.closePreferenceStore.remember(action)
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
