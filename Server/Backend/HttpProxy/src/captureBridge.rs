use capture_core::{
    BeginTransaction, BodyWrite, CaptureError, HeaderField, MessageSide, RecordingSession,
    TransactionCompletion, TransactionFlags, TransactionProgressUpdate, TransactionUpdate,
    TransactionUserUpdate, currentTimeMilliseconds,
};
use location_core::ResolvedLocation;

use crate::error::RequestFailure;

/// 绑定一个 capture-core 事务；None 由调用方表示暂停或 Location 忽略。
#[derive(Clone)]
pub(crate) struct CaptureTransaction {
    session: RecordingSession,
    transactionId: String,
    recordingSessionId: String,
    host: String,
}

impl CaptureTransaction {
    /// 使用代理层已解析的不可变输入创建事务；录制暂停或规则忽略时返回 None。
    pub(crate) async fn begin(
        session: &RecordingSession,
        input: BeginTransaction,
    ) -> Result<Option<Self>, CaptureError> {
        let host = input.location.host.clone();
        // 会话 ID 在 clear 时不变，先读取快照可避免并发 clear 后再查询导致创建路径被误判失败。
        let recordingSessionId = session.snapshot().await?.recordingSessionId;
        let transactionId = session.beginTransaction(input).await?;
        Ok(transactionId.map(|transactionId| Self {
            session: session.clone(),
            transactionId,
            recordingSessionId,
            host,
        }))
    }

    /// 返回 capture-core 已分配的事务 ID，供工具流水线把异步断点和事务痕迹关联到同一记录。
    pub(crate) fn transactionId(&self) -> &str {
        &self.transactionId
    }

    /// 返回当前录制会话 ID；该值只用于流水线上下文，不会写入日志或代理响应。
    pub(crate) fn recordingSessionId(&self) -> &str {
        &self.recordingSessionId
    }

    /// 原子写入请求工具处理后的方法与目标，使事务树、详情和导出结果展示真正发送到上游的请求。
    pub(crate) async fn storeFinalRequestIdentity(
        &self,
        method: String,
        location: ResolvedLocation,
    ) -> Result<(), CaptureError> {
        self.session
            .update(
                &self.transactionId,
                TransactionUpdate {
                    method: Some(method),
                    location: Some(location),
                    ..TransactionUpdate::default()
                },
            )
            .await
    }

    /// 原子写入当前工具痕迹与标志；调用方必须在事务终态前同步，避免响应异步泵覆盖请求阶段结果。
    pub(crate) async fn storePipelineState(
        &self,
        flags: TransactionFlags,
        appliedTools: Vec<String>,
    ) -> Result<(), CaptureError> {
        self.session
            .update(
                &self.transactionId,
                TransactionUpdate {
                    flags: Some(flags),
                    ..TransactionUpdate::default()
                },
            )
            .await?;
        self.session
            .updateUserFields(
                &self.transactionId,
                TransactionUserUpdate {
                    appliedTools: Some(appliedTools),
                    ..TransactionUserUpdate::default()
                },
            )
            .await
    }

    /// 写入请求头并以字段级原子更新记录线上请求头字节数。
    pub(crate) async fn storeRequestHeaders(
        &self,
        headers: Vec<HeaderField>,
        headerBytes: u64,
    ) -> Result<(), CaptureError> {
        self.session
            .storeHeaders(&self.transactionId, MessageSide::Request, headers)
            .await?;
        self.session
            .updateProgress(
                &self.transactionId,
                TransactionProgressUpdate {
                    requestHeaderBytes: Some(headerBytes),
                    ..TransactionProgressUpdate::default()
                },
            )
            .await
    }

    /// 标记请求已交给上游客户端；字段级合并不会覆盖并行到达的响应进度。
    pub(crate) async fn markRequestSent(&self) -> Result<(), CaptureError> {
        self.session
            .updateProgress(
                &self.transactionId,
                TransactionProgressUpdate {
                    requestSentAtMilliseconds: Some(currentTimeMilliseconds()),
                    ..TransactionProgressUpdate::default()
                },
            )
            .await
    }

    /// 写入请求正文的有界前缀和线上总字节数。
    pub(crate) async fn storeRequestBody(&self, body: BodyWrite) -> Result<(), CaptureError> {
        self.session
            .storeBody(&self.transactionId, MessageSide::Request, body)
            .await
            .map(|_| ())
    }

