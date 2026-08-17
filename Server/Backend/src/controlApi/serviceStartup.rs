use std::sync::atomic::Ordering;

use super::{
    ApiError, ControlSnapshot, ControlState, accountRecovery::ConfigurationTransactionBarrier,
};

impl ControlState {
    /// 通知长连接结束，使 Axum 能先停止接收并完成控制连接排空，再关闭代理数据面。
    pub fn beginShutdown(&self) {
        self.shutdownSender.send_replace(true);
    }

    /// 串行启动数据面；与停止和配置重启共用操作锁，避免并发请求交错修改监听器生命周期。
    ///
    /// 运行上下文：公开控制 API、桌面运行时退出流程都经由此入口启动服务。
    /// 失败语义：当前状态不可启动或全部监听器绑定失败时返回结构化错误，配置保持不变。
    pub async fn startService(&self) -> Result<ControlSnapshot, ApiError> {
        // 运行意图先于操作锁发布，使正在执行故障恢复的监督任务能观察到最新用户决定。
        self.serviceRunIntent.store(true, Ordering::Release);
        let _operationGuard = self.serviceOperationLock.lock().await;
        let transactionBarrier = ConfigurationTransactionBarrier::activate(self);
        let proxyPort = self.configuration.read().await.listenPort;
        // PID 会随程序重启变化；每次显式启动都从已保存路径重建捕获集合，禁止复用上一次运行的陈旧 PID。
        *self.processCaptureConfiguration.write().await =
            self.processSelection.runtimeConfiguration(proxyPort);
        let startOutcome = self.startServiceExclusiveStaged().await;
        self.publishCurrentConfiguration().await;
        self.publishCurrentServiceState().await;
        drop(transactionBarrier);
        self.publishRuntimeViews().await;
        startOutcome?;
        Ok(self.snapshot().await)
    }

    /// 按持久化偏好启动代理数据面；仅由控制进程完成配置恢复后调用一次。
    ///
    /// 运行上下文：控制接口已成功绑定且统一配置已经加载，此时启动失败不能阻止用户进入设置页修正配置。
    /// 返回值：`true` 表示本次实际发起启动，`false` 表示用户关闭了自动启动。
    /// 失败语义：返回原始服务启动错误，调用方负责记录诊断，但控制接口继续运行。
    pub async fn startServiceIfConfigured(&self) -> Result<bool, ApiError> {
        if !self.startServiceOnLaunch.load(Ordering::Acquire) {
            return Ok(false);
        }
        self.startService().await?;
        Ok(true)
    }

    /// 串行停止融合代理数据面并强制关闭连接；停止状态重复调用保持幂等。
    ///
    /// 运行上下文：公开停止操作和配置替换前的强制断连均使用此入口。
    /// 失败语义：底层停止失败时保留 faulted 状态和监听器诊断，不继续执行后续配置替换。
    pub async fn stopService(&self) -> Result<ControlSnapshot, ApiError> {
        // 必须在等待生命周期锁前清除意图，防止已经持锁的账号服务恢复路径重新启动用户刚关闭的数据面。
        self.serviceRunIntent.store(false, Ordering::Release);
        let _operationGuard = self.serviceOperationLock.lock().await;
        self.stopServiceExclusive().await
    }

    /// 在代理数据面完全停止后关闭独立账号服务；进程退出路径必须按此顺序刷新最终租约和 SQLite。
    ///
    /// 运行上下文：仅由整个 `proxyService` 退出流程调用，普通数据面 stop 保持管理页面可用。
    /// 失败语义：有序关闭失败会返回结构化错误，监督器已强制回收超时的子进程。
    pub async fn stopAccountService(&self) -> Result<(), ApiError> {
        self.accountService.stop().await.map_err(|detail| {
            ApiError::internal(crate::localization::ErrorCode::ServiceStopFailed)
                .withParam("detail", detail)
        })
    }
}
