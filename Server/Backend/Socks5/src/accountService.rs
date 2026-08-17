use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::Sleep,
};
use tokio::{sync::Notify, time::Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    error::{Result, Socks5Error},
    model::TrafficDirection,
};

const defaultSynchronizationIntervalMilliseconds: u64 = 2_000;
const defaultRequestTimeoutMilliseconds: u64 = 3_000;

/// 描述 SOCKS5 数据面连接账号服务内部端点所需的最小配置。
///
/// 令牌由父进程匿名管道注入且不参与 Debug；端点必须指向回环 HTTP 服务，避免内部租约协议暴露到网卡。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountServiceClientConfig {
    pub endpoint: String,
    pub internalToken: String,
    #[serde(default = "defaultSynchronizationInterval")]
    pub synchronizationIntervalMilliseconds: u64,
    #[serde(default = "defaultRequestTimeout")]
    pub requestTimeoutMilliseconds: u64,
}

impl std::fmt::Debug for AccountServiceClientConfig {
    /// 输出不含内部令牌的诊断视图，防止控制层日志泄露进程级凭据。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountServiceClientConfig")
            .field("endpoint", &self.endpoint)
            .field(
                "synchronizationIntervalMilliseconds",
                &self.synchronizationIntervalMilliseconds,
            )
            .field(
                "requestTimeoutMilliseconds",
                &self.requestTimeoutMilliseconds,
            )
            .finish()
    }
}

impl AccountServiceClientConfig {
    /// 校验内部端点、令牌和心跳窗口；失败时服务不得进入监听状态。
    pub fn validate(&self) -> Result<()> {
        let endpoint = reqwest::Url::parse(&self.endpoint)
            .map_err(|error| Socks5Error::Configuration(format!("账号服务端点无效：{error}")))?;
        let host = endpoint
            .host_str()
            .ok_or_else(|| Socks5Error::Configuration("账号服务端点缺少主机".to_owned()))?;
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if endpoint.scheme() != "http" || !loopback {
            return Err(Socks5Error::Configuration(
                "账号服务内部端点必须使用回环 HTTP 地址".to_owned(),
            ));
        }
        if self.internalToken.len() < 32 {
            return Err(Socks5Error::Configuration(
                "账号服务内部令牌不能少于 32 个字符".to_owned(),
            ));
        }
        if !(250..=4_000).contains(&self.synchronizationIntervalMilliseconds) {
            return Err(Socks5Error::Configuration(
                "账号租约同步间隔必须位于 250..=4000 毫秒".to_owned(),
            ));
        }
        if !(100..=10_000).contains(&self.requestTimeoutMilliseconds) {
            return Err(Socks5Error::Configuration(
                "账号服务请求超时必须位于 100..=10000 毫秒".to_owned(),
            ));
        }
        Ok(())
    }
}

/// 为缺省配置提供低于账号服务租约过期窗口的同步间隔。
fn defaultSynchronizationInterval() -> u64 {
    defaultSynchronizationIntervalMilliseconds
}

