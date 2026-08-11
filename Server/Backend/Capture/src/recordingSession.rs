use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    mem::size_of,
    sync::Arc,
};

use location_core::{
    LocationMatchOptions, LocationPattern, ResolvedLocation, locationMatches,
    validateLocationPattern,
};
use tokio::sync::{RwLock, watch};
use uuid::Uuid;

use crate::{
    BeginTransaction, BodyHandleMeta, BodyReadLease, BodyRef, BodyResponse, BodyStorageKind,
    BodyWrite, CaptureError, HeaderField, MessageSide, RecordingConfiguration, RecordingLimits,
    RecordingPageView, RecordingRuleAction, RecordingRuleRuntime, RecordingSettingsUpdate,
    RecordingSnapshot, RecordingState, ResponseRangeCandidate, StreamPacket, TransactionCompletion,
    TransactionDetailRecord, TransactionError, TransactionProgressUpdate, TransactionProtocol,
    TransactionStatus, TransactionSummary, TransactionUpdate, TransactionUserUpdate,
    bodyStore::{BodySpool, BodyStore},
    metadataBudget::{
        boundBeginTransaction, boundBodyWrite, boundCompletion, boundHeaders,
        boundTransactionError, boundTransactionUpdate, boundUserUpdate, headerStorageBytes,
        maximumTransactionSummaryBytes, minimumMetadataMemoryBudgetBytes, summaryStorageBytes,
    },
    responseContentRange, strongResponseEntityTag,
};

#[derive(Clone)]
struct TransactionRecord {
    summary: TransactionSummary,
    summaryStorageBytes: usize,
    requestHeaders: Vec<HeaderField>,
    responseHeaders: Vec<HeaderField>,
    requestBody: Option<BodyRef>,
    responseBody: Option<BodyRef>,
    requestPackets: Vec<StreamPacket>,
    responsePackets: Vec<StreamPacket>,
    requestHeadersTruncated: bool,
    responseHeadersTruncated: bool,
}

/// 唯一标识可安全跨事务拼接的 HTTP 实体版本；URL 保留 scheme，强 ETag 按字节比较。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ResponseEntityIndexKey {
    urlDisplay: String,
    entityTag: String,
    totalBytes: u64,
    contentEncoding: String,
}

impl TransactionRecord {
    /// 克隆控制面详情所需的非正文字段；正文只暴露句柄元信息，不读取或复制实际字节。
    fn detail(&self) -> TransactionDetailRecord {
        TransactionDetailRecord {
            transaction: self.summary.clone(),
            requestHeaders: self.requestHeaders.clone(),
            responseHeaders: self.responseHeaders.clone(),
            requestBody: self.requestBody.as_ref().map(|body| body.meta().clone()),
            responseBody: self.responseBody.as_ref().map(|body| body.meta().clone()),
            requestPackets: self.requestPackets.clone(),
            responsePackets: self.responsePackets.clone(),
        }
    }

    /// 返回指定消息侧的正文引用，供读取和替换逻辑共用。
    fn body(&self, side: MessageSide) -> Option<&BodyRef> {
        match side {
            MessageSide::Request => self.requestBody.as_ref(),
            MessageSide::Response => self.responseBody.as_ref(),
        }
    }

    /// 替换指定消息侧正文，并返回原引用供会话锁外回收 spill 文件。
    fn replaceBody(&mut self, side: MessageSide, bodyReference: BodyRef) -> Option<BodyRef> {
        match side {
            MessageSide::Request => self.requestBody.replace(bodyReference),
            MessageSide::Response => self.responseBody.replace(bodyReference),
        }
    }

    /// 汇总请求和响应两侧正文引用，供用户显式清空时统一回收 spill 资源。
    fn intoBodyReferences(self) -> Vec<BodyRef> {
        self.requestBody
            .into_iter()
            .chain(self.responseBody)
            .collect()
    }

    /// 根据两侧正文元信息重新计算列表级截断标志。
    fn refreshTruncatedFlag(&mut self) {
        self.summary.flags.bodyTruncated = self
            .requestBody
            .iter()
            .chain(self.responseBody.iter())
            .any(|bodyReference| bodyReference.meta().truncated);
        self.summary.flags.headersTruncated =
            self.requestHeadersTruncated || self.responseHeadersTruncated;
    }
}

/// 从终态 206 事务构造最小 Range 候选；查询热路径不克隆 URL、ETag 或响应头。
fn responseRangeCandidate(record: &TransactionRecord) -> Option<ResponseRangeCandidate> {
    if record.summary.status != TransactionStatus::Complete
        || record.summary.statusCode != Some(206)
    {
        return None;
    }
    let (start, end, _) = responseContentRange(&record.responseHeaders)?;
    let body = record.responseBody.as_ref()?.meta();
    let rangeBytes = end.checked_sub(start)?.checked_add(1)?;
    if body.truncated || body.originalBytes != rangeBytes || body.storedBytes as u64 != rangeBytes {
        return None;
    }
    Some(ResponseRangeCandidate {
        transactionId: record.summary.transactionId.clone(),
        sequence: record.summary.sequence,
        start,
        end,
        body: body.clone(),
    })
}

/// 从有效 Range 事务构造实体索引键和成员；仅在终态提交与删除边界执行字符串克隆。
fn responseEntityEntry(
    record: &TransactionRecord,
) -> Option<(ResponseEntityIndexKey, ResponseRangeCandidate)> {
    let candidate = responseRangeCandidate(record)?;
    let entityTag = strongResponseEntityTag(&record.responseHeaders)?;
    let (_, _, totalBytes) = responseContentRange(&record.responseHeaders)?;
    let key = ResponseEntityIndexKey {
        urlDisplay: record.summary.urlDisplay.clone(),
        entityTag: entityTag.to_owned(),
        totalBytes,
        contentEncoding: candidate.body.encoding.to_ascii_lowercase(),
    };
    Some((key, candidate))
}

/// 把刚进入终态且满足实体约束的事务登记到二级索引；同一事务重复调用保持幂等。
fn insertResponseEntityEntry(state: &mut RecordingStateInner, transactionId: &str) {
    let entry = state
        .transactions
        .get(transactionId)
        .and_then(responseEntityEntry)
        .map(|(key, candidate)| (key, candidate.sequence, candidate.transactionId));
    let Some((key, sequence, identifier)) = entry else {
        return;
    };
    state
        .responseEntityIndex
        .entry(key)
        .or_default()
        .insert((sequence, identifier));
}

/// 在事务记录销毁前同步移除二级索引项；最后一个成员离开时同时释放实体键。
fn removeResponseEntityEntry(state: &mut RecordingStateInner, record: &TransactionRecord) {
    let Some((key, candidate)) = responseEntityEntry(record) else {
        return;
    };
    let shouldRemoveKey = state
        .responseEntityIndex
        .get_mut(&key)
        .is_some_and(|members| {
            members.remove(&(candidate.sequence, candidate.transactionId));
            members.is_empty()
        });
    if shouldRemoveKey {
        state.responseEntityIndex.remove(&key);
    }
}

struct RecordingStateInner {
    recordingState: RecordingState,
    recordingSessionId: String,
    startedAtMilliseconds: u64,
    nextSequence: u64,
    droppedCount: u64,
    totalBodyBytes: usize,
    totalMetadataBytes: usize,
    metadataMemoryBudgetBytes: usize,
    collectionVersion: u64,
    limits: RecordingLimits,
    ignoreLocations: Vec<LocationPattern>,
    recordTunnelMetadata: bool,
    transactions: HashMap<String, TransactionRecord>,
    responseEntityIndex: HashMap<ResponseEntityIndexKey, BTreeSet<(u64, String)>>,
    order: VecDeque<String>,
    cleanupQueue: VecDeque<BodyRef>,
    directoryCleanupPending: bool,
    closed: bool,
}

struct RecordingSessionInner {
    // 同一会话的正文提交、clear 与 close 必须共享这一写锁完成线性化。写锁可跨
    // Tokio 文件 I/O await，但不会阻塞执行线程；最大单体受 limits 严格限制。
    // 不能把状态预算与文件提交任意拆锁，否则 clear 可能删除刚提交但尚未登记的正文。
    state: RwLock<RecordingStateInner>,
    bodyStore: BodyStore,
    recordingRules: RecordingRuleRuntime,
    changeSender: watch::Sender<u64>,
}

/// 管理单一 active 录制会话；克隆句柄可被多个代理任务并发使用。
#[derive(Clone)]
pub struct RecordingSession {
    inner: Arc<RecordingSessionInner>,
}

