use std::{
    convert::Infallible,
    error::Error,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::body::{Body, Frame, Incoming};
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

pub(crate) type BoxBodyError = Box<dyn Error + Send + Sync>;
pub(crate) type ProxyBody = UnsyncBoxBody<Bytes, BoxBodyError>;
pub(crate) type BodyFrameSender = mpsc::Sender<Result<Frame<Bytes>, BoxBodyError>>;

/// 保存流式镜像的完整字节和线上总字节数；工具物化预算不得影响录制副本。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapturedBody {
    pub bytes: Vec<u8>,
    pub originalBytes: u64,
}

/// 保存单方向正文的完整镜像；录制失败必须显式上报，禁止静默保留前缀。
struct BodyAccumulator {
    bytes: Vec<u8>,
    originalBytes: u64,
}

impl BodyAccumulator {
    /// 创建完整镜像缓冲；不按可能过时的代理配置预分配容量。
    fn new() -> Self {
        Self {
            // 高并发连接不能按估算正文大小预分配；Vec 仅随实际已转发字节按需增长。
            bytes: Vec::new(),
            originalBytes: 0,
        }
    }

    /// 记录一个数据 frame；计数使用饱和加法，镜像必须复制完整内容。
    fn append(&mut self, bytes: &[u8]) {
        self.originalBytes = self.originalBytes.saturating_add(bytes.len() as u64);
        self.bytes.extend_from_slice(bytes);
    }

    /// 克隆当前完整镜像，供请求结束或响应泵完成后写入 capture-core。
    fn snapshot(&self) -> CapturedBody {
        CapturedBody {
            bytes: self.bytes.clone(),
            originalBytes: self.originalBytes,
        }
    }
}

/// 可跨 Hyper body poll 与异步事务任务共享的正文捕获器。
#[derive(Clone)]
pub(crate) struct SharedBodyCapture {
    inner: Arc<Mutex<BodyAccumulator>>,
}

impl SharedBodyCapture {
    /// 创建完整正文捕获器；该对象不接受任何会造成录制裁剪的大小参数。
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BodyAccumulator::new())),
        }
    }

    /// 追加线上数据 frame；parking_lot 临界区只执行顺序内存复制，不跨 await。
    pub(crate) fn append(&self, bytes: &[u8]) {
        self.inner.lock().append(bytes);
    }

    /// 返回当前镜像和原始计数，不暴露内部锁。
    pub(crate) fn snapshot(&self) -> CapturedBody {
        self.inner.lock().snapshot()
    }
}

/// 为任意 Hyper 请求 body 增加一次性终结通知，不改变原始 frame 与大小提示。
struct CompletionBody<InnerBody> {
    inner: Pin<Box<InnerBody>>,
    completion: Option<oneshot::Sender<()>>,
}

impl<InnerBody> CompletionBody<InnerBody> {
    /// 包装上游请求 body，并在完整结束或提前丢弃时通知录制任务可以取得最终计数。
    fn new(inner: InnerBody, completion: oneshot::Sender<()>) -> Self {
        Self {
            inner: Box::pin(inner),
            completion: Some(completion),
        }
    }

    /// 只发送一次结束通知；接收方已取消时无需额外处理。
    fn signalCompletion(&mut self) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        if completion.send(()).is_err() {
            tracing::debug!(
                errorCode = "httpProxyRequestCaptureReceiverClosed",
                messageKey = "error.httpProxy.cancelled"
            );
        }
    }
}

impl<InnerBody> Body for CompletionBody<InnerBody>
where
    InnerBody: Body,
{
    type Data = InnerBody::Data;
    type Error = InnerBody::Error;

    /// 透传 frame，并在底层返回结束时发布线性化完成信号。
    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let result = self.inner.as_mut().poll_frame(context);
        if matches!(result, Poll::Ready(None)) {
            self.signalCompletion();
        }
        result
    }

    /// 保留底层大小提示，帮助 Hyper 正确生成 Content-Length 或分块语义。
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }

    /// 透传正文结束判断，避免完成包装改变 Hyper 调度行为。
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

impl<InnerBody> Drop for CompletionBody<InnerBody> {
    /// 上游 early response 丢弃未消费请求时仍通知录制层，避免读取尚在变化的镜像。
    fn drop(&mut self) {
        self.signalCompletion();
    }
}

/// 表示上游客户端已完整消费或明确丢弃请求 body，确保最终快照不会与 body poll 竞态。
pub(crate) struct RequestBodyCompletion {
    receiver: oneshot::Receiver<()>,
}