/// 为缺省配置提供有限请求超时，序列化反序列化双方共享同一默认值。
fn defaultRequestTimeout() -> u64 {
    defaultRequestTimeoutMilliseconds
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthenticationRequest<'a> {
    connectionId: &'a str,
    username: &'a str,
    password: &'a str,
    sourceIp: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthenticationResponse {
    serviceInstanceId: String,
    accountId: String,
    leaseId: String,
    username: String,
    policyRevision: i64,
    maxUploadBytesPerSecond: i64,
    maxDownloadBytesPerSecond: i64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SynchronizationRequest {
    serviceInstanceId: String,
    batchId: String,
    leases: Vec<LeaseProgress>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LeaseProgress {
    leaseId: String,
    connectionId: String,
    uploadedBytes: u64,
    downloadedBytes: u64,
    #[serde(rename = "final")]
    final_: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SynchronizationResponse {
    serviceInstanceId: String,
    leases: Vec<SynchronizationResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SynchronizationResult {
    leaseId: String,
    policyRevision: i64,
    maxUploadBytesPerSecond: i64,
    maxDownloadBytesPerSecond: i64,
    revoked: bool,
}

struct DirectionSchedule {
    bytesPerSecond: i64,
    nextAvailable: Instant,
}

impl DirectionSchedule {
    /// 创建指定速率的空排期；负一表示该方向不限制，零只作为服务端撤销前的闭合边界。
    fn new(bytesPerSecond: i64) -> Self {
        Self {
            bytesPerSecond,
            nextAvailable: Instant::now(),
        }
    }

    /// 原子预留账号在当前方向的发送时间片；同账号所有连接因此共享同一条时间线。
    fn reserve(&mut self, byteCount: usize) -> Option<Instant> {
        if self.bytesPerSecond < 0 {
            return None;
        }
        if self.bytesPerSecond == 0 {
            return Some(Instant::now() + Duration::from_secs(86_400));
        }
        let now = Instant::now();
        let nanos = (byteCount as u128)
            .saturating_mul(1_000_000_000)
            .div_ceil(self.bytesPerSecond as u128)
            .min(u64::MAX as u128) as u64;
        let duration = Duration::from_nanos(nanos);
        // 只允许最多一秒突发信用；大首包超过一秒速率的部分必须在交付前等待。
        let credit = Duration::from_secs(1).min(duration);
        let base = self.nextAvailable.max(now);
        let scheduled = base + duration.saturating_sub(credit);
        self.nextAvailable = base + duration;
        Some(scheduled)
    }

    /// 策略修订立即替换速率并清除旧排期，避免降速或提速沿用失效时间债务。
    fn update(&mut self, bytesPerSecond: i64) {
        self.bytesPerSecond = bytesPerSecond;
        self.nextAvailable = Instant::now();
    }
}

struct AccountTrafficController {
    upload: Mutex<DirectionSchedule>,
    download: Mutex<DirectionSchedule>,
    policyRevision: Mutex<i64>,
    activeLeases: AtomicU64,
}

impl AccountTrafficController {
    /// 从认证响应建立账号级双向控制器，后续同账号租约复用该实例。
    fn new(response: &AuthenticationResponse) -> Self {
        Self {
            upload: Mutex::new(DirectionSchedule::new(response.maxUploadBytesPerSecond)),
            download: Mutex::new(DirectionSchedule::new(response.maxDownloadBytesPerSecond)),
            policyRevision: Mutex::new(response.policyRevision),
            activeLeases: AtomicU64::new(0),
        }
    }

    /// 仅接受不旧于当前值的策略修订，防止并发心跳的迟到响应回滚限速。
    fn update(&self, revision: i64, upload: i64, download: i64) {
        let mut currentRevision = self.policyRevision.lock();
        if revision <= *currentRevision {
            return;
        }
        *currentRevision = revision;
        self.upload.lock().update(upload);
        self.download.lock().update(download);
    }

    /// 等待账号共享时间片；取消信号用于策略撤销和服务停机时立即中断等待。
    async fn acquire(
        &self,
        direction: TrafficDirection,
        byteCount: usize,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let scheduled = match direction {
            TrafficDirection::Up => self.upload.lock().reserve(byteCount),
            TrafficDirection::Down => self.download.lock().reserve(byteCount),
        };
        let Some(scheduled) = scheduled else {
            return Ok(());
        };
        tokio::select! {
            _ = cancellation.cancelled() => Err(Socks5Error::AuthenticationFailed),
            _ = tokio::time::sleep_until(scheduled) => Ok(()),
        }
    }
}

struct ClientState {
    http: reqwest::Client,
    endpoint: String,
    internalToken: String,
    synchronizationInterval: Duration,
    controllers: Mutex<HashMap<String, Arc<AccountTrafficController>>>,
    leases: Mutex<HashMap<String, Arc<LeaseState>>>,
    synchronizationNotify: Notify,
    serviceInstanceId: Mutex<Option<String>>,
}

/// 复用 HTTP 连接池和账号级限速状态；该类型只在 SOCKS5 服务实例内创建一次。
#[derive(Clone)]
pub(crate) struct AccountServiceClient {
    state: Arc<ClientState>,
}

/// 收拢一次账号租约认证所需的连接身份与取消信号，避免认证入口依赖易错的位置参数顺序。
pub(crate) struct AccountLeaseAuthentication<'a> {
    pub connectionId: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub sourceIp: IpAddr,
    pub cancellation: CancellationToken,
}

impl AccountServiceClient {
    /// 构造并验证内部客户端；请求超时覆盖认证和心跳，防止账号服务故障拖住数据面任务。
    pub(crate) fn new(config: AccountServiceClientConfig) -> Result<Self> {
        config.validate()?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.requestTimeoutMilliseconds))
            .build()
            .map_err(|error| Socks5Error::Runtime(format!("创建账号服务客户端失败：{error}")))?;
        let state = Arc::new(ClientState {
            http,
            endpoint: config.endpoint.trim_end_matches('/').to_owned(),
            internalToken: config.internalToken,
            synchronizationInterval: Duration::from_millis(
                config.synchronizationIntervalMilliseconds,
            ),
            controllers: Mutex::new(HashMap::new()),
            leases: Mutex::new(HashMap::new()),
            synchronizationNotify: Notify::new(),
            serviceInstanceId: Mutex::new(None),
        });
        let client = Self { state };
        client.startSynchronizationWorker();
        Ok(client)
    }

    /// 调用账号服务完成密码校验和租约申请；任何协议或传输失败都按认证拒绝处理。
    pub(crate) async fn authenticate(
        &self,
        authentication: AccountLeaseAuthentication<'_>,
    ) -> Option<AccountTrafficLease> {
        let AccountLeaseAuthentication {
            connectionId,
            username,
            password,
            sourceIp,
            cancellation,
        } = authentication;
        let response = self
            .state
            .http
            .post(format!(
                "{}/internal/v1/leases/authenticate",
                self.state.endpoint
            ))
            .header("x-account-service-token", &self.state.internalToken)
            .json(&AuthenticationRequest {
                connectionId,
                username,
                password,
                sourceIp: sourceIp.to_string(),
            })
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let response: AuthenticationResponse = response.json().await.ok()?;
        self.adoptServiceInstance(&response.serviceInstanceId);
        let controller = {
            let mut controllers = self.state.controllers.lock();
            let controller = controllers
                .entry(response.accountId.clone())
                .or_insert_with(|| Arc::new(AccountTrafficController::new(&response)))
                .clone();
            // 引用计数必须在控制器映射锁内增加，避免最后租约回收与新认证交错后误删共享实例。
            controller.activeLeases.fetch_add(1, Ordering::Relaxed);
            controller
        };
        controller.update(
            response.policyRevision,
            response.maxUploadBytesPerSecond,
            response.maxDownloadBytesPerSecond,
        );
        let lease = AccountTrafficLease {
            inner: Arc::new(LeaseState {
                client: self.clone(),
                serviceInstanceId: response.serviceInstanceId,
                accountId: response.accountId,
                leaseId: response.leaseId,
                connectionId: connectionId.to_owned(),
                username: response.username,
                uploadedBytes: AtomicU64::new(0),
                downloadedBytes: AtomicU64::new(0),
                controller,
                cancellation,
                revoked: CancellationToken::new(),
                finalPending: AtomicBool::new(false),
            }),
        };
        self.state
            .leases
            .lock()
            .insert(lease.inner.leaseId.clone(), lease.inner.clone());
        Some(lease)
    }

    /// 启动唯一批量同步任务；全部租约共享定时器和 HTTP 请求，避免连接数放大后台任务。
    fn startSynchronizationWorker(&self) {
        let weakState = Arc::downgrade(&self.state);
        tokio::spawn(async move {
            loop {
                let Some(state) = weakState.upgrade() else {
                    return;
                };
                tokio::select! {
                    _ = tokio::time::sleep(state.synchronizationInterval) => {},
                    _ = state.synchronizationNotify.notified() => {},
                }
                let client = AccountServiceClient { state };
                let Some(request) = client.buildSynchronizationRequest() else {
                    continue;
                };
                client.synchronizeWithRetry(request).await;
            }
        });
    }

    /// 接受认证响应中的实例标识；发现重启后撤销并清空所有旧实例租约。
    fn adoptServiceInstance(&self, serviceInstanceId: &str) {
        let changed = {
            let mut current = self.state.serviceInstanceId.lock();
            let changed = current
                .as_deref()
                .is_some_and(|value| value != serviceInstanceId);
            *current = Some(serviceInstanceId.to_owned());
            changed
        };
        if !changed {
            return;
        }
        let removed: Vec<Arc<LeaseState>> = self
            .state
            .leases
            .lock()
            .drain()
            .map(|(_, lease)| lease)
            .collect();
        for lease in removed {
            lease.revoked.cancel();
            releaseController(&self.state, &lease);
        }
    }

    /// 对活动租约生成单一不可变批次；重试期间新流量留到下一批，保证同 batchId 载荷一致。
    fn buildSynchronizationRequest(&self) -> Option<SynchronizationRequest> {
        let leases = self.state.leases.lock();
        let first = leases.values().next()?;
        Some(SynchronizationRequest {
            serviceInstanceId: first.serviceInstanceId.clone(),
            batchId: Uuid::new_v4().to_string(),
            leases: leases
                .values()
                .map(|lease| LeaseProgress {
                    leaseId: lease.leaseId.clone(),
                    connectionId: lease.connectionId.clone(),
                    uploadedBytes: lease.uploadedBytes.load(Ordering::Relaxed),
                    downloadedBytes: lease.downloadedBytes.load(Ordering::Relaxed),
                    final_: lease.finalPending.load(Ordering::Acquire),
                })
                .collect(),
        })
    }

    /// 在八秒租约窗口内使用完全相同的批次有界重试；超窗后撤销该批全部连接。
    async fn synchronizeWithRetry(&self, request: SynchronizationRequest) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match self.sendSynchronization(&request).await {
                Ok(response) => {
                    self.applySynchronizationResponse(&request, response);
                    return;
                }
                Err(SynchronizationFailure::Transient) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(_) => {
                    let leases = self.state.leases.lock();
                    for progress in &request.leases {
                        if let Some(lease) = leases.get(&progress.leaseId) {
                            lease.revoked.cancel();
                        }
                    }
                    return;
                }
            }
        }
    }

    /// 发送一个不可变同步批次并解码响应；调用者负责重试策略。
    async fn sendSynchronization(
        &self,
        request: &SynchronizationRequest,
    ) -> std::result::Result<SynchronizationResponse, SynchronizationFailure> {
        let response = self
            .state
            .http
            .post(format!(
                "{}/internal/v1/leases/{}",
                self.state.endpoint, "synchronize"
            ))
            .header("x-account-service-token", &self.state.internalToken)
            .json(&request)
            .send()
            .await
            .map_err(|_| SynchronizationFailure::Transient)?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Err(SynchronizationFailure::InstanceChanged);
        }
        if !response.status().is_success() {
            return Err(SynchronizationFailure::Transient);
        }
        let response: SynchronizationResponse = response
            .json()
            .await
            .map_err(|_| SynchronizationFailure::Transient)?;
        if response.serviceInstanceId != request.serviceInstanceId {
            return Err(SynchronizationFailure::InstanceChanged);
        }
        Ok(response)
    }

    /// 应用批量策略与撤销结果，并在服务端确认 final 后回收客户端租约。
    fn applySynchronizationResponse(
        &self,
        request: &SynchronizationRequest,
        response: SynchronizationResponse,
    ) {
        let mut leases = self.state.leases.lock();
        for result in response.leases {
            let Some(lease) = leases.get(&result.leaseId).cloned() else {
                continue;
            };
            lease.controller.update(
                result.policyRevision,
                result.maxUploadBytesPerSecond,
                result.maxDownloadBytesPerSecond,
            );
            if result.revoked {
                lease.revoked.cancel();
            }
            let finalConfirmed = request
                .leases
                .iter()
                .any(|progress| progress.leaseId == result.leaseId && progress.final_);
            if finalConfirmed {
                leases.remove(&result.leaseId);
                releaseController(&self.state, &lease);
            }
        }
    }
}