impl RecordingSession {
    /// 创建录制会话并初始化专属 spill 目录；无效预算、规则或目录会返回结构化错误。
    pub async fn new(configuration: RecordingConfiguration) -> Result<Self, CaptureError> {
        if !configuration.limits.isValid() {
            return Err(CaptureError::InvalidLimits);
        }
        if configuration.memoryBodyThreshold == 0 {
            return Err(CaptureError::InvalidMemoryThreshold);
        }
        if configuration.metadataMemoryBudgetBytes < minimumMetadataMemoryBudgetBytes {
            return Err(CaptureError::InvalidMetadataMemoryBudget);
        }
        for pattern in &configuration.ignoreLocations {
            validateLocationPattern(pattern)?;
        }
        let recordingRules = RecordingRuleRuntime::new(configuration.recordingRules)
            .map_err(|_| CaptureError::InvalidRecordingRules)?;
        let recordingSessionId = Uuid::new_v4().to_string();
        let bodyStore = BodyStore::new(
            &configuration.spillDirectory,
            &recordingSessionId,
            configuration.memoryBodyThreshold,
        )
        .await?;
        let (changeSender, _) = watch::channel(0_u64);
        Ok(Self {
            inner: Arc::new(RecordingSessionInner {
                state: RwLock::new(RecordingStateInner {
                    recordingState: RecordingState::Recording,
                    recordingSessionId,
                    startedAtMilliseconds: crate::currentTimeMilliseconds(),
                    nextSequence: 1,
                    droppedCount: 0,
                    totalBodyBytes: 0,
                    totalMetadataBytes: 0,
                    metadataMemoryBudgetBytes: configuration.metadataMemoryBudgetBytes,
                    collectionVersion: 0,
                    limits: configuration.limits,
                    ignoreLocations: configuration.ignoreLocations,
                    recordTunnelMetadata: configuration.recordTunnelMetadata,
                    transactions: HashMap::new(),
                    responseEntityIndex: HashMap::new(),
                    order: VecDeque::new(),
                    cleanupQueue: VecDeque::new(),
                    directoryCleanupPending: false,
                    closed: false,
                }),
                bodyStore,
                recordingRules,
                changeSender,
            }),
        })
    }

    /// 订阅录制状态和事务元数据变化；watch 保留最新序号，使慢消费者不会丢失最终状态。
    pub fn subscribeChanges(&self) -> watch::Receiver<u64> {
        self.inner.changeSender.subscribe()
    }

    /// 返回当前内部变化序号；控制层用它去重同步写事件与 50ms 合并事件。
    pub fn currentChangeRevision(&self) -> u64 {
        *self.inner.changeSender.borrow()
    }

    /// 在成功提交权威状态后递增内部变化序号；控制层只把它作为合并提示，不将其暴露为公共 revision。
    fn notifyChanged(&self) {
        self.inner
            .changeSender
            .send_modify(|revision| *revision = revision.saturating_add(1));
    }

