use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::IpAddr,
    str::FromStr,
};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    AccountPolicy, AccountServiceError, ConnectionView, LeaseAuthenticationRequest,
    LeaseSynchronizationRequest, LeaseSynchronizationResponse, LeaseSynchronizationResult, Result,
    StoredAccount,
    store::{UsageIncrement, currentTimeMilliseconds},
};

const leaseHeartbeatTimeoutMilliseconds: i64 = 8_000;
const maximumRememberedBatches: usize = 4_096;

/// 活动租约只属于当前账号服务实例，进程重启后由 SOCKS5 数据面重新认证。
#[derive(Clone)]
struct LeaseState {
    leaseId: String,
    accountId: String,
    connectionId: String,
    sourceIp: String,
    createdAt: i64,
    lastHeartbeatAt: i64,
    policy: AccountPolicy,
    policyRevision: i64,
    uploadedBytes: u64,
    downloadedBytes: u64,
    uploadBytesPerSecond: u64,
    downloadBytesPerSecond: u64,
    rateObservedAt: i64,
    persistedUploadedBytes: u64,
    persistedDownloadedBytes: u64,
    finalPending: bool,
    revoked: bool,
}

/// 缓存已经确认的同步批次；相同批次原样重试返回原响应，不重复累计流量。
struct BatchRecord {
    requestDigest: [u8; 32],
    response: LeaseSynchronizationResponse,
}

struct LeaseRegistryState {
    leases: HashMap<String, LeaseState>,
    batches: HashMap<String, BatchRecord>,
    batchOrder: VecDeque<String>,
}

/// 维护活动连接、在线 IP 和幂等批次；所有复合限制在同一互斥区内判定。
pub struct LeaseRegistry {
    state: Mutex<LeaseRegistryState>,
}