enum SynchronizationFailure {
    Transient,
    InstanceChanged,
}

struct LeaseState {
    client: AccountServiceClient,
    serviceInstanceId: String,
    accountId: String,
    leaseId: String,
    connectionId: String,
    username: String,
    uploadedBytes: AtomicU64,
    downloadedBytes: AtomicU64,
    controller: Arc<AccountTrafficController>,
    cancellation: CancellationToken,
    revoked: CancellationToken,
    finalPending: AtomicBool,
}

/// 表示一次已认证连接的租约、共享限速器和单调流量账本。
#[derive(Clone)]
pub struct AccountTrafficLease {
    inner: Arc<LeaseState>,
}

impl AccountTrafficLease {
    /// 返回账号服务确认的规范用户名，用于会话快照而不回显输入差异。
    pub fn username(&self) -> &str {
        &self.inner.username
    }

    /// 返回稳定账号标识，供诊断确认多个连接确实共享同一限速器。
    pub fn accountId(&self) -> &str {
        &self.inner.accountId
    }

    /// 用当前租约包装双工流；包装器严格在写入/交付前等待额度，并在成功后记录实际字节。
    pub fn meterStream<S>(
        &self,
        stream: S,
        readDirection: TrafficDirection,
        writeDirection: TrafficDirection,
    ) -> AccountTrafficStream<S> {
        AccountTrafficStream::new(stream, self.clone(), readDirection, writeDirection)
    }