    /// 切换为 recording，使之后的新事务可以进入当前会话；关闭后返回 SessionClosed。
    pub async fn startRecording(&self) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        state.recordingState = RecordingState::Recording;
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 切换为 paused；已有 pending 事务仍可继续更新、写体并完成。
    pub async fn pauseRecording(&self) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        state.recordingState = RecordingState::Paused;
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 返回不含头和正文的录制快照，读取期间不会执行任何磁盘 I/O。
    pub async fn snapshot(&self) -> Result<RecordingSnapshot, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        Ok(snapshotFromState(
            &state,
            self.inner.bodyStore.pendingOrphanCount(),
        ))
    }

    /// 在单读锁内克隆指定页；offset=None 表示最新页，limit 只控制摘要数量且不触碰正文。
    pub async fn pageView(
        &self,
        offset: Option<usize>,
        limit: usize,
        expectedCollectionToken: Option<&str>,
    ) -> Result<RecordingPageView, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        let collectionToken = collectionTokenFromState(&state);
        if expectedCollectionToken.is_some_and(|expected| expected != collectionToken) {
            return Err(CaptureError::CollectionChanged);
        }
        let total = state.order.len();
        let pageOffset = offset.unwrap_or_else(|| total.saturating_sub(limit));
        let transactions = state
            .order
            .iter()
            .skip(pageOffset.min(total))
            .take(limit)
            .filter_map(|identifier| state.transactions.get(identifier))
            .map(|record| record.summary.clone())
            .collect();
        Ok(RecordingPageView {
            recording: snapshotFromState(&state, self.inner.bodyStore.pendingOrphanCount()),
            collectionToken,
            total,
            offset: pageOffset,
            transactions,
        })
    }

    /// 判断当前状态和忽略 Location 是否允许创建新事务；无效候选返回 Location 错误。
    pub async fn shouldRecord(&self, location: &ResolvedLocation) -> Result<bool, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        shouldRecordFromState(&state, location)
    }

    /// 返回录制规则运行时的共享句柄；控制面热更新与数据面判断必须读取同一原子快照。
    pub fn recordingRules(&self) -> RecordingRuleRuntime {
        self.inner.recordingRules.clone()
    }

    /// 对即将建立的事务执行规则裁决；同步读取不持有会话异步锁，也不会阻塞正文落盘。
    pub fn recordingDecision(&self, input: &BeginTransaction) -> RecordingRuleAction {
        self.inner.recordingRules.decision(input)
    }

    /// 创建 pending 事务；暂停、忽略或禁用隧道元数据时返回 Ok(None) 而不消耗 sequence。
    pub async fn beginTransaction(
        &self,
        input: BeginTransaction,
    ) -> Result<Option<String>, CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        self.drainCleanupQueueLocked(&mut state).await?;
        if !shouldRecordFromState(&state, &input.location)?
            || self.recordingDecision(&input) == RecordingRuleAction::DoNotRecord
            || (matches!(
                input.protocol,
                TransactionProtocol::Tunnel | TransactionProtocol::Socks
            ) && !state.recordTunnelMetadata)
        {
            return Ok(None);
        }
        let input = boundBeginTransaction(input);
        let transactionId = Uuid::new_v4().to_string();
        let sequence = state.nextSequence;
        let summary = TransactionSummary {
            transactionId: transactionId.clone(),
            recordingSessionId: state.recordingSessionId.clone(),
            sequence,
            protocol: input.protocol,
            method: input.method,
            host: input.location.host,
            port: input.location.port,
            path: input.location.path,
            query: input.location.query,
            urlDisplay: input.location.display,
            status: TransactionStatus::Pending,
            statusCode: None,
            clientAddress: input.clientAddress,
            clientProcessName: input.clientProcessName,
            clientProcessId: input.clientProcessId,
            contentType: input.contentType,
            timings: crate::TransactionTimings {
                startAtMilliseconds: input.startAtMilliseconds,
                ..crate::TransactionTimings::default()
            },
            sizes: crate::TransactionSizes::default(),
            flags: crate::TransactionFlags::default(),
            error: None,
            notes: String::new(),
            tags: Vec::new(),
            appliedTools: Vec::new(),
        };
        let summaryBytes = summaryStorageBytes(&summary);
        let newRecord = TransactionRecord {
            summary,
            // 摘要按实际字段容量记账；预留固定上限会让完整 URI、备注和错误参数在写入前被迫裁剪。
            summaryStorageBytes: summaryBytes,
            requestHeaders: Vec::new(),
            responseHeaders: Vec::new(),
            requestBody: None,
            responseBody: None,
            requestPackets: Vec::new(),
            responsePackets: Vec::new(),
            requestHeadersTruncated: false,
            responseHeadersTruncated: false,
        };
        let newTransactionBytes = transactionMetadataBytes(&newRecord);
        let evictionIds = planTransactionAdmissionEvictions(&state, newTransactionBytes);
        let removedBodies = removeTransactions(&mut state, &evictionIds);
        enqueueBodyReferences(&mut state, removedBodies);
        if !evictionIds.is_empty() {
            // 终态事务淘汰已经改变公开集合，必须在任何磁盘 await 前发布新的集合令牌。
            self.notifyChanged();
        }
        self.drainCleanupQueueLocked(&mut state).await?;
        if !canAdmitTransaction(&state, newTransactionBytes) {
            // 活动事务不会为预算或数量让位；跳过当前录制但保持数据面继续转发。
            state.droppedCount = state.droppedCount.saturating_add(1);
            drop(state);
            self.notifyChanged();
            return Ok(None);
        }
        state.nextSequence = state.nextSequence.saturating_add(1);
        state.order.push_back(transactionId.clone());
        state.transactions.insert(transactionId.clone(), newRecord);
        state.totalMetadataBytes = state.totalMetadataBytes.saturating_add(newTransactionBytes);
        // 新事务只追加到 FIFO 尾部，既有 offset 对应的事务不会移动，因此沿用当前分页代际。
        // collectionVersion 只保护会让既有 offset 失真的头部淘汰与 clear；若每次追加都推进代际，
        // 高流量期间任何跨页读取都会被下一条事务打断，前端便会错误报告“较早事务加载失败”。
        drop(state);
        self.notifyChanged();
        Ok(Some(transactionId))
    }

    /// 原子更新 pending 事务的协议观测字段；终态后返回 TransactionFinished，防止线上事实被回写覆盖。
    pub async fn update(
        &self,
        transactionId: &str,
        update: TransactionUpdate,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let mut summary = pendingRecord(&mut state, transactionId)?.summary.clone();
        applyTransactionUpdate(&mut summary, boundTransactionUpdate(update));
        let summaryBytes = summaryStorageBytes(&summary);
        let removedBodies =
            replaceSummaryWithinMetadataBudget(&mut state, transactionId, summary, summaryBytes)?;
        enqueueBodyReferences(&mut state, removedBodies);
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 在 pending 阶段按字段合并大小与时间进度；终态后拒绝迟到任务，避免完成摘要继续漂移。
    pub async fn updateProgress(
        &self,
        transactionId: &str,
        update: TransactionProgressUpdate,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let record = pendingRecord(&mut state, transactionId)?;
        applyProgressUpdate(&mut record.summary, update);
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 更新用户维护的备注、标签和工具记录；终态允许编辑，但不存在或关闭会话仍返回结构化错误。
    pub async fn updateUserFields(
        &self,
        transactionId: &str,
        update: TransactionUserUpdate,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let record = state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        let mut summary = record.summary.clone();
        applyUserUpdate(&mut summary, boundUserUpdate(update));
        let summaryBytes = summaryStorageBytes(&summary);
        let removedBodies =
            replaceSummaryWithinMetadataBudget(&mut state, transactionId, summary, summaryBytes)?;
        enqueueBodyReferences(&mut state, removedBodies);
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 将事务迁移到 complete；重复完成或失败后的提交返回 TransactionFinished。
    pub async fn commit(
        &self,
        transactionId: &str,
        completion: TransactionCompletion,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let completion = boundCompletion(completion);
        let mut summary = pendingRecord(&mut state, transactionId)?.summary.clone();
        summary.status = TransactionStatus::Complete;
        summary.statusCode = Some(completion.statusCode);
        summary.contentType = completion.contentType;
        summary.timings.endAtMilliseconds = Some(completion.endAtMilliseconds);
        let summaryBytes = summaryStorageBytes(&summary);
        let removedBodies =
            finalizeSummaryWithinLimits(&mut state, transactionId, summary, summaryBytes)?;
        enqueueBodyReferences(&mut state, removedBodies);
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 将本地工具拦截的事务迁移到 blocked；保留合成响应状态码、正文和结束时间，
    /// 使检查器能够把“被工具阻断”与上游网络失败明确区分。
    pub async fn block(
        &self,
        transactionId: &str,
        completion: TransactionCompletion,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let completion = boundCompletion(completion);
        let mut summary = pendingRecord(&mut state, transactionId)?.summary.clone();
        summary.status = TransactionStatus::Blocked;
        summary.statusCode = Some(completion.statusCode);
        summary.contentType = completion.contentType;
        summary.timings.endAtMilliseconds = Some(completion.endAtMilliseconds);
        let summaryBytes = summaryStorageBytes(&summary);
        let removedBodies =
            finalizeSummaryWithinLimits(&mut state, transactionId, summary, summaryBytes)?;
        enqueueBodyReferences(&mut state, removedBodies);
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 将事务迁移到 failed 并保存未本地化错误描述；终态事务不会被二次覆盖。
    pub async fn fail(
        &self,
        transactionId: &str,
        error: TransactionError,
        endAtMilliseconds: u64,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let error = boundTransactionError(error);
        let mut summary = pendingRecord(&mut state, transactionId)?.summary.clone();
        summary.status = TransactionStatus::Failed;
        summary.error = Some(error);
        summary.timings.endAtMilliseconds = Some(endAtMilliseconds);
        let summaryBytes = summaryStorageBytes(&summary);
        let removedBodies =
            finalizeSummaryWithinLimits(&mut state, transactionId, summary, summaryBytes)?;
        enqueueBodyReferences(&mut state, removedBodies);
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 将事务迁移到 cancelled 并保存未本地化取消原因；终态事务不会被二次覆盖。
    pub async fn cancel(
        &self,
        transactionId: &str,
        error: TransactionError,
        endAtMilliseconds: u64,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let error = boundTransactionError(error);
        let mut summary = pendingRecord(&mut state, transactionId)?.summary.clone();
        summary.status = TransactionStatus::Cancelled;
        summary.error = Some(error);
        summary.timings.endAtMilliseconds = Some(endAtMilliseconds);
        let summaryBytes = summaryStorageBytes(&summary);
        let removedBodies =
            finalizeSummaryWithinLimits(&mut state, transactionId, summary, summaryBytes)?;
        enqueueBodyReferences(&mut state, removedBodies);
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 替换请求或响应头；预算允许范围内保留重复字段和原始顺序，不足时置 headersTruncated。
    pub async fn storeHeaders(
        &self,
        transactionId: &str,
        side: MessageSide,
        headers: Vec<HeaderField>,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        self.drainCleanupQueueLocked(&mut state).await?;
        let record = state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        if record.summary.status != TransactionStatus::Pending {
            return Err(CaptureError::TransactionFinished);
        }
        let oldHeaderBytes = match side {
            MessageSide::Request => {
                headerStorageBytes(&record.requestHeaders, record.requestHeaders.capacity())
            }
            MessageSide::Response => {
                headerStorageBytes(&record.responseHeaders, record.responseHeaders.capacity())
            }
        };
        let initiallyBounded = boundHeaders(headers, usize::MAX);
        let requiredIncrease = initiallyBounded.storedBytes.saturating_sub(oldHeaderBytes);
        let evictionIds = planMetadataBudgetEvictions(&state, transactionId, requiredIncrease);
        let removedBodies = removeTransactions(&mut state, &evictionIds);
        enqueueBodyReferences(&mut state, removedBodies);
        if !evictionIds.is_empty() {
            self.notifyChanged();
        }
        let availableBytes = state
            .metadataMemoryBudgetBytes
            .saturating_sub(state.totalMetadataBytes.saturating_sub(oldHeaderBytes));
        let bounded = if initiallyBounded.storedBytes > availableBytes {
            let mut reduced = boundHeaders(initiallyBounded.headers, availableBytes);
            reduced.truncated = true;
            reduced
        } else {
            initiallyBounded
        };
        let record = state
            .transactions
            .get_mut(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        match side {
            MessageSide::Request => {
                record.requestHeaders = bounded.headers;
                record.requestHeadersTruncated = bounded.truncated;
            }
            MessageSide::Response => {
                record.responseHeaders = bounded.headers;
                record.responseHeadersTruncated = bounded.truncated;
            }
        }
        record.refreshTruncatedFlag();
        state.totalMetadataBytes = state
            .totalMetadataBytes
            .saturating_sub(oldHeaderBytes)
            .saturating_add(bounded.storedBytes);
        self.notifyChanged();
        let cleanupResult = self.drainCleanupQueueLocked(&mut state).await;
        drop(state);
        cleanupResult
    }

    /// 存储一侧完整正文；生产会话的只读限额为安全整数上限，不裁剪正文也不淘汰事务。
    /// 参数：transactionId 和 side 定位唯一正文，body.originalBytes 必须不小于实际字节长度。
    /// 失败语义：长度不一致、事务不存在或存储 I/O 失败均原样返回，不生成前缀正文。
    pub async fn storeBody(
        &self,
        transactionId: &str,
        side: MessageSide,
        body: BodyWrite,
    ) -> Result<BodyHandleMeta, CaptureError> {
        let body = boundBodyWrite(body);
        if body.originalBytes < body.bytes.len() as u64 {
            return Err(CaptureError::InvalidBodyLength);
        }
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        self.drainCleanupQueueLocked(&mut state).await?;
        let oldBody = state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        if oldBody.summary.status != TransactionStatus::Pending {
            return Err(CaptureError::TransactionFinished);
        }
        let oldBody = oldBody.body(side).cloned();
        let oldBodyBytes = oldBody
            .as_ref()
            .map_or(0, |bodyReference| bodyReference.meta().storedBytes);
        let desiredBytes = body.bytes.len().min(state.limits.maxBodyBytes);
        let baseTotalBytes = state.totalBodyBytes.saturating_sub(oldBodyBytes);
        let evictionIds =
            planBodyBudgetEvictions(&state, transactionId, baseTotalBytes, desiredBytes);
        let freedBytes = evictionIds
            .iter()
            .filter_map(|identifier| state.transactions.get(identifier))
            .map(transactionBodyBytes)
            .sum::<usize>();
        let retainedBytes = baseTotalBytes.saturating_sub(freedBytes);
        let availableBytes = state.limits.maxTotalBodyBytes.saturating_sub(retainedBytes);
        let storedBytes = desiredBytes.min(availableBytes);
        let meta = BodyHandleMeta {
            transactionId: transactionId.to_owned(),
            side,
            contentType: body.contentType,
            encoding: body.encoding,
            storedBytes,
            originalBytes: body.originalBytes,
            truncated: body.originalBytes > storedBytes as u64,
        };
        let stagedBody = self
            .inner
            .bodyStore
            .store(
                transactionId,
                side,
                &body.bytes[..storedBytes],
                meta.clone(),
            )
            .await?;
        let mut removedBodies = removeTransactions(&mut state, &evictionIds);
        if let Some(oldBody) = oldBody {
            state.totalBodyBytes = state.totalBodyBytes.saturating_sub(oldBodyBytes);
            removedBodies.push(oldBody);
        }
        state.totalBodyBytes = state.totalBodyBytes.saturating_add(storedBytes);
        let record = state
            .transactions
            .get_mut(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        record.replaceBody(side, stagedBody.commit());
        match side {
            MessageSide::Request => record.summary.sizes.requestBodyBytes = body.originalBytes,
            MessageSide::Response => record.summary.sizes.responseBodyBytes = body.originalBytes,
        }
        record.refreshTruncatedFlag();
        enqueueBodyReferences(&mut state, removedBodies);
        // 从这里起正文引用、预算与淘汰结果均已成为权威状态；先通知再做可取消清理，避免漏掉删除事件。
        self.notifyChanged();
        // 正文已完成权威登记后才进入清理阶段；清理失败会保留队列并返回错误，重试不会丢失引用。
        let cleanupResult = self.drainCleanupQueueLocked(&mut state).await;
        drop(state);
        cleanupResult?;
        Ok(meta)
    }

    /// 为 pending 事务的一侧创建增量 spool；长连接从首包开始落盘，不在内存累积完整正文。
    ///
    /// 运行上下文：透明 TCP/TLS 中继在开始转发前调用一次，请求和响应分别持有独立 spool。
    /// 失败语义：事务不存在、已经终态或临时文件创建失败时返回错误，调用方不得静默跳过录制。
    pub async fn createBodySpool(
        &self,
        transactionId: &str,
        side: MessageSide,
    ) -> Result<BodySpool, CaptureError> {
        {
            let state = self.inner.state.read().await;
            ensureOpen(&state)?;
            let record = state
                .transactions
                .get(transactionId)
                .ok_or(CaptureError::TransactionNotFound)?;
            if record.summary.status != TransactionStatus::Pending {
                return Err(CaptureError::TransactionFinished);
            }
        }
        self.inner.bodyStore.createSpool(transactionId, side).await
    }

    /// 将请求和响应 spool 原子绑定到同一 pending 事务；任一方向失败都不会留下单侧正文。
    ///
    /// 运行上下文：双向中继结束或被取消后由后台录制任务调用，文件同步不占用会话状态锁。
    /// 失败语义：正文超过会话只读预算、事务已清除或文件提交失败时返回错误，两个 staged 文件由守卫回收。
    pub async fn storeBodySpools(
        &self,
        transactionId: &str,
        requestSpool: BodySpool,
        responseSpool: BodySpool,
        contentType: &str,
        encoding: &str,
    ) -> Result<(BodyHandleMeta, BodyHandleMeta), CaptureError> {
        let requestBytes = requestSpool.writtenBytes();
        let responseBytes = responseSpool.writtenBytes();
        let requestMeta = BodyHandleMeta {
            transactionId: transactionId.to_owned(),
            side: MessageSide::Request,
            contentType: contentType.to_owned(),
            encoding: encoding.to_owned(),
            storedBytes: requestBytes,
            originalBytes: requestBytes as u64,
            truncated: false,
        };
        let responseMeta = BodyHandleMeta {
            transactionId: transactionId.to_owned(),
            side: MessageSide::Response,
            contentType: contentType.to_owned(),
            encoding: encoding.to_owned(),
            storedBytes: responseBytes,
            originalBytes: responseBytes as u64,
            truncated: false,
        };
        let requestBody = self
            .inner
            .bodyStore
            .stageSpool(requestSpool, requestMeta.clone())
            .await?;
        let responseBody = self
            .inner
            .bodyStore
            .stageSpool(responseSpool, responseMeta.clone())
            .await?;

        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let record = state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        if record.summary.status != TransactionStatus::Pending {
            return Err(CaptureError::TransactionFinished);
        }
        let oldRequest = record.requestBody.clone();
        let oldResponse = record.responseBody.clone();
        let oldBytes = oldRequest
            .as_ref()
            .map_or(0, |body| body.meta().storedBytes)
            .saturating_add(
                oldResponse
                    .as_ref()
                    .map_or(0, |body| body.meta().storedBytes),
            );
        let newBytes = requestBytes.saturating_add(responseBytes);
        let retainedBytes = state.totalBodyBytes.saturating_sub(oldBytes);
        if requestBytes > state.limits.maxBodyBytes
            || responseBytes > state.limits.maxBodyBytes
            || newBytes > state.limits.maxTotalBodyBytes.saturating_sub(retainedBytes)
        {
            // 流式正文已经完整落盘；预算不足时必须显式失败，禁止把完整文件裁成前缀后伪装成功。
            return Err(CaptureError::InvalidLimits);
        }
        let requestBody = requestBody.commit();
        let responseBody = responseBody.commit();
        let record = state
            .transactions
            .get_mut(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        record.requestBody = Some(requestBody);
        record.responseBody = Some(responseBody);
        record.summary.sizes.requestBodyBytes = requestBytes as u64;
        record.summary.sizes.responseBodyBytes = responseBytes as u64;
        record.refreshTruncatedFlag();
        state.totalBodyBytes = retainedBytes.saturating_add(newBytes);
        let mut removedBodies = Vec::with_capacity(2);
        if let Some(oldRequest) = oldRequest {
            removedBodies.push(oldRequest);
        }
        if let Some(oldResponse) = oldResponse {
            removedBodies.push(oldResponse);
        }
        enqueueBodyReferences(&mut state, removedBodies);
        self.notifyChanged();
        let cleanupResult = self.drainCleanupQueueLocked(&mut state).await;
        drop(state);
        cleanupResult?;
        Ok((requestMeta, responseMeta))
    }

    /// 返回按 sequence 排列的事务摘要；结果结构不含 HeaderField、BodyRef 或正文 bytes。
    pub async fn listMetadata(&self) -> Result<Vec<TransactionSummary>, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        Ok(state
            .order
            .iter()
            .filter_map(|identifier| state.transactions.get(identifier))
            .map(|record| record.summary.clone())
            .collect())
    }

    /// 返回单条事务摘要；被 FIFO 淘汰或清空后返回 TransactionNotFound。
    pub async fn getTransaction(
        &self,
        transactionId: &str,
    ) -> Result<TransactionSummary, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        state
            .transactions
            .get(transactionId)
            .map(|record| record.summary.clone())
            .ok_or(CaptureError::TransactionNotFound)
    }

    /// 在单个读锁内返回详情所需全部非正文字节字段；与 clear 线性化为完整详情或稳定 NotFound。
    pub async fn getTransactionDetail(
        &self,
        transactionId: &str,
    ) -> Result<TransactionDetailRecord, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        let record = state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        Ok(record.detail())
    }

    /// 从二级索引读取同一完整 URL、强 ETag、总长和编码的最小 Range 元数据。
    ///
    /// 查询复杂度只与该实体版本的候选数相关，不扫描事务表，也不克隆摘要或响应头。
    /// 返回结果按 sequence 排列且不包含 BodyRef；调用方完成区间规划后才能为最终 ID 建立租约。
    /// 失败语义：会话关闭返回 CaptureError；实体不存在返回空集合。
    pub async fn findResponseRangeCandidates(
        &self,
        urlDisplay: &str,
        entityTag: &str,
        totalBytes: u64,
        contentEncoding: &str,
    ) -> Result<Vec<ResponseRangeCandidate>, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        let key = ResponseEntityIndexKey {
            urlDisplay: urlDisplay.to_owned(),
            entityTag: entityTag.to_owned(),
            totalBytes,
            contentEncoding: contentEncoding.to_ascii_lowercase(),
        };
        let Some(members) = state.responseEntityIndex.get(&key) else {
            return Ok(Vec::new());
        };
        Ok(members
            .iter()
            .filter_map(|(_, identifier)| state.transactions.get(identifier))
            .filter_map(responseRangeCandidate)
            .collect())
    }

    /// 按需返回指定消息侧的全部头字段；正文与列表不受此调用影响。
    pub async fn getHeaders(
        &self,
        transactionId: &str,
        side: MessageSide,
    ) -> Result<Vec<HeaderField>, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        let record = state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        Ok(match side {
            MessageSide::Request => record.requestHeaders.clone(),
            MessageSide::Response => record.responseHeaders.clone(),
        })
    }

    /// 返回不含字节的正文引用，供详情接口读取 meta 或诊断内存/spill 介质。
    pub async fn getBodyRef(
        &self,
        transactionId: &str,
        side: MessageSide,
    ) -> Result<BodyRef, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?
            .body(side)
            .cloned()
            .ok_or(CaptureError::BodyNotFound)
    }

    /// 在事务仍位于权威表时打开单个稳定正文租约，供长时间流式响应脱离 FIFO 生命周期读取。
    ///
    /// 会话读锁会一直持有到 spill 文件完成打开和长度校验，因而 clear/淘汰无法插入
    /// “克隆路径后、打开文件前”的竞态。事务或正文不存在及底层 I/O 失败均直接返回错误。
    pub async fn getBodyReadLease(
        &self,
        transactionId: &str,
        side: MessageSide,
    ) -> Result<BodyReadLease, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        let bodyReference = state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?
            .body(side)
            .ok_or(CaptureError::BodyNotFound)?;
        self.inner.bodyStore.lease(bodyReference).await
    }

    /// 在一个线性化读锁内只为最终规划事务打开稳定正文租约，返回顺序与输入标识完全一致。
    ///
    /// 元数据索引查询和区间规划不持有本锁；本方法只处理最终分段，通常为少量句柄。
    /// clear/FIFO 若在规划后先获得写锁，则缺失事务返回 TransactionNotFound，禁止发送半计划正文。
    pub async fn getBodyReadLeases(
        &self,
        transactionIds: &[String],
        side: MessageSide,
    ) -> Result<Vec<BodyReadLease>, CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        let mut leases = Vec::with_capacity(transactionIds.len());
        for transactionId in transactionIds {
            let bodyReference = state
                .transactions
                .get(transactionId)
                .ok_or(CaptureError::TransactionNotFound)?
                .body(side)
                .ok_or(CaptureError::BodyNotFound)?;
            leases.push(self.inner.bodyStore.lease(bodyReference).await?);
        }
        Ok(leases)
    }

    /// 保存单侧有界流片段索引；片段只引用已录制的聚合正文范围，避免高频转发为每个片段复制正文。
    pub async fn storeStreamPackets(
        &self,
        transactionId: &str,
        side: MessageSide,
        packets: Vec<StreamPacket>,
    ) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let record = state
            .transactions
            .get(transactionId)
            .ok_or(CaptureError::TransactionNotFound)?;
        let bodyLength = record
            .body(side)
            .map_or(0, |bodyReference| bodyReference.meta().storedBytes);
        if packets.iter().any(|packet| {
            packet.storedBytes == 0
                || packet.originalBytes < packet.storedBytes as u64
                || packet.truncated != (packet.originalBytes > packet.storedBytes as u64)
                || packet
                    .storedOffsetBytes
                    .checked_add(packet.storedBytes)
                    .is_none_or(|end| end > bodyLength)
        }) {
            return Err(CaptureError::InvalidBodyLength);
        }
        let removedBodies =
            replaceStreamPacketsWithinMetadataBudget(&mut state, transactionId, side, packets)?;
        enqueueBodyReferences(&mut state, removedBodies);
        self.notifyChanged();
        // 片段索引替换也可能淘汰最旧终态事务；在返回成功前回收其正文引用，避免索引更新留下待清理文件。
        let cleanupResult = self.drainCleanupQueueLocked(&mut state).await;
        drop(state);
        cleanupResult
    }

    /// 按需读取一侧正文；读取前只短暂持有状态锁，磁盘 I/O 不阻塞其它事务更新。
    pub async fn getBody(
        &self,
        transactionId: &str,
        side: MessageSide,
    ) -> Result<BodyResponse, CaptureError> {
        let bodyReference = self.getBodyRef(transactionId, side).await?;
        let bytes = self.inner.bodyStore.read(&bodyReference).await?;
        Ok(BodyResponse {
            meta: bodyReference.meta().clone(),
            bytes,
        })
    }

    /// 按正文偏移读取单个有界分块，供控制面的惰性二进制响应遵循网络背压。
    ///
    /// 运行上下文：调用先在短读锁内克隆稳定 BodyRef，随后释放状态锁再执行内存切片或
    /// spill 文件 I/O；不会把完整媒体聚合进控制请求内存。`maximumBytes` 是本次读取上限。
    /// 失败语义：事务/正文不存在、偏移越界、spill 被清理或读取失败时返回 CaptureError，
    /// 已经开始的 HTTP 流应以传输错误终止而不是伪造较短成功响应。
    pub async fn getBodyChunk(
        &self,
        transactionId: &str,
        side: MessageSide,
        offset: usize,
        maximumBytes: usize,
    ) -> Result<BodyResponse, CaptureError> {
        let bodyReference = self.getBodyRef(transactionId, side).await?;
        let bytes = self
            .inner
            .bodyStore
            .readRange(&bodyReference, offset, maximumBytes)
            .await?;
        Ok(BodyResponse {
            meta: bodyReference.meta().clone(),
            bytes,
        })
    }

    /// 保留旧调用方的限额入口但禁止改变权威值；录制集合只能由显式 clear 删除。
    ///
    /// 运行上下文：相同值写回保持幂等，任何不同值返回 `InvalidLimits`，且不会触发正文回收。
    pub async fn setLimits(&self, limits: RecordingLimits) -> Result<(), CaptureError> {
        let state = self.inner.state.read().await;
        ensureOpen(&state)?;
        if limits != state.limits {
            return Err(CaptureError::InvalidLimits);
        }
        Ok(())
    }

    /// 原子替换忽略 Location；任一规则无效时保持原配置不变。
    pub async fn setIgnoreLocations(
        &self,
        patterns: Vec<LocationPattern>,
    ) -> Result<(), CaptureError> {
        for pattern in &patterns {
            validateLocationPattern(pattern)?;
        }
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        state.ignoreLocations = patterns;
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 控制未解密 CONNECT 是否创建隧道元数据；不会删除已有事务。
    pub async fn setRecordTunnelMetadata(&self, enabled: bool) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        state.recordTunnelMetadata = enabled;
        drop(state);
        self.notifyChanged();
        Ok(())
    }

    /// 先校验整组设置，再在一个写锁内一次提交；录制限额是只读兼容字段，运行期不得修改。
    /// 参数：update 只允许录制状态、忽略规则与隧道元数据开关。
    /// 失败语义：携带 limits 返回 `InvalidLimits`，其它字段也不会部分生效。
    pub async fn updateSettings(
        &self,
        update: RecordingSettingsUpdate,
    ) -> Result<(), CaptureError> {
        if update.limits.is_some() {
            return Err(CaptureError::InvalidLimits);
        }
        if let Some(patterns) = update.ignoreLocations.as_ref() {
            for pattern in patterns {
                validateLocationPattern(pattern)?;
            }
        }
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        if let Some(recordingState) = update.state {
            state.recordingState = recordingState;
        }
        if let Some(patterns) = update.ignoreLocations {
            state.ignoreLocations = patterns;
        }
        if let Some(enabled) = update.recordTunnelMetadata {
            state.recordTunnelMetadata = enabled;
        }
        // 设置更新不再触发资源淘汰；只有用户明确 clear 才能删除已经录制的事务和正文。
        self.notifyChanged();
        let cleanupResult = self.drainCleanupQueueLocked(&mut state).await;
        drop(state);
        cleanupResult
    }

    /// 清除全部事务、头和正文；清理失败时事务保持不可见，待清理引用保留并可再次调用重试。
    pub async fn clearSession(&self) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let removedBodies = removeAllTransactions(&mut state);
        markCollectionChanged(&mut state);
        state.directoryCleanupPending = true;
        enqueueBodyReferences(&mut state, removedBodies);
        // 元数据和正文引用已从可见集合移除；目录清理被取消时仍要发布空集合。
        self.notifyChanged();
        let cleanupResult = self.drainCleanupQueueLocked(&mut state).await;
        drop(state);
        cleanupResult
    }

    /// 关闭会话并删除 spill 目录；失败或取消时保持可重试，只有全部资源删除成功后才进入 closed。
    pub async fn close(&self) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        if state.closed {
            return Ok(());
        }
        let removedBodies = removeAllTransactions(&mut state);
        markCollectionChanged(&mut state);
        enqueueBodyReferences(&mut state, removedBodies);
        // close 进入磁盘清理前已移除全部公开事务；取消关闭也必须让观察者看到空集合。
        self.notifyChanged();
        self.drainCleanupQueueLocked(&mut state).await?;
        self.inner.bodyStore.close().await?;
        state.closed = true;
        Ok(())
    }

    /// 重试 FIFO、限额调整或清空留下的待清理正文；失败项保持队首且不会被后续成功掩盖。
    pub async fn cleanupPendingBodies(&self) -> Result<(), CaptureError> {
        let mut state = self.inner.state.write().await;
        ensureOpen(&state)?;
        let cleanupResult = self.drainCleanupQueueLocked(&mut state).await;
        drop(state);
        if cleanupResult.is_ok() {
            self.notifyChanged();
        }
        cleanupResult
    }

    /// 在会话写锁内逐项回收 tombstone；取消时当前队首仍在，重复删除按 BodyStore 幂等语义继续。
    async fn drainCleanupQueueLocked(
        &self,
        state: &mut RecordingStateInner,
    ) -> Result<(), CaptureError> {
        while let Some(bodyReference) = state.cleanupQueue.front().cloned() {
            self.inner.bodyStore.remove(&bodyReference).await?;
            let removedReference = state
                .cleanupQueue
                .pop_front()
                .expect("cleanupQueueFrontMustExist");
            state.totalMetadataBytes = state
                .totalMetadataBytes
                .saturating_sub(cleanupBodyReferenceMetadataBytes(&removedReference));
            // 每个 tombstone 一经移除就立即发布，保证下一次可取消 await 不会吞掉健康状态变化。
            self.notifyChanged();
        }
        let previousOrphanCount = self.inner.bodyStore.pendingOrphanCount();
        self.inner.bodyStore.cleanupOrphanedSpills().await?;
        if self.inner.bodyStore.pendingOrphanCount() != previousOrphanCount {
            self.notifyChanged();
        }
        if state.directoryCleanupPending {
            self.inner.bodyStore.clear().await?;
            state.directoryCleanupPending = false;
            self.notifyChanged();
        }
        Ok(())
    }

    /// 返回正文当前介质类别，避免调用方为诊断目的接触内部路径。
    pub async fn getBodyStorageKind(
        &self,
        transactionId: &str,
        side: MessageSide,
    ) -> Result<BodyStorageKind, CaptureError> {
        Ok(self.getBodyRef(transactionId, side).await?.storageKind())
    }
}