impl LeaseRegistry {
    /// 创建空注册表；持久化层不恢复旧连接，避免进程重启后出现虚假在线状态。
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LeaseRegistryState {
                leases: HashMap::new(),
                batches: HashMap::new(),
                batchOrder: VecDeque::new(),
            }),
        }
    }

    /// 清理超时租约后原子检查连接/IP 上限并创建租约；达到任一边界统一拒绝认证。
    pub fn createLease(
        &self,
        account: &StoredAccount,
        request: &LeaseAuthenticationRequest,
    ) -> Result<String> {
        let sourceIp = normalizeSourceIp(&request.sourceIp)?;
        let now = currentTimeMilliseconds();
        let mut state = self.state.lock();
        purgeExpiredLeases(&mut state, now);
        let accountLeases: Vec<&LeaseState> = state
            .leases
            .values()
            .filter(|lease| lease.accountId == account.accountId && !lease.revoked)
            .collect();
        if account.policy.maxConnections > 0
            && accountLeases.len() >= account.policy.maxConnections as usize
        {
            return Err(AccountServiceError::SocksAuthenticationFailed);
        }
        let onlineIps: HashSet<&str> = accountLeases
            .iter()
            .map(|lease| lease.sourceIp.as_str())
            .collect();
        let sourceAlreadyOnline = onlineIps.contains(sourceIp.as_str());
        if !sourceAlreadyOnline
            && account.policy.maxOnlineIps > 0
            && onlineIps.len() >= account.policy.maxOnlineIps as usize
        {
            return Err(AccountServiceError::SocksAuthenticationFailed);
        }
        let leaseId = Uuid::new_v4().to_string();
        state.leases.insert(
            leaseId.clone(),
            LeaseState {
                leaseId: leaseId.clone(),
                accountId: account.accountId.clone(),
                connectionId: request.connectionId.clone(),
                sourceIp,
                createdAt: now,
                lastHeartbeatAt: now,
                policy: account.policy.clone(),
                policyRevision: account.policyRevision,
                uploadedBytes: 0,
                downloadedBytes: 0,
                uploadBytesPerSecond: 0,
                downloadBytesPerSecond: 0,
                rateObservedAt: now,
                persistedUploadedBytes: 0,
                persistedDownloadedBytes: 0,
                finalPending: false,
                revoked: false,
            },
        );
        Ok(leaseId)
    }

    /// 批量确认单调累计值并返回每账号增量；final 租约在生成确认响应后立即回收。
    pub fn synchronize(
        &self,
        serviceInstanceId: &str,
        request: &LeaseSynchronizationRequest,
    ) -> Result<(
        LeaseSynchronizationResponse,
        HashMap<String, UsageIncrement>,
    )> {
        if request.serviceInstanceId != serviceInstanceId {
            return Err(AccountServiceError::StateConflict(
                "账号服务实例标识已变化".to_owned(),
            ));
        }
        let requestBytes = serde_json::to_vec(request)?;
        let requestDigest: [u8; 32] = Sha256::digest(requestBytes).into();
        let now = currentTimeMilliseconds();
        let mut state = self.state.lock();
        if let Some(record) = state.batches.get(&request.batchId) {
            if record.requestDigest != requestDigest {
                return Err(AccountServiceError::StateConflict(
                    "同一批次标识提交了不同租约内容".to_owned(),
                ));
            }
            return Ok((record.response.clone(), pendingUsage(&state)));
        }
        purgeExpiredLeases(&mut state, now);
        let mut results = Vec::with_capacity(request.leases.len());
        for progress in &request.leases {
            let Some(lease) = state.leases.get_mut(&progress.leaseId) else {
                results.push(missingLeaseResult(&progress.leaseId));
                continue;
            };
            if lease.connectionId != progress.connectionId {
                results.push(missingLeaseResult(&progress.leaseId));
                continue;
            }
            if progress.uploadedBytes < lease.uploadedBytes
                || progress.downloadedBytes < lease.downloadedBytes
            {
                lease.revoked = true;
                results.push(LeaseSynchronizationResult {
                    leaseId: lease.leaseId.clone(),
                    acknowledgedUploadedBytes: lease.uploadedBytes,
                    acknowledgedDownloadedBytes: lease.downloadedBytes,
                    policyRevision: lease.policyRevision,
                    maxUploadBytesPerSecond: lease.policy.maxUploadBytesPerSecond,
                    maxDownloadBytesPerSecond: lease.policy.maxDownloadBytesPerSecond,
                    revoked: true,
                    errorCode: Some("nonMonotonicUsage".to_owned()),
                });
                continue;
            }
            // 速率必须使用相邻两次单调累计值计算，不能拿进程生命周期累计量冒充实时带宽。
            let elapsedMilliseconds = now.saturating_sub(lease.lastHeartbeatAt);
            lease.uploadBytesPerSecond = bytesPerSecond(
                progress.uploadedBytes.saturating_sub(lease.uploadedBytes),
                elapsedMilliseconds,
            );
            lease.downloadBytesPerSecond = bytesPerSecond(
                progress
                    .downloadedBytes
                    .saturating_sub(lease.downloadedBytes),
                elapsedMilliseconds,
            );
            lease.rateObservedAt = now;
            lease.uploadedBytes = progress.uploadedBytes;
            lease.downloadedBytes = progress.downloadedBytes;
            lease.lastHeartbeatAt = now;
            // 到期由服务端当前时间判定；持续心跳只能维持租约活性，不能延长账号有效期。
            lease.revoked |= lease.policy.disabled() || lease.policy.expired(now);
            results.push(LeaseSynchronizationResult {
                leaseId: lease.leaseId.clone(),
                acknowledgedUploadedBytes: lease.uploadedBytes,
                acknowledgedDownloadedBytes: lease.downloadedBytes,
                policyRevision: lease.policyRevision,
                maxUploadBytesPerSecond: lease.policy.maxUploadBytesPerSecond,
                maxDownloadBytesPerSecond: lease.policy.maxDownloadBytesPerSecond,
                revoked: lease.revoked,
                errorCode: None,
            });
            if progress.final_ {
                // final 只标记待回收；SQLite 成功提交全部待写增量后才能删除租约。
                lease.finalPending = true;
            }
        }
        let response = LeaseSynchronizationResponse {
            serviceInstanceId: serviceInstanceId.to_owned(),
            batchId: request.batchId.clone(),
            leases: results,
        };
        rememberBatch(
            &mut state,
            request.batchId.clone(),
            requestDigest,
            response.clone(),
        );
        Ok((response, pendingUsage(&state)))
    }

    /// 确认当前所有待写增量已持久化，并在同一锁内回收 final 租约。
    ///
    /// 任意新批次都会携带全局未提交差值，因此数据库失败后客户端即使更换 batchId 也不会丢账。
    pub fn confirmUsageCommitted(&self, batchId: &str) -> Result<()> {
        let mut state = self.state.lock();
        state.batches.get(batchId).ok_or_else(|| {
            AccountServiceError::StateConflict("租约同步批次已经离开幂等窗口".to_owned())
        })?;
        for lease in state.leases.values_mut() {
            lease.persistedUploadedBytes = lease.uploadedBytes;
            lease.persistedDownloadedBytes = lease.downloadedBytes;
        }
        state.leases.retain(|_, lease| !lease.finalPending);
        Ok(())
    }

    /// 用最新策略替换活动租约；账号被禁用、过期或缩限低于现状时撤销该账号全部租约。
    pub fn reconcileAccount(&self, account: &StoredAccount, serverTime: i64) {
        let mut state = self.state.lock();
        let activeLeases: Vec<&LeaseState> = state
            .leases
            .values()
            .filter(|lease| lease.accountId == account.accountId && !lease.revoked)
            .collect();
        let onlineIps: HashSet<&str> = activeLeases
            .iter()
            .map(|lease| lease.sourceIp.as_str())
            .collect();
        let exceedsConnections = account.policy.maxConnections > 0
            && activeLeases.len() > account.policy.maxConnections as usize;
        let exceedsIps = account.policy.maxOnlineIps > 0
            && onlineIps.len() > account.policy.maxOnlineIps as usize;
        let revokeAll = account.policy.disabled()
            || account.policy.expired(serverTime)
            || exceedsConnections
            || exceedsIps;
        for lease in state
            .leases
            .values_mut()
            .filter(|lease| lease.accountId == account.accountId)
        {
            lease.policy = account.policy.clone();
            lease.policyRevision = account.policyRevision;
            lease.revoked |= revokeAll;
        }
    }

    /// 删除账号或管理员强制下线时标记全部租约撤销，等待下一同步让数据面主动关闭。
    pub fn revokeAccount(&self, accountId: &str) -> usize {
        let mut state = self.state.lock();
        let mut revoked = 0_usize;
        for lease in state
            .leases
            .values_mut()
            .filter(|lease| lease.accountId == accountId && !lease.revoked)
        {
            lease.revoked = true;
            revoked += 1;
        }
        revoked
    }

    /// 删除账号前立即清除其租约和待持久化差值，避免后续全局同步写入已不存在的外键。
    ///
    /// 删除语义明确允许丢弃尚未提交的尾流量；旧客户端下一同步会得到 missing/revoked。
    pub fn removeAccount(&self, accountId: &str) -> usize {
        let mut state = self.state.lock();
        let before = state.leases.len();
        state.leases.retain(|_, lease| lease.accountId != accountId);
        before.saturating_sub(state.leases.len())
    }

    /// 返回全部或指定账号的活动租约快照；读取不暴露内部批次和策略对象。
    pub fn connections(&self, accountId: Option<&str>) -> Vec<ConnectionView> {
        let now = currentTimeMilliseconds();
        let mut state = self.state.lock();
        purgeExpiredLeases(&mut state, now);
        let mut connections: Vec<ConnectionView> = state
            .leases
            .values()
            .filter(|lease| accountId.is_none_or(|id| lease.accountId == id))
            .map(|lease| connectionView(lease, now))
            .collect();
        connections.sort_by_key(|connection| std::cmp::Reverse(connection.createdAt));
        connections
    }

    /// 返回账号活动连接数和去重 IP 数，供账号列表组合公开状态。
    pub fn accountPresence(&self, accountId: &str) -> (usize, usize) {
        let now = currentTimeMilliseconds();
        let mut state = self.state.lock();
        purgeExpiredLeases(&mut state, now);
        let accountLeases: Vec<&LeaseState> = state
            .leases
            .values()
            .filter(|lease| lease.accountId == accountId && !lease.revoked)
            .collect();
        let onlineIps: HashSet<&str> = accountLeases
            .iter()
            .map(|lease| lease.sourceIp.as_str())
            .collect();
        (accountLeases.len(), onlineIps.len())
    }

    /// 返回全局在线态和实时带宽；读取前清除超时租约，避免停止心跳后仍显示在线或旧速率。
    pub fn aggregateOverview(&self) -> (usize, usize, usize, u64, u64) {
        let now = currentTimeMilliseconds();
        let mut state = self.state.lock();
        purgeExpiredLeases(&mut state, now);
        let activeLeases: Vec<&LeaseState> = state
            .leases
            .values()
            .filter(|lease| !lease.revoked)
            .collect();
        let accounts: HashSet<&str> = activeLeases
            .iter()
            .map(|lease| lease.accountId.as_str())
            .collect();
        let addresses: HashSet<(&str, &str)> = activeLeases
            .iter()
            .map(|lease| (lease.accountId.as_str(), lease.sourceIp.as_str()))
            .collect();
        let uploadBytesPerSecond = activeLeases.iter().fold(0_u64, |total, lease| {
            total.saturating_add(freshRate(
                lease.uploadBytesPerSecond,
                lease.rateObservedAt,
                now,
            ))
        });
        let downloadBytesPerSecond = activeLeases.iter().fold(0_u64, |total, lease| {
            total.saturating_add(freshRate(
                lease.downloadBytesPerSecond,
                lease.rateObservedAt,
                now,
            ))
        });
        (
            accounts.len(),
            addresses.len(),
            activeLeases.len(),
            uploadBytesPerSecond,
            downloadBytesPerSecond,
        )
    }
}

