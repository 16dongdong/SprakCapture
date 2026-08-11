use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use socks5_core::ServiceMetrics;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::watch,
};

/// 保存 HTTP 连接在客户端套接字边界产生的真实累计指标，并以无队列覆盖通知唤醒控制面。
///
/// 计数器使用 Relaxed 顺序，因为各字段只表达统计值，不承担跨线程业务状态同步；`watch` 只传递
/// “快照已变化”信号，消费者必须重新调用 `snapshot` 读取权威原子值，避免高流量下积压逐包事件。
#[derive(Clone)]
pub struct HttpRuntimeMetrics {
    inner: Arc<HttpRuntimeMetricsInner>,
}

struct HttpRuntimeMetricsInner {
    acceptedConnections: AtomicU64,
    activeConnections: AtomicU64,
    failedConnections: AtomicU64,
    bytesUp: AtomicU64,
    bytesDown: AtomicU64,
    revision: AtomicU64,
    changeSender: watch::Sender<u64>,
}

impl Default for HttpRuntimeMetrics {
    /// 创建从零开始的独立 HTTP 服务周期账本；新服务不得复用旧周期计数。
    fn default() -> Self {
        let (changeSender, _) = watch::channel(0);
        Self {
            inner: Arc::new(HttpRuntimeMetricsInner {
                acceptedConnections: AtomicU64::new(0),
                activeConnections: AtomicU64::new(0),
                failedConnections: AtomicU64::new(0),
                bytesUp: AtomicU64::new(0),
                bytesDown: AtomicU64::new(0),
                revision: AtomicU64::new(0),
                changeSender,
            }),
        }
    }
}

impl HttpRuntimeMetrics {
    /// 返回当前 HTTP 累计值；UDP 字段保持为零，最终由控制层与 SOCKS 指标饱和相加。
    pub fn snapshot(&self) -> ServiceMetrics {
        ServiceMetrics {
            acceptedConnections: self.inner.acceptedConnections.load(Ordering::Relaxed),
            activeConnections: self.inner.activeConnections.load(Ordering::Relaxed),
            failedConnections: self.inner.failedConnections.load(Ordering::Relaxed),
            bytesUp: self.inner.bytesUp.load(Ordering::Relaxed),
            bytesDown: self.inner.bytesDown.load(Ordering::Relaxed),
            ..ServiceMetrics::default()
        }
    }

    /// 订阅后续变化；调用方必须先订阅再读取快照，避免连接恰好在基线读取窗口内结束时漏掉通知。
    pub fn subscribeChanges(&self) -> watch::Receiver<u64> {
        self.inner.changeSender.subscribe()
    }