/// 检查会话是否仍接受操作；关闭状态统一返回 SessionClosed。
fn ensureOpen(state: &RecordingStateInner) -> Result<(), CaptureError> {
    if state.closed {
        Err(CaptureError::SessionClosed)
    } else {
        Ok(())
    }
}

/// 从锁内状态构建不含正文的公开快照。
fn snapshotFromState(state: &RecordingStateInner, pendingOrphanCount: usize) -> RecordingSnapshot {
    RecordingSnapshot {
        recordingSessionId: state.recordingSessionId.clone(),
        state: state.recordingState,
        startedAtMilliseconds: state.startedAtMilliseconds,
        transactionCount: state.transactions.len(),
        droppedCount: state.droppedCount,
        totalBodyBytes: state.totalBodyBytes,
        totalMetadataBytes: state.totalMetadataBytes,
        metadataMemoryBudgetBytes: state.metadataMemoryBudgetBytes,
        pendingCleanupCount: state.cleanupQueue.len()
            + pendingOrphanCount
            + usize::from(state.directoryCleanupPending),
        limits: state.limits,
        ignoreLocations: state.ignoreLocations.clone(),
        recordTunnelMetadata: state.recordTunnelMetadata,
    }
}

/// 在会话读锁内生成稳定分页代际；尾部追加保持既有 offset，头部淘汰与清空才推进代际。
fn collectionTokenFromState(state: &RecordingStateInner) -> String {
    format!("{}:{}", state.recordingSessionId, state.collectionVersion)
}