impl Default for LeaseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 规范化来源地址；IPv4-mapped IPv6 与对应 IPv4 必须计为同一个在线 IP。
fn normalizeSourceIp(sourceIp: &str) -> Result<String> {
    let address = IpAddr::from_str(sourceIp)
        .map_err(|_| AccountServiceError::Validation("sourceIp 不是有效 IP 地址".to_owned()))?;
    Ok(match address {
        IpAddr::V6(ipv6) => ipv6
            .to_ipv4_mapped()
            .map_or_else(|| ipv6.to_string(), |ipv4| ipv4.to_string()),
        IpAddr::V4(ipv4) => ipv4.to_string(),
    })
}

/// 把同步区间字节增量换算为每秒速率；同毫秒同步没有有效时间窗口，返回零而不是制造峰值。
fn bytesPerSecond(bytes: u64, elapsedMilliseconds: i64) -> u64 {
    if elapsedMilliseconds <= 0 {
        return 0;
    }
    bytes.saturating_mul(1_000) / elapsedMilliseconds as u64
}

/// 实时速率只在一个心跳窗口内有效；空闲连接仍保持在线，但不得永久显示最后一次传输峰值。
fn freshRate(rate: u64, observedAt: i64, now: i64) -> u64 {
    if now.saturating_sub(observedAt) > leaseHeartbeatTimeoutMilliseconds {
        return 0;
    }
    rate
}