    /// 在实际发送前等待账号共享额度，并在成功发送后由 record 计入租约累计值。
    pub async fn acquire(&self, direction: TrafficDirection, byteCount: usize) -> Result<()> {
        tokio::select! {
            result = self.inner.controller.acquire(direction, byteCount, &self.inner.cancellation) => result,
            _ = self.inner.revoked.cancelled() => Err(Socks5Error::AuthenticationFailed),
        }
    }

    /// 记录已经成功送达另一侧的有效负载；饱和加法保证极长连接不会回绕破坏单调协议。
    pub fn record(&self, direction: TrafficDirection, byteCount: usize) {
        let counter = match direction {
            TrafficDirection::Up => &self.inner.uploadedBytes,
            TrafficDirection::Down => &self.inner.downloadedBytes,
        };
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(byteCount as u64))
        });
    }

    /// 同步预留一个共享时间片并返回需要等待的时长，供基于 poll 的流包装器使用。
    fn reserveDelay(&self, direction: TrafficDirection, byteCount: usize) -> Option<Duration> {
        let scheduled = match direction {
            TrafficDirection::Up => self.inner.controller.upload.lock().reserve(byteCount),
            TrafficDirection::Down => self.inner.controller.download.lock().reserve(byteCount),
        }?;
        Some(scheduled.saturating_duration_since(Instant::now()))
    }

    /// 标记 final 并唤醒全局批量任务；失败批次由同一任务保持原载荷重试。
    pub(crate) async fn finish(&self) {
        self.inner.finalPending.store(true, Ordering::Release);
        self.inner.client.state.synchronizationNotify.notify_one();
    }

    /// 等待账号服务撤销或同步失败；命令处理器据此关闭仍阻塞在网络读取中的连接。
    pub(crate) async fn cancelled(&self) {
        self.inner.revoked.cancelled().await;
    }
}