/// 标记既有分页 offset 已经失效；调用方必须持有会话写锁且只用于头部移除或全量清空。
fn markCollectionChanged(state: &mut RecordingStateInner) {
    state.collectionVersion = state.collectionVersion.saturating_add(1);
}

/// 在已校验规则集合上判断暂停与忽略语义；候选错误继续向调用方传播。
fn shouldRecordFromState(
    state: &RecordingStateInner,
    location: &ResolvedLocation,
) -> Result<bool, CaptureError> {
    if state.recordingState == RecordingState::Paused {
        return Ok(false);
    }
    for pattern in &state.ignoreLocations {
        if locationMatches(pattern, location, LocationMatchOptions::default())? {
            return Ok(false);
        }
    }
    locationMatches(
        &LocationPattern::default(),
        location,
        LocationMatchOptions::default(),
    )
    .map_err(CaptureError::from)
}

/// 判断事务数量与新 pending 事务完整预留是否允许接纳；活动事务永远不会在此路径被删除。
fn canAdmitTransaction(state: &RecordingStateInner, newTransactionBytes: usize) -> bool {
    state.transactions.len() < state.limits.maxTransactions
        && state.totalMetadataBytes.saturating_add(newTransactionBytes)
            <= state.metadataMemoryBudgetBytes
}