/// 超过心跳窗口的租约立即回收，后续旧同步会收到 revoked/missing 结果。
fn purgeExpiredLeases(state: &mut LeaseRegistryState, now: i64) {
    state.leases.retain(|_, lease| {
        let timedOut =
            now.saturating_sub(lease.lastHeartbeatAt) > leaseHeartbeatTimeoutMilliseconds;
        if timedOut {
            lease.revoked = true;
            lease.finalPending = true;
        }
        // 已持久化的超时租约可立即回收；含待写流量的租约保留到后续任意同步成功冲账。
        !timedOut
            || lease.uploadedBytes != lease.persistedUploadedBytes
            || lease.downloadedBytes != lease.persistedDownloadedBytes
    });
}

/// 缺失租约使用零累计确认并明确撤销，调用方必须关闭对应数据面连接。
fn missingLeaseResult(leaseId: &str) -> LeaseSynchronizationResult {
    LeaseSynchronizationResult {
        leaseId: leaseId.to_owned(),
        acknowledgedUploadedBytes: 0,
        acknowledgedDownloadedBytes: 0,
        policyRevision: 0,
        maxUploadBytesPerSecond: 0,
        maxDownloadBytesPerSecond: 0,
        revoked: true,
        errorCode: Some("leaseNotFound".to_owned()),
    }
}