/// 回收租约对应的账号控制器引用；最后租约确认 final 后删除共享调度器。
fn releaseController(state: &ClientState, lease: &LeaseState) {
    let mut controllers = state.controllers.lock();
    let lastLease = lease.controller.activeLeases.fetch_sub(1, Ordering::AcqRel) == 1;
    if lastLease
        && controllers
            .get(&lease.accountId)
            .is_some_and(|current| Arc::ptr_eq(current, &lease.controller))
    {
        controllers.remove(&lease.accountId);
    }
}

/// 把账号共享限速和累计统计附着到任意异步双工流，供 HTTP/TLS 接管器复用核心算法。
///
/// 读取先暂存在固定缓冲区，等额度到达后才交给协议解析器；写入在调用底层流之前等待额度，
/// 因此应用层接管不会因 Hyper 或 TLS 自行读写而绕开账号策略。
pub struct AccountTrafficStream<S> {
    inner: S,
    lease: AccountTrafficLease,
    readDirection: TrafficDirection,
    writeDirection: TrafficDirection,
    pendingRead: Vec<u8>,
    pendingReadOffset: usize,
    readDelay: Option<Pin<Box<Sleep>>>,
    writeDelay: Option<Pin<Box<Sleep>>>,
    scheduledWriteBytes: usize,
}