/// 规划新事务接纳前可淘汰的最旧终态集合，同时满足数量和元数据预算才停止。
fn planTransactionAdmissionEvictions(
    state: &RecordingStateInner,
    newTransactionBytes: usize,
) -> Vec<String> {
    let mut retainedCount = state.transactions.len();
    let mut retainedMetadataBytes = state.totalMetadataBytes;
    let mut evictionIds = Vec::new();
    for transactionId in &state.order {
        if retainedCount < state.limits.maxTransactions
            && retainedMetadataBytes.saturating_add(newTransactionBytes)
                <= state.metadataMemoryBudgetBytes
        {
            break;
        }
        let Some(record) = state.transactions.get(transactionId) else {
            continue;
        };
        if record.summary.status == TransactionStatus::Pending {
            continue;
        }
        retainedCount = retainedCount.saturating_sub(1);
        retainedMetadataBytes =
            retainedMetadataBytes.saturating_sub(transactionMetadataBytes(record));
        evictionIds.push(transactionId.clone());
    }
    evictionIds
}

/// 在 limits 变小时仅淘汰终态事务，活动转发链保留到完成后再参与后续收敛。
fn enforceAllLimits(state: &mut RecordingStateInner) -> Vec<BodyRef> {
    let mut retainedCount = state.transactions.len();
    let mut retainedBodyBytes = state.totalBodyBytes;
    let mut evictionIds = Vec::new();
    for transactionId in &state.order {
        if retainedCount <= state.limits.maxTransactions
            && retainedBodyBytes <= state.limits.maxTotalBodyBytes
        {
            break;
        }
        let Some(record) = state.transactions.get(transactionId) else {
            continue;
        };
        if record.summary.status == TransactionStatus::Pending {
            continue;
        }
        retainedCount = retainedCount.saturating_sub(1);
        retainedBodyBytes = retainedBodyBytes.saturating_sub(transactionBodyBytes(record));
        evictionIds.push(transactionId.clone());
    }
    removeTransactions(state, &evictionIds)
}

/// 把已从权威事务表移除的正文引用转成可重试 tombstone；引用成功删除前仍计入全局元数据总账。
fn enqueueBodyReferences(state: &mut RecordingStateInner, bodyReferences: Vec<BodyRef>) {
    let metadataBytes = bodyReferences
        .iter()
        .map(cleanupBodyReferenceMetadataBytes)
        .sum::<usize>();
    state.totalMetadataBytes = state.totalMetadataBytes.saturating_add(metadataBytes);
    state.cleanupQueue.extend(bodyReferences);
}

/// 计算总正文预算不足时要移除的最旧其它事务；当前正在写体的事务永不自我淘汰。
fn planBodyBudgetEvictions(
    state: &RecordingStateInner,
    currentTransactionId: &str,
    baseTotalBytes: usize,
    desiredBytes: usize,
) -> Vec<String> {
    let mut requiredBytes = baseTotalBytes
        .saturating_add(desiredBytes)
        .saturating_sub(state.limits.maxTotalBodyBytes);
    let mut evictionIds = Vec::new();
    if requiredBytes == 0 {
        return evictionIds;
    }
    for transactionId in &state.order {
        if transactionId == currentTransactionId {
            continue;
        }
        let Some(record) = state.transactions.get(transactionId) else {
            continue;
        };
        if record.summary.status == TransactionStatus::Pending {
            continue;
        }
        requiredBytes = requiredBytes.saturating_sub(transactionBodyBytes(record));
        evictionIds.push(transactionId.clone());
        if requiredBytes == 0 {
            break;
        }
    }
    evictionIds
}