/// 保存有界批次历史；达到上限时按插入顺序淘汰最旧记录。
fn rememberBatch(
    state: &mut LeaseRegistryState,
    batchId: String,
    requestDigest: [u8; 32],
    response: LeaseSynchronizationResponse,
) {
    if state.batches.len() >= maximumRememberedBatches
        && let Some(expiredBatchId) = state.batchOrder.pop_front()
    {
        state.batches.remove(&expiredBatchId);
    }
    state.batchOrder.push_back(batchId.clone());
    state.batches.insert(
        batchId,
        BatchRecord {
            requestDigest,
            response,
        },
    );
}

/// 聚合所有租约尚未写入 SQLite 的单调差值；跨批次失败恢复以持久化水位而非批次身份为准。
fn pendingUsage(state: &LeaseRegistryState) -> HashMap<String, UsageIncrement> {
    let mut usageByAccount: HashMap<String, UsageIncrement> = HashMap::new();
    for lease in state.leases.values() {
        let uploadedDelta = lease
            .uploadedBytes
            .saturating_sub(lease.persistedUploadedBytes);
        let downloadedDelta = lease
            .downloadedBytes
            .saturating_sub(lease.persistedDownloadedBytes);
        if uploadedDelta == 0 && downloadedDelta == 0 {
            continue;
        }
        let usage = usageByAccount.entry(lease.accountId.clone()).or_default();
        usage.uploadedBytes = usage
            .uploadedBytes
            .saturating_add(uploadedDelta.min(i64::MAX as u64) as i64);
        usage.downloadedBytes = usage
            .downloadedBytes
            .saturating_add(downloadedDelta.min(i64::MAX as u64) as i64);
    }
    usageByAccount
}

/// 把内部租约转换为公共连接视图，不复制账号策略和内部批次状态。
fn connectionView(lease: &LeaseState, now: i64) -> ConnectionView {
    ConnectionView {
        leaseId: lease.leaseId.clone(),
        accountId: lease.accountId.clone(),
        connectionId: lease.connectionId.clone(),
        sourceIp: lease.sourceIp.clone(),
        createdAt: lease.createdAt,
        lastHeartbeatAt: lease.lastHeartbeatAt,
        uploadedBytes: lease.uploadedBytes,
        downloadedBytes: lease.downloadedBytes,
        uploadBytesPerSecond: freshRate(lease.uploadBytesPerSecond, lease.rateObservedAt, now),
        downloadBytesPerSecond: freshRate(lease.downloadBytesPerSecond, lease.rateObservedAt, now),
        revoked: lease.revoked,
    }
}