    /// 登记一个已经由融合监听器交给 HTTP 状态机的客户端连接，并返回析构即结算的生命周期守卫。
    fn beginConnection(&self) -> HttpConnectionMetricGuard {
        self.inner
            .acceptedConnections
            .fetch_add(1, Ordering::Relaxed);
        self.inner.activeConnections.fetch_add(1, Ordering::Relaxed);
        self.notifyChanged();
        HttpConnectionMetricGuard {
            metrics: self.clone(),
            failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 累计客户端到代理的实际读取字节；只有底层 `poll_read` 成功推进缓冲区才会计入。
    fn recordUpstreamBytes(&self, byteCount: usize) {
        if byteCount == 0 {
            return;
        }
        self.inner
            .bytesUp
            .fetch_add(byteCount as u64, Ordering::Relaxed);
        self.notifyChanged();
    }

    /// 累计代理到客户端的实际写入字节；部分写仅按操作系统确认的长度计入。
    fn recordDownstreamBytes(&self, byteCount: usize) {
        if byteCount == 0 {
            return;
        }
        self.inner
            .bytesDown
            .fetch_add(byteCount as u64, Ordering::Relaxed);
        self.notifyChanged();
    }

    /// 用覆盖式修订通知唤醒实时控制面；通知不携带计数，慢消费者不会形成无界消息队列。
    fn notifyChanged(&self) {
        let revision = self
            .inner
            .revision
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.inner.changeSender.send_replace(revision);
    }
}

/// 绑定单连接活动计数；任务被强制中止时析构仍会归还 active，避免停止后残留幽灵连接。
pub(crate) struct HttpConnectionMetricGuard {
    metrics: HttpRuntimeMetrics,
    failed: Arc<AtomicBool>,
}

impl HttpConnectionMetricGuard {
    /// 创建不拥有 active 计数的失败标记；Hyper 状态机可在底层流已移动后标记最终失败。
    fn failureMarker(&self) -> HttpConnectionFailureMarker {
        HttpConnectionFailureMarker {
            failed: Arc::clone(&self.failed),
        }
    }
}

/// 与底层连接守卫共享失败位，但不参与 active 生命周期；升级隧道可继续持有真实套接字。
pub(crate) struct HttpConnectionFailureMarker {
    failed: Arc<AtomicBool>,
}

impl HttpConnectionFailureMarker {
    /// 标记 HTTP 状态机异常；底层流最终析构时一次性累计失败，重复标记保持幂等。
    pub(crate) fn markFailed(&self) {
        self.failed.store(true, Ordering::Relaxed);
    }
}

impl Drop for HttpConnectionMetricGuard {
    /// 在任何退出路径归还活动连接，并在协议错误路径累计失败后发布最终变化。
    fn drop(&mut self) {
        self.metrics
            .inner
            .activeConnections
            .fetch_sub(1, Ordering::Relaxed);
        if self.failed.load(Ordering::Relaxed) {
            self.metrics
                .inner
                .failedConnections
                .fetch_add(1, Ordering::Relaxed);
        }
        self.metrics.notifyChanged();
    }
}

/// 在不复制载荷的前提下包装客户端套接字；读写计数位于 Hyper 可见的最外层 I/O 边界。
pub(crate) struct HttpMetricsStream<Stream> {
    inner: Stream,
    metrics: HttpRuntimeMetrics,
    _connectionGuard: HttpConnectionMetricGuard,
}

impl<Stream> HttpMetricsStream<Stream> {
    /// 绑定一个底层流与当前服务周期账本；构造本身不改变连接计数，生命周期由独立守卫拥有。
    pub(crate) fn new(
        inner: Stream,
        metrics: HttpRuntimeMetrics,
    ) -> (Self, HttpConnectionFailureMarker) {
        let connectionGuard = metrics.beginConnection();
        let failureMarker = connectionGuard.failureMarker();
        (
            Self {
                inner,
                metrics,
                _connectionGuard: connectionGuard,
            },
            failureMarker,
        )
    }
}

impl<Stream: AsyncRead + Unpin> AsyncRead for HttpMetricsStream<Stream> {
    /// 代理异步读取，并按 `ReadBuf` 实际新增长度累计上行字节；Pending 与错误均不产生计数。
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let filledBefore = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(context, buffer);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.metrics
                .recordUpstreamBytes(buffer.filled().len().saturating_sub(filledBefore));
        }
        result
    }
}

impl<Stream: AsyncWrite + Unpin> AsyncWrite for HttpMetricsStream<Stream> {
    /// 代理异步写入，并仅按底层确认写出的长度累计下行字节；不会把待写长度误记为已发送。
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let result = Pin::new(&mut self.inner).poll_write(context, buffer);
        if let Poll::Ready(Ok(byteCount)) = result {
            self.metrics.recordDownstreamBytes(byteCount);
        }
        result
    }

    /// 转发刷新；刷新不代表新增线缆字节，因此不改变统计。
    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    /// 转发写半关闭；生命周期守卫在完整连接任务退出时统一结算 active。
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }

    /// 保留底层分散写能力，使大响应头与正文无需退化为额外拼接分配。
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    /// 代理分散写并按实际完成长度计数；语义与 `poll_write` 完全一致。
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        let result = Pin::new(&mut self.inner).poll_write_vectored(context, buffers);
        if let Poll::Ready(Ok(byteCount)) = result {
            self.metrics.recordDownstreamBytes(byteCount);
        }
        result
    }
}