/// 规划头写入所需的终态事务淘汰；当前事务和所有活动事务始终保留。
fn planMetadataBudgetEvictions(
    state: &RecordingStateInner,
    currentTransactionId: &str,
    requiredIncrease: usize,
) -> Vec<String> {
    let mut requiredBytes = state
        .totalMetadataBytes
        .saturating_add(requiredIncrease)
        .saturating_sub(state.metadataMemoryBudgetBytes);
    let mut evictionIds = Vec::new();
    for transactionId in &state.order {
        if requiredBytes == 0 {
            break;
        }
        if transactionId == currentTransactionId {
            continue;
        }
        let Some(record) = state.transactions.get(transactionId) else {
            continue;
        };
        if record.summary.status == TransactionStatus::Pending {
            continue;
        }
        // 正文引用移入 cleanupQueue 后仍持有元数据，只有事务记录与索引部分可立即释放。
        requiredBytes = requiredBytes.saturating_sub(
            transactionMetadataBytes(record)
                .saturating_sub(cleanupBodyReferencesMetadataBytes(record)),
        );
        evictionIds.push(transactionId.clone());
    }
    evictionIds
}

/// 批量移除预算规划选中的事务，并一次整理顺序队列避免重复线性扫描。
fn removeTransactions(state: &mut RecordingStateInner, transactionIds: &[String]) -> Vec<BodyRef> {
    if transactionIds.is_empty() {
        return Vec::new();
    }
    let identifierSet: HashSet<&str> = transactionIds.iter().map(String::as_str).collect();
    state
        .order
        .retain(|identifier| !identifierSet.contains(identifier.as_str()));
    let mut removedBodies = Vec::new();
    let mut removedAny = false;
    for transactionId in transactionIds {
        let Some(record) = state.transactions.remove(transactionId) else {
            continue;
        };
        removeResponseEntityEntry(state, &record);
        removedAny = true;
        state.totalBodyBytes = state
            .totalBodyBytes
            .saturating_sub(transactionBodyBytes(&record));
        state.totalMetadataBytes = state
            .totalMetadataBytes
            .saturating_sub(transactionMetadataBytes(&record));
        state.droppedCount = state.droppedCount.saturating_add(1);
        removedBodies.extend(record.intoBodyReferences());
    }
    if removedAny {
        markCollectionChanged(state);
    }
    removedBodies
}

/// 移除当前全部公开事务，同时保留既有 cleanupQueue 的元数据计费；clear/close 仅转移正文清理所有权。
fn removeAllTransactions(state: &mut RecordingStateInner) -> Vec<BodyRef> {
    // clear/close 在同一写锁内同时清空事务表和二级索引，之后的规划查询只会看到空集合。
    state.responseEntityIndex.clear();
    let removedRecords = state
        .transactions
        .drain()
        .map(|(_, record)| record)
        .collect::<Vec<_>>();
    let removedMetadataBytes = removedRecords
        .iter()
        .map(transactionMetadataBytes)
        .sum::<usize>();
    state.order.clear();
    state.totalBodyBytes = 0;
    state.totalMetadataBytes = state
        .totalMetadataBytes
        .saturating_sub(removedMetadataBytes);
    removedRecords
        .into_iter()
        .flat_map(TransactionRecord::intoBodyReferences)
        .collect()
}

/// 汇总事务两侧实际存储字节，供 FIFO 预算精确回收。
fn transactionBodyBytes(record: &TransactionRecord) -> usize {
    record
        .requestBody
        .iter()
        .chain(record.responseBody.iter())
        .map(|bodyReference| bodyReference.meta().storedBytes)
        .sum()
}

/// 保守估算单个事务在实体二级索引中的逻辑占用；共享键按事务重复计费以确保预算不低估。
fn responseEntityIndexMetadataBytes(
    summary: &TransactionSummary,
    responseHeaders: &[HeaderField],
    responseBody: Option<&BodyRef>,
) -> usize {
    if summary.status != TransactionStatus::Complete || summary.statusCode != Some(206) {
        return 0;
    }
    let Some(entityTag) = strongResponseEntityTag(responseHeaders) else {
        return 0;
    };
    let Some((start, end, _)) = responseContentRange(responseHeaders) else {
        return 0;
    };
    let Some(body) = responseBody.map(BodyRef::meta) else {
        return 0;
    };
    let Some(rangeBytes) = end
        .checked_sub(start)
        .and_then(|bytes| bytes.checked_add(1))
    else {
        return 0;
    };
    if body.truncated || body.originalBytes != rangeBytes || body.storedBytes as u64 != rangeBytes {
        return 0;
    }
    size_of::<ResponseEntityIndexKey>()
        .saturating_add(summary.urlDisplay.capacity())
        .saturating_add(entityTag.len())
        .saturating_add(body.encoding.capacity())
        .saturating_add(size_of::<(u64, String)>())
        .saturating_add(summary.transactionId.capacity())
        // 覆盖 HashMap 桶和 BTree 节点中的指针、长度及分配器边界，宁可保守高估。
        .saturating_add(size_of::<usize>().saturating_mul(8))
}

/// 计算事务记录、索引副本、摘要计费、头与正文引用元数据的保守逻辑占用。
fn transactionMetadataBytes(record: &TransactionRecord) -> usize {
    let transactionIdBytes =
        size_of::<String>().saturating_add(record.summary.transactionId.capacity());
    let recordFixedBytes =
        size_of::<TransactionRecord>().saturating_sub(size_of::<TransactionSummary>());
    recordFixedBytes
        .saturating_add(record.summaryStorageBytes)
        // HashMap 键与 VecDeque 顺序项各持有一份事务标识；两个 usize 覆盖哈希桶与顺序槽开销。
        .saturating_add(transactionIdBytes.saturating_mul(2))
        .saturating_add(size_of::<usize>().saturating_mul(2))
        .saturating_add(responseEntityIndexMetadataBytes(
            &record.summary,
            &record.responseHeaders,
            record.responseBody.as_ref(),
        ))
        .saturating_add(headerStorageBytes(
            &record.requestHeaders,
            record.requestHeaders.capacity(),
        ))
        .saturating_add(headerStorageBytes(
            &record.responseHeaders,
            record.responseHeaders.capacity(),
        ))
        .saturating_add(streamPacketStorageBytes(
            &record.requestPackets,
            record.requestPackets.capacity(),
        ))
        .saturating_add(streamPacketStorageBytes(
            &record.responsePackets,
            record.responsePackets.capacity(),
        ))
        .saturating_add(if record.summary.status == TransactionStatus::Pending {
            // pending 的 64 KiB 摘要计费同时预留两侧正文引用元数据，避免代理热路径因小额元信息失败。
            0
        } else {
            bodyReferenceMetadataBytes(record)
        })
}

/// 计算流片段索引的固定槽位容量；片段不携带正文副本，因此不存在额外的动态字符串或字节计费。
fn streamPacketStorageBytes(packets: &[StreamPacket], capacity: usize) -> usize {
    debug_assert!(capacity >= packets.len());
    size_of::<StreamPacket>().saturating_mul(capacity)
}

/// 汇总两侧正文引用在正文实际字节之外占用的路径与文本容量。
fn bodyReferenceMetadataBytes(record: &TransactionRecord) -> usize {
    record
        .requestBody
        .iter()
        .chain(record.responseBody.iter())
        .map(BodyRef::metadataStorageBytes)
        .sum()
}

/// 计算 cleanupQueue 单项的固定槽位和动态文本/路径容量；正文实际字节仍由正文预算独立管理。
fn cleanupBodyReferenceMetadataBytes(bodyReference: &BodyRef) -> usize {
    size_of::<BodyRef>().saturating_add(bodyReference.metadataStorageBytes())
}

/// 汇总事务移除后会转入 cleanupQueue 的正文引用计费，用于预算规划区分立即释放与延迟释放。
fn cleanupBodyReferencesMetadataBytes(record: &TransactionRecord) -> usize {
    record
        .requestBody
        .iter()
        .chain(record.responseBody.iter())
        .map(cleanupBodyReferenceMetadataBytes)
        .sum()
}