    /// 写入响应头、状态码和首字节时间；各阶段字段在会话写锁内独立合并。
    pub(crate) async fn storeResponseHeaders(
        &self,
        headers: Vec<HeaderField>,
        headerBytes: u64,
        statusCode: u16,
    ) -> Result<(), CaptureError> {
        self.session
            .storeHeaders(&self.transactionId, MessageSide::Response, headers)
            .await?;
        self.session
            .updateProgress(
                &self.transactionId,
                TransactionProgressUpdate {
                    responseHeaderBytes: Some(headerBytes),
                    responseStartAtMilliseconds: Some(currentTimeMilliseconds()),
                    ..TransactionProgressUpdate::default()
                },
            )
            .await?;
        self.session
            .update(
                &self.transactionId,
                TransactionUpdate {
                    statusCode: Some(statusCode),
                    ..TransactionUpdate::default()
                },
            )
            .await
    }

    /// 写入完整响应流的有界镜像并将事务提交为 complete。
    pub(crate) async fn completeHttp(
        &self,
        body: BodyWrite,
        statusCode: u16,
    ) -> Result<(), CaptureError> {
        let contentType = body.contentType.clone();
        self.session
            .storeBody(&self.transactionId, MessageSide::Response, body)
            .await?;
        self.session
            .commit(
                &self.transactionId,
                TransactionCompletion {
                    statusCode,
                    endAtMilliseconds: currentTimeMilliseconds(),
                    contentType,
                },
            )
            .await
    }

    /// 写入合成阻断响应并将事务提交为 blocked；此状态仅用于流水线在出站前主动终止的请求。
    pub(crate) async fn completeBlocked(
        &self,
        body: BodyWrite,
        statusCode: u16,
    ) -> Result<(), CaptureError> {
        let contentType = body.contentType.clone();
        self.session
            .storeBody(&self.transactionId, MessageSide::Response, body)
            .await?;
        self.session
            .block(
                &self.transactionId,
                TransactionCompletion {
                    statusCode,
                    endAtMilliseconds: currentTimeMilliseconds(),
                    contentType,
                },
            )
            .await
    }

    /// 更新 CONNECT 建连时间，使隧道概览能区分等待连接与传输阶段。
    pub(crate) async fn markTunnelConnected(&self) -> Result<(), CaptureError> {
        let connectedAtMilliseconds = currentTimeMilliseconds();
        self.session
            .updateProgress(
                &self.transactionId,
                TransactionProgressUpdate {
                    connectEndAtMilliseconds: Some(connectedAtMilliseconds),
                    responseStartAtMilliseconds: Some(connectedAtMilliseconds),
                    ..TransactionProgressUpdate::default()
                },
            )
            .await
    }

    /// 保存 CONNECT 双向字节并提交 200 隧道事务；不创建任何明文 BodyRef。
    pub(crate) async fn completeTunnel(
        &self,
        clientToRemoteBytes: u64,
        remoteToClientBytes: u64,
    ) -> Result<(), CaptureError> {
        self.session
            .updateProgress(
                &self.transactionId,
                TransactionProgressUpdate {
                    requestBodyBytes: Some(clientToRemoteBytes),
                    responseBodyBytes: Some(remoteToClientBytes),
                    ..TransactionProgressUpdate::default()
                },
            )
            .await?;
        self.session
            .commit(
                &self.transactionId,
                TransactionCompletion {
                    statusCode: 200,
                    endAtMilliseconds: currentTimeMilliseconds(),
                    contentType: String::new(),
                },
            )
            .await
    }

    /// 将代理或录制失败写为结构化终态；事务被并发 clear 删除时视为已明确结束。
    pub(crate) async fn fail(&self, failure: RequestFailure) -> Result<(), CaptureError> {
        let result = if failure == RequestFailure::Cancelled {
            self.session
                .cancel(
                    &self.transactionId,
                    failure.transactionError(Some(&self.host)),
                    currentTimeMilliseconds(),
                )
                .await
        } else {
            self.session
                .fail(
                    &self.transactionId,
                    failure.transactionError(Some(&self.host)),
                    currentTimeMilliseconds(),
                )
                .await
        };
        match result {
            Ok(()) | Err(CaptureError::TransactionNotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}