impl<S> AccountTrafficStream<S> {
    /// 创建带方向语义的流；客户端流通常使用 read=Up、write=Down。
    pub fn new(
        inner: S,
        lease: AccountTrafficLease,
        readDirection: TrafficDirection,
        writeDirection: TrafficDirection,
    ) -> Self {
        Self {
            inner,
            lease,
            readDirection,
            writeDirection,
            pendingRead: Vec::new(),
            pendingReadOffset: 0,
            readDelay: None,
            writeDelay: None,
            scheduledWriteBytes: 0,
        }
    }

    /// 取回底层流；仅供接管器明确结束账号计量边界后使用。
    pub fn intoInner(self) -> S {
        self.inner
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for AccountTrafficStream<S> {
    /// 读取底层数据后等待共享额度再交付调用方，并只统计实际交付字节。
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pendingReadOffset >= self.pendingRead.len() {
            let capacity = output.remaining().min(64 * 1024);
            if capacity == 0 {
                return Poll::Ready(Ok(()));
            }
            let mut bytes = vec![0_u8; capacity];
            let mut temporary = ReadBuf::new(&mut bytes);
            match Pin::new(&mut self.inner).poll_read(context, &mut temporary) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => {
                    let byteCount = temporary.filled().len();
                    if byteCount == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    bytes.truncate(byteCount);
                    self.pendingRead = bytes;
                    self.pendingReadOffset = 0;
                    self.readDelay = self
                        .lease
                        .reserveDelay(self.readDirection, byteCount)
                        .filter(|delay| !delay.is_zero())
                        .map(|delay| Box::pin(tokio::time::sleep(delay)));
                }
            }
        }
        if let Some(delay) = &mut self.readDelay
            && delay.as_mut().poll(context).is_pending()
        {
            return Poll::Pending;
        }
        self.readDelay = None;
        let available = self.pendingRead.len() - self.pendingReadOffset;
        let byteCount = available.min(output.remaining());
        let start = self.pendingReadOffset;
        let end = start + byteCount;
        output.put_slice(&self.pendingRead[start..end]);
        self.pendingReadOffset = end;
        self.lease.record(self.readDirection, byteCount);
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for AccountTrafficStream<S> {
    /// 在底层写入前等待共享额度，底层只接受本次已经预留的字节范围。
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.scheduledWriteBytes == 0 {
            self.scheduledWriteBytes = bytes.len().min(64 * 1024);
            self.writeDelay = self
                .lease
                .reserveDelay(self.writeDirection, self.scheduledWriteBytes)
                .filter(|delay| !delay.is_zero())
                .map(|delay| Box::pin(tokio::time::sleep(delay)));
        }
        if let Some(delay) = &mut self.writeDelay
            && delay.as_mut().poll(context).is_pending()
        {
            return Poll::Pending;
        }
        self.writeDelay = None;
        let maximum = self.scheduledWriteBytes.min(bytes.len());
        match Pin::new(&mut self.inner).poll_write(context, &bytes[..maximum]) {
            Poll::Ready(Ok(byteCount)) => {
                self.scheduledWriteBytes = 0;
                self.lease.record(self.writeDirection, byteCount);
                Poll::Ready(Ok(byteCount))
            }
            Poll::Ready(Err(error)) => {
                self.scheduledWriteBytes = 0;
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    /// 直接转发刷新；所有已计费写入都已经进入底层流，不存在包装器私有写缓存。
    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    /// 直接转发半关闭；调用者仍由外层租约生命周期提交 final 累计值。
    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[cfg(test)]
#[path = "../tests/unit/accountServiceScheduleTests.rs"]
mod tests;