/// 原子替换单侧流片段索引并遵守会话元数据预算；预算不足时只淘汰其它终态事务，当前记录保持完整不被半写入。
fn replaceStreamPacketsWithinMetadataBudget(
    state: &mut RecordingStateInner,
    transactionId: &str,
    side: MessageSide,
    packets: Vec<StreamPacket>,
) -> Result<Vec<BodyRef>, CaptureError> {
    let currentRecord = state
        .transactions
        .get(transactionId)
        .ok_or(CaptureError::TransactionNotFound)?;
    let oldTransactionBytes = transactionMetadataBytes(currentRecord);
    let mut candidateRecord = currentRecord.clone();
    match side {
        MessageSide::Request => candidateRecord.requestPackets = packets,
        MessageSide::Response => candidateRecord.responsePackets = packets,
    }
    let newTransactionBytes = transactionMetadataBytes(&candidateRecord);
    let requiredIncrease = newTransactionBytes.saturating_sub(oldTransactionBytes);
    let evictionIds = planMetadataBudgetEvictions(state, transactionId, requiredIncrease);
    let freedBytes = evictionIds
        .iter()
        .filter_map(|identifier| state.transactions.get(identifier))
        .map(transactionMetadataBytes)
        .sum::<usize>();
    let deferredCleanupBytes = evictionIds
        .iter()
        .filter_map(|identifier| state.transactions.get(identifier))
        .map(cleanupBodyReferencesMetadataBytes)
        .sum::<usize>();
    let projectedVisibleBytes = state
        .totalMetadataBytes
        .saturating_sub(oldTransactionBytes)
        .saturating_sub(freedBytes)
        .saturating_add(newTransactionBytes);
    if projectedVisibleBytes.saturating_add(deferredCleanupBytes) > state.metadataMemoryBudgetBytes
    {
        return Err(CaptureError::MetadataMemoryBudgetExceeded);
    }
    let removedBodies = removeTransactions(state, &evictionIds);
    let record = state
        .transactions
        .get_mut(transactionId)
        .ok_or(CaptureError::TransactionNotFound)?;
    match side {
        MessageSide::Request => record.requestPackets = candidateRecord.requestPackets,
        MessageSide::Response => record.responsePackets = candidateRecord.responsePackets,
    }
    state.totalMetadataBytes = projectedVisibleBytes;
    Ok(removedBodies)
}

/// 原子替换摘要并按实际逻辑字节更新总账；增长前可淘汰其它终态事务，预算仍不足则保持原值。
fn replaceSummaryWithinMetadataBudget(
    state: &mut RecordingStateInner,
    transactionId: &str,
    summary: TransactionSummary,
    summaryBytes: usize,
) -> Result<Vec<BodyRef>, CaptureError> {
    if summaryBytes > maximumTransactionSummaryBytes {
        return Err(CaptureError::MetadataMemoryBudgetExceeded);
    }
    let currentRecord = state
        .transactions
        .get(transactionId)
        .ok_or(CaptureError::TransactionNotFound)?;
    let oldTransactionBytes = transactionMetadataBytes(currentRecord);
    let oldIndexBytes = responseEntityIndexMetadataBytes(
        &currentRecord.summary,
        &currentRecord.responseHeaders,
        currentRecord.responseBody.as_ref(),
    );
    let newIndexBytes = responseEntityIndexMetadataBytes(
        &summary,
        &currentRecord.responseHeaders,
        currentRecord.responseBody.as_ref(),
    );
    let oldBodyReferenceBytes = if currentRecord.summary.status == TransactionStatus::Pending {
        0
    } else {
        bodyReferenceMetadataBytes(currentRecord)
    };
    let newBodyReferenceBytes = if summary.status == TransactionStatus::Pending {
        0
    } else {
        bodyReferenceMetadataBytes(currentRecord)
    };
    let newTransactionBytes = oldTransactionBytes
        .saturating_sub(currentRecord.summaryStorageBytes)
        .saturating_sub(oldBodyReferenceBytes)
        .saturating_sub(oldIndexBytes)
        .saturating_add(summaryBytes)
        .saturating_add(newBodyReferenceBytes);
    let newTransactionBytes = newTransactionBytes.saturating_add(newIndexBytes);
    let requiredIncrease = newTransactionBytes.saturating_sub(oldTransactionBytes);
    let evictionIds = planMetadataBudgetEvictions(state, transactionId, requiredIncrease);
    let freedBytes = evictionIds
        .iter()
        .filter_map(|identifier| state.transactions.get(identifier))
        .map(transactionMetadataBytes)
        .sum::<usize>();
    let deferredCleanupBytes = evictionIds
        .iter()
        .filter_map(|identifier| state.transactions.get(identifier))
        .map(cleanupBodyReferencesMetadataBytes)
        .sum::<usize>();
    let projectedVisibleBytes = state
        .totalMetadataBytes
        .saturating_sub(oldTransactionBytes)
        .saturating_sub(freedBytes)
        .saturating_add(newTransactionBytes);
    if projectedVisibleBytes.saturating_add(deferredCleanupBytes) > state.metadataMemoryBudgetBytes
    {
        return Err(CaptureError::MetadataMemoryBudgetExceeded);
    }
    let removedBodies = removeTransactions(state, &evictionIds);
    let record = state
        .transactions
        .get_mut(transactionId)
        .ok_or(CaptureError::TransactionNotFound)?;
    record.summary = summary;
    record.summaryStorageBytes = summaryBytes;
    // 调用方随后把 removedBodies 计入 cleanupQueue；这里先写入不含 tombstone 的可见集合总额。
    state.totalMetadataBytes = projectedVisibleBytes;
    Ok(removedBodies)
}

/// 提交终态摘要后立即重跑数量与正文总限额；活动期无法删除的事务在结束点按 FIFO 收敛。
fn finalizeSummaryWithinLimits(
    state: &mut RecordingStateInner,
    transactionId: &str,
    summary: TransactionSummary,
    summaryBytes: usize,
) -> Result<Vec<BodyRef>, CaptureError> {
    let mut removedBodies =
        replaceSummaryWithinMetadataBudget(state, transactionId, summary, summaryBytes)?;
    removedBodies.extend(enforceAllLimits(state));
    insertResponseEntityEntry(state, transactionId);
    Ok(removedBodies)
}

/// 获取仍为 pending 的事务；终态事务返回 TransactionFinished。
fn pendingRecord<'a>(
    state: &'a mut RecordingStateInner,
    transactionId: &str,
) -> Result<&'a mut TransactionRecord, CaptureError> {
    let record = state
        .transactions
        .get_mut(transactionId)
        .ok_or(CaptureError::TransactionNotFound)?;
    if record.summary.status != TransactionStatus::Pending {
        return Err(CaptureError::TransactionFinished);
    }
    Ok(record)
}

/// 将调用方提供的 pending 协议字段原子应用到事务摘要；请求身份更新必须同时覆盖全部 URL 派生字段，避免树节点与详情展示不同目标。
fn applyTransactionUpdate(summary: &mut TransactionSummary, update: TransactionUpdate) {
    if let Some(method) = update.method {
        summary.method = method;
    }
    if let Some(location) = update.location {
        summary.host = location.host;
        summary.port = location.port;
        summary.path = location.path;
        summary.query = location.query;
        summary.urlDisplay = location.display;
    }
    if let Some(statusCode) = update.statusCode {
        summary.statusCode = Some(statusCode);
    }
    if let Some(contentType) = update.contentType {
        summary.contentType = contentType;
    }
    if let Some(flags) = update.flags {
        let bodyTruncated = summary.flags.bodyTruncated;
        let headersTruncated = summary.flags.headersTruncated;
        summary.flags = flags;
        summary.flags.bodyTruncated = bodyTruncated;
        summary.flags.headersTruncated = headersTruncated;
    }
}

/// 将用户标注字段独立应用到摘要；该路径不触碰协议状态、计时、大小或错误结果。
fn applyUserUpdate(summary: &mut TransactionSummary, update: TransactionUserUpdate) {
    if let Some(notes) = update.notes {
        summary.notes = notes;
    }
    if let Some(tags) = update.tags {
        summary.tags = tags;
    }
    if let Some(appliedTools) = update.appliedTools {
        summary.appliedTools = appliedTools;
    }
}

/// 合并单个阶段实际提供的进度字段；None 永远不清除其它并发阶段已写入的值。
fn applyProgressUpdate(summary: &mut TransactionSummary, update: TransactionProgressUpdate) {
    if let Some(value) = update.requestHeaderBytes {
        summary.sizes.requestHeaderBytes = value;
    }
    if let Some(value) = update.requestBodyBytes {
        summary.sizes.requestBodyBytes = value;
    }
    if let Some(value) = update.responseHeaderBytes {
        summary.sizes.responseHeaderBytes = value;
    }
    if let Some(value) = update.responseBodyBytes {
        summary.sizes.responseBodyBytes = value;
    }
    if let Some(value) = update.dnsEndAtMilliseconds {
        summary.timings.dnsEndAtMilliseconds = Some(value);
    }
    if let Some(value) = update.connectEndAtMilliseconds {
        summary.timings.connectEndAtMilliseconds = Some(value);
    }
    if let Some(value) = update.tlsEndAtMilliseconds {
        summary.timings.tlsEndAtMilliseconds = Some(value);
    }
    if let Some(value) = update.requestSentAtMilliseconds {
        summary.timings.requestSentAtMilliseconds = Some(value);
    }
    if let Some(value) = update.responseStartAtMilliseconds {
        summary.timings.responseStartAtMilliseconds = Some(value);
    }
}