impl RequestBodyCompletion {
    /// 等待请求 body 终结；代理取消优先返回 Cancelled，由事务层进入对应终态。
    pub(crate) async fn wait(
        self,
        cancellation: &CancellationToken,
    ) -> Result<(), crate::error::RequestFailure> {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(crate::error::RequestFailure::Cancelled),
            result = self.receiver => {
                result.map_err(|_| crate::error::RequestFailure::UpstreamProtocol)
            }
        }
    }
}

/// 将客户端 Incoming 包装为零拷贝转发 body，同时同步复制完整字节用于录制。
pub(crate) fn captureIncomingBody(
    incoming: Incoming,
) -> (ProxyBody, SharedBodyCapture, RequestBodyCompletion) {
    let capture = SharedBodyCapture::new();
    let frameCapture = capture.clone();
    let mappedBody = incoming
        .map_frame(move |frame| {
            if let Some(bytes) = frame.data_ref() {
                frameCapture.append(bytes);
            }
            frame
        })
        .map_err(|error| -> BoxBodyError { Box::new(error) });
    let (completionSender, completionReceiver) = oneshot::channel();
    let body = CompletionBody::new(mappedBody, completionSender).boxed_unsync();
    (
        body,
        capture,
        RequestBodyCompletion {
            receiver: completionReceiver,
        },
    )
}

/// 消费未出站的客户端正文并保留完整字节；Map Local/Block 短路必须排空请求，避免 keep-alive 连接残留下一条消息字节。
pub(crate) async fn drainIncomingBody(
    mut incoming: Incoming,
    cancellation: &CancellationToken,
) -> Result<CapturedBody, crate::error::RequestFailure> {
    let capture = SharedBodyCapture::new();
    loop {
        let frame = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(crate::error::RequestFailure::Cancelled),
            frame = incoming.frame() => frame,
        };
        match frame {
            Some(Ok(frame)) => {
                if let Some(bytes) = frame.data_ref() {
                    capture.append(bytes);
                }
            }
            Some(Err(_)) => return Err(crate::error::RequestFailure::ClientDisconnected),
            None => return Ok(capture.snapshot()),
        }
    }
}

/// 在正文工具明确声明需求时将单条消息物化为可改写字节；超出代理既有正文预算立即失败，绝不把工具模式变成无界缓冲代理。
pub(crate) async fn materializeIncomingBody(
    mut incoming: Incoming,
    maximumBytes: usize,
    cancellation: &CancellationToken,
) -> Result<Bytes, crate::error::RequestFailure> {
    let mut body = Vec::new();
    loop {
        let frame = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(crate::error::RequestFailure::Cancelled),
            frame = incoming.frame() => frame,
        };
        match frame {
            Some(Ok(frame)) => {
                let Some(bytes) = frame.data_ref() else {
                    continue;
                };
                let nextLength = body
                    .len()
                    .checked_add(bytes.len())
                    .ok_or(crate::error::RequestFailure::PipelineBodyLimitExceeded)?;
                if nextLength > maximumBytes {
                    return Err(crate::error::RequestFailure::PipelineBodyLimitExceeded);
                }
                body.extend_from_slice(bytes);
            }
            Some(Err(_)) => return Err(crate::error::RequestFailure::ClientDisconnected),
            None => return Ok(Bytes::from(body)),
        }
    }
}

/// 创建可用于请求或响应方向的有界 frame 通道；通道满时生产者自然背压，避免节流任务积压无界正文。
pub(crate) fn bodyFrameChannel(capacity: usize) -> (BodyFrameSender, ProxyBody) {
    let (sender, receiver) = mpsc::channel(capacity);
    let body = StreamBody::new(ReceiverStream::new(receiver)).boxed_unsync();
    (sender, body)
}

/// 创建不携带正文的统一响应 body。
pub(crate) fn emptyBody() -> ProxyBody {
    Empty::<Bytes>::new()
        .map_err(infallibleBodyError)
        .boxed_unsync()
}

/// 将已物化的合成响应正文恢复为统一代理 body；只用于短路路径，常规上游响应仍走有界流式通道。
pub(crate) fn bodyFromBytes(bytes: Bytes) -> ProxyBody {
    Full::new(bytes).map_err(infallibleBodyError).boxed_unsync()
}

/// 将 Infallible 映射到统一 body 错误类型；该分支在运行时不可达。
fn infallibleBodyError(error: Infallible) -> BoxBodyError {
    match error {}
}
