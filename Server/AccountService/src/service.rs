use std::{cmp::Ordering, collections::HashMap, sync::Arc};

use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    AccountExpirationFilter, AccountQuery, AccountServiceError, AccountSortField,
    AccountStatistics, AccountStatusFilter, AccountStore, AccountUsageView, AccountView,
    ActiveRuleSetMetadata, ApiKeyResponse, AuditLogView, BatchDeleteAccountsRequest,
    BatchDeleteAccountsResponse, BatchDeleteRuleSetsRequest, BatchDeleteRuleSetsResponse,
    BatchUpdateAccountsRequest, BatchUpdateAccountsResponse, ConnectionView, CreateAccountRequest,
    CreateRuleSetRequest, LeaseAuthenticationRequest, LeaseAuthenticationResponse,
    LeaseSynchronizationRequest, LeaseSynchronizationResponse, ManagementIdentityView,
    PasswordMode, Result, RuleSetView, SetPasswordRequest, SetRuleSetEnabledRequest, SortOrder,
    StoredAccount, UpdateAccountRequest, UpdateRuleSetRequest,
    credential::verifyPassword,
    lease::LeaseRegistry,
    store::{UsageIncrement, currentTimeMilliseconds},
};

struct AccountDomainState {
    store: AccountStore,
    leases: LeaseRegistry,
    serviceInstanceId: String,
    // 账号写事务、认证和租约创建共用该锁，保证数据库策略提交与内存租约判定没有旧策略窗口。
    accountOperationLock: Mutex<()>,
}

/// 聚合账号存储和易失租约；HTTP 与 SOCKS5 适配器只能通过该领域入口改变状态。
#[derive(Clone)]
pub struct AccountDomainService {
    state: Arc<AccountDomainState>,
}

impl AccountDomainService {
    /// 使用已迁移的独占存储创建新服务实例；实例 ID 每次进程启动变化。
    pub fn new(store: AccountStore) -> Self {
        Self {
            state: Arc::new(AccountDomainState {
                store,
                leases: LeaseRegistry::new(),
                serviceInstanceId: Uuid::new_v4().to_string(),
                accountOperationLock: Mutex::new(()),
            }),
        }
    }

    /// 返回当前进程实例标识，数据面用它拒绝跨重启的旧租约批次。
    pub fn serviceInstanceId(&self) -> &str {
        &self.state.serviceInstanceId
    }

    /// 初始化或重新派生默认管理身份，完整 Key 只返回给本次内部调用者。
    pub fn bootstrapManagement(&self, username: &str, password: &str) -> Result<ApiKeyResponse> {
        self.state.store.bootstrapManagement(username, password)
    }

    /// 判断管理身份是否已完成初始化，供进程启动时安全区分首次建库和普通重启。
    pub fn managementInitialized(&self) -> Result<bool> {
        self.state.store.managementInitialized()
    }

    /// 在已授权入口恢复当前完整 Key；不可逆密码摘要始终封装在存储层内。
    pub fn managementApiKey(&self) -> Result<ApiKeyResponse> {
        self.state.store.managementApiKey()
    }

    /// 在已授权入口更新管理身份并随新凭据派生 Key；账号业务租约不受管理会话撤销影响。
    pub fn updateManagementIdentity(
        &self,
        username: &str,
        password: &str,
    ) -> Result<ApiKeyResponse> {
        self.state
            .store
            .updateManagementIdentity(username, password)
    }

    /// 校验浏览器登录并返回会话修订号。
    pub fn authenticateManagement(&self, username: &str, password: &str) -> Result<i64> {
        self.state.store.authenticateManagement(username, password)
    }

    /// 返回 HTTP 层签名持久浏览器会话所需的派生材料和身份修订号；不会进入公共响应或日志。
    pub fn browserSessionMaterial(&self) -> Result<(String, i64)> {
        self.state.store.browserSessionMaterial()
    }

    /// 校验自动化 Bearer Key。
    pub fn authenticateApiKey(&self, apiKey: &str) -> Result<()> {
        self.state.store.authenticateApiKey(apiKey)
    }

    /// 返回脱敏管理身份。
    pub fn managementIdentity(&self) -> Result<ManagementIdentityView> {
        self.state.store.managementIdentity()
    }

    /// 使用与 SOCKS5 相同的账号策略和密码语义做无租约校验；规则下载与打包授权不会占用连接额度。
    pub async fn verifyAccountCredentials(&self, username: &str, password: &str) -> Result<()> {
        let _operation = self.state.accountOperationLock.lock().await;
        let account = self
            .state
            .store
            .accountByUsername(username)?
            .ok_or(AccountServiceError::SocksAuthenticationFailed)?;
        validateAccountAuthentication(&account, password, currentTimeMilliseconds())
    }

    /// 返回管理端规则集完整快照；启用互斥由存储层唯一索引和事务共同保证。
    pub fn listRuleSets(&self) -> Result<Vec<RuleSetView>> {
        self.state.store.listRuleSets()
    }

    /// 创建规则集；正文语法校验和可选启用切换均在存储事务中完成。
    pub fn createRuleSet(&self, request: &CreateRuleSetRequest) -> Result<RuleSetView> {
        self.state.store.createRuleSet(request)
    }

    /// 返回单个规则集，供编辑页面按最新修订刷新。
    pub fn ruleSet(&self, ruleSetId: &str) -> Result<RuleSetView> {
        self.state.store.ruleSetById(ruleSetId)
    }

    /// 保存规则集名称和正文；并发冲突返回当前修订号，不覆盖其他管理端输入。
    pub fn updateRuleSet(
        &self,
        ruleSetId: &str,
        request: &UpdateRuleSetRequest,
    ) -> Result<RuleSetView> {
        self.state.store.updateRuleSet(ruleSetId, request)
    }

    /// 切换规则集启用状态；开启目标时同一事务关闭当前启用项并推进双方修订。
    pub fn setRuleSetEnabled(
        &self,
        ruleSetId: &str,
        request: &SetRuleSetEnabledRequest,
    ) -> Result<RuleSetView> {
        self.state.store.setRuleSetEnabled(ruleSetId, request)
    }

    /// 删除单个规则集；删除当前启用项后客户端下载返回明确未配置状态。
    pub fn deleteRuleSet(&self, ruleSetId: &str) -> Result<()> {
        self.state.store.deleteRuleSet(ruleSetId)
    }

    /// 原子删除管理端多选规则集；任一 ID 无效时整批保持原状。
    pub fn deleteRuleSetsBatch(
        &self,
        request: &BatchDeleteRuleSetsRequest,
    ) -> Result<BatchDeleteRuleSetsResponse> {
        Ok(BatchDeleteRuleSetsResponse {
            deletedRuleSets: self.state.store.deleteRuleSetsBatch(request)?,
        })
    }

    /// 返回唯一启用规则集正文和修订，供认证后的客户端下载与条件缓存。
    pub fn activeRuleSet(&self) -> Result<RuleSetView> {
        self.state.store.activeRuleSet()
    }

    /// 投影打包器需要的当前启用规则元数据；不存在启用项时保留规则集 404 失败语义。
    pub fn activeRuleSetMetadata(&self) -> Result<ActiveRuleSetMetadata> {
        let active = self.activeRuleSet()?;
        Ok(ActiveRuleSetMetadata {
            id: active.ruleSetId,
            revision: active.revision,
        })
    }

    /// 创建账号并组合零在线态；创建事务与同时到达的认证请求互斥。
    pub async fn createAccount(&self, request: &CreateAccountRequest) -> Result<AccountView> {
        let _operation = self.state.accountOperationLock.lock().await;
        let account = self.state.store.createAccount(request)?;
        self.accountView(account)
    }

    /// 在同一完整账号快照上依次执行搜索、状态/到期筛选、白名单排序和分页。
    ///
    /// 运行上下文：外部 API 的查询语义不能依赖动态 SQL，流量与在线值还来自不同运行层，故在领域层
    /// 合并后投影。参数非法时不访问数据库；数据库或在线态读取失败时不返回不完整页面。
    pub fn queryAccounts(&self, query: &AccountQuery) -> Result<Vec<AccountView>> {
        if query.offset < 0 || !(1..=200).contains(&query.limit) {
            return Err(AccountServiceError::Validation(
                "offset 必须非负且 limit 必须位于 1..=200".to_owned(),
            ));
        }
        let usage = self.state.store.usageTotals()?;
        let now = currentTimeMilliseconds();
        let normalizedSearch = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase);
        let mut accounts = self
            .state
            .store
            .listAllAccounts()?
            .into_iter()
            .map(|account| accountViewWithUsage(&self.state.leases, account, &usage))
            .collect::<Result<Vec<_>>>()?;
        accounts.retain(|account| {
            matchesSearch(account, normalizedSearch.as_deref())
                && matchesStatus(account, query.status, now)
                && matchesExpiration(account, query.expiration, now)
        });
        accounts.sort_by(|left, right| compareAccounts(left, right, query.sort, query.order));
        Ok(accounts
            .into_iter()
            .skip(query.offset as usize)
            .take(query.limit as usize)
            .collect())
    }

    /// 返回单账号详情；在线态从当前租约注册表实时读取。
    pub fn account(&self, accountId: &str) -> Result<AccountView> {
        self.accountView(self.state.store.accountById(accountId)?)
    }

    /// 更新策略后在释放操作锁前同步现有租约，禁止旧策略认证穿过提交窗口。
    pub async fn updateAccount(
        &self,
        accountId: &str,
        request: &UpdateAccountRequest,
    ) -> Result<AccountView> {
        let _operation = self.state.accountOperationLock.lock().await;
        let account = self.state.store.updateAccount(accountId, request)?;
        self.state
            .leases
            .reconcileAccount(&account, currentTimeMilliseconds());
        self.accountView(account)
    }

    /// 批量更新账号策略，并在数据库事务提交后用同代策略刷新所有活动租约。
    ///
    /// 运行上下文：单一操作锁把 SQLite 提交和租约发布串成原子可见边界；存储失败时不修改租约。
    pub async fn updateAccountsBatch(
        &self,
        request: &BatchUpdateAccountsRequest,
    ) -> Result<BatchUpdateAccountsResponse> {
        let _operation = self.state.accountOperationLock.lock().await;
        let (accounts, updatedAccounts) = self.state.store.updateAccountsBatch(request)?;
        let now = currentTimeMilliseconds();
        let mut views = Vec::with_capacity(accounts.len());
        for account in accounts {
            self.state.leases.reconcileAccount(&account, now);
            views.push(self.accountView(account)?);
        }
        Ok(BatchUpdateAccountsResponse {
            accounts: views,
            updatedAccounts,
        })
    }

    /// 批量删除账号，并在数据库整批提交后移除全部对应租约；任何校验失败都保持原状态。
    pub async fn deleteAccountsBatch(
        &self,
        request: &BatchDeleteAccountsRequest,
    ) -> Result<BatchDeleteAccountsResponse> {
        let _operation = self.state.accountOperationLock.lock().await;
        let deletedAccounts = self.state.store.deleteAccountsBatch(request)?;
        for selection in &request.accounts {
            self.state.leases.removeAccount(&selection.accountId);
        }
        Ok(BatchDeleteAccountsResponse { deletedAccounts })
    }

    /// 设置固定密码并撤销现有租约，避免旧凭据连接无限延续。
    pub async fn setAccountPassword(
        &self,
        accountId: &str,
        request: &SetPasswordRequest,
    ) -> Result<()> {
        let _operation = self.state.accountOperationLock.lock().await;
        self.state
            .store
            .setAccountPassword(accountId, &request.password)?;
        self.state.leases.revokeAccount(accountId);
        Ok(())
    }

    /// 切换为任意非空密码并撤销现有租约，使新密码语义只影响后续认证。
    pub async fn clearAccountPassword(&self, accountId: &str) -> Result<()> {
        let _operation = self.state.accountOperationLock.lock().await;
        self.state.store.clearAccountPassword(accountId)?;
        self.state.leases.revokeAccount(accountId);
        Ok(())
    }

    /// 删除账号后撤销全部租约；数据面在下一同步关闭对应连接。
    pub async fn deleteAccount(&self, accountId: &str) -> Result<()> {
        let _operation = self.state.accountOperationLock.lock().await;
        // 数据库删除成功后再清除租约；操作锁隔离同步，既避免外键失败，也避免删除事务失败却误断连接。
        self.state.store.deleteAccount(accountId)?;
        self.state.leases.removeAccount(accountId);
        Ok(())
    }

    /// 认证账号并原子创建租约；任何拒绝原因都折叠为统一 SOCKS5 认证失败。
    pub async fn authenticateLease(
        &self,
        request: &LeaseAuthenticationRequest,
    ) -> Result<LeaseAuthenticationResponse> {
        let _operation = self.state.accountOperationLock.lock().await;
        let account = self
            .state
            .store
            .accountByUsername(&request.username)?
            .ok_or(AccountServiceError::SocksAuthenticationFailed)?;
        if let Err(error) =
            validateAccountAuthentication(&account, &request.password, currentTimeMilliseconds())
        {
            // 已识别账号的失败认证计入该账号统计；未知用户名无法安全归属，仍返回相同协议错误。
            self.state.store.applyUsage(&HashMap::from([(
                account.accountId.clone(),
                UsageIncrement {
                    rejectedAuthentications: 1,
                    ..UsageIncrement::default()
                },
            )]))?;
            return Err(error);
        }
        let leaseId = self.state.leases.createLease(&account, request)?;
        self.state.store.applyUsage(&HashMap::from([(
            account.accountId.clone(),
            UsageIncrement {
                acceptedConnections: 1,
                ..UsageIncrement::default()
            },
        )]))?;
        Ok(LeaseAuthenticationResponse {
            serviceInstanceId: self.state.serviceInstanceId.clone(),
            accountId: account.accountId,
            leaseId,
            username: account.username,
            policyRevision: account.policyRevision,
            maxUploadBytesPerSecond: account.policy.maxUploadBytesPerSecond,
            maxDownloadBytesPerSecond: account.policy.maxDownloadBytesPerSecond,
        })
    }

    /// 幂等同步租约并把首次确认的流量差值批量提交 SQLite。
    pub async fn synchronizeLeases(
        &self,
        request: &LeaseSynchronizationRequest,
    ) -> Result<LeaseSynchronizationResponse> {
        let _operation = self.state.accountOperationLock.lock().await;
        let (response, usage) = self
            .state
            .leases
            .synchronize(&self.state.serviceInstanceId, request)?;
        self.state.store.applyUsage(&usage)?;
        // 只有 SQLite 事务提交成功后才清除批次待写增量；写库失败时同一批次重试会再次提交而不是静默丢失统计。
        self.state.leases.confirmUsageCommitted(&request.batchId)?;
        Ok(response)
    }

    /// 强制下线不修改账号策略，后续新认证仍可成功。
    pub fn disconnectAccount(&self, accountId: &str) -> Result<usize> {
        self.state.store.accountById(accountId)?;
        Ok(self.state.leases.revokeAccount(accountId))
    }

    /// 返回全部或单账号活动连接快照。
    pub fn connections(&self, accountId: Option<&str>) -> Vec<ConnectionView> {
        self.state.leases.connections(accountId)
    }

    /// 组合数据库账号总数和易失在线态，形成管理首页统计；累计值仅为兼容既有 API 调用方保留。
    pub fn statistics(&self) -> Result<AccountStatistics> {
        let (totalAccounts, uploadedBytes, downloadedBytes) =
            self.state.store.aggregateStatistics()?;
        let (
            onlineAccounts,
            onlineIps,
            activeConnections,
            uploadBytesPerSecond,
            downloadBytesPerSecond,
        ) = self.state.leases.aggregateOverview();
        Ok(AccountStatistics {
            totalAccounts,
            onlineAccounts,
            onlineIps,
            activeConnections,
            uploadedBytes,
            downloadedBytes,
            uploadBytesPerSecond,
            downloadBytesPerSecond,
        })
    }

    /// 健康检查读取迁移版本，数据库锁定或损坏时不伪造就绪。
    pub fn schemaVersion(&self) -> Result<i64> {
        self.state.store.currentSchemaVersion()
    }

    /// 返回远程管理页面使用的脱敏审计记录，调用方负责验证管理身份。
    pub fn listAuditLogs(&self, offset: i64, limit: i64) -> Result<Vec<AuditLogView>> {
        self.state.store.listAuditLogs(offset, limit)
    }

    /// 返回指定账号累计与每日用量，活动连接流量以最近一次同步确认值为准。
    pub fn accountUsage(&self, accountId: &str) -> Result<AccountUsageView> {
        self.state.store.accountUsage(accountId)
    }

    /// 组合单账号持久化累计值和在线租约。
    fn accountView(&self, account: StoredAccount) -> Result<AccountView> {
        let usage = self.state.store.usageTotals()?;
        accountViewWithUsage(&self.state.leases, account, &usage)
    }
}

/// 搜索规范用户名和备注；Unicode 小写转换只用于查询，不改变账号精确认证语义。
fn matchesSearch(account: &AccountView, search: Option<&str>) -> bool {
    search.is_none_or(|search| {
        account.username.to_lowercase().contains(search)
            || account
                .remark
                .as_deref()
                .is_some_and(|remark| remark.to_lowercase().contains(search))
    })
}

/// 按服务端当前时间计算账号状态；零值禁用优先于到期，和认证拒绝顺序保持一致。
fn matchesStatus(account: &AccountView, filter: Option<AccountStatusFilter>, now: i64) -> bool {
    let status = if account.policy.disabled() {
        AccountStatusFilter::Disabled
    } else if account.policy.expired(now) {
        AccountStatusFilter::Expired
    } else {
        AccountStatusFilter::Available
    };
    filter.is_none_or(|filter| filter == status)
}

/// 到期维度只描述永不过期、未来计划和已经到期；expiresAt=0 由状态筛选表达。
fn matchesExpiration(
    account: &AccountView,
    filter: Option<AccountExpirationFilter>,
    now: i64,
) -> bool {
    filter.is_none_or(|filter| match filter {
        AccountExpirationFilter::Never => account.policy.expiresAt == -1,
        AccountExpirationFilter::Scheduled => account.policy.expiresAt > now,
        AccountExpirationFilter::Expired => {
            account.policy.expiresAt > 0 && account.policy.expiresAt < now
        }
    })
}

/// 比较白名单字段并以 accountId 固定同值顺序；排序方向只反转主字段，不破坏稳定分页。
fn compareAccounts(
    left: &AccountView,
    right: &AccountView,
    field: AccountSortField,
    order: SortOrder,
) -> Ordering {
    let primary = match field {
        AccountSortField::CreatedAt => left.createdAt.cmp(&right.createdAt),
        AccountSortField::Username => left.username.cmp(&right.username),
        AccountSortField::ExpiresAt => sortableExpiration(left).cmp(&sortableExpiration(right)),
        AccountSortField::UploadedBytes => left.uploadedBytes.cmp(&right.uploadedBytes),
        AccountSortField::DownloadedBytes => left.downloadedBytes.cmp(&right.downloadedBytes),
        AccountSortField::TotalBytes => accountTotalBytes(left).cmp(&accountTotalBytes(right)),
        AccountSortField::ActiveConnections => left.activeConnections.cmp(&right.activeConnections),
        AccountSortField::OnlineIps => left.onlineIps.cmp(&right.onlineIps),
    };
    let directed = match order {
        SortOrder::Asc => primary,
        SortOrder::Desc => primary.reverse(),
    };
    directed.then_with(|| left.accountId.cmp(&right.accountId))
}

/// 负一和零没有实际到期时刻，排序时统一位于明确时间之后。
fn sortableExpiration(account: &AccountView) -> i64 {
    if account.policy.expiresAt > 0 {
        account.policy.expiresAt
    } else {
        i64::MAX
    }
}

/// 使用 i128 汇总累计流量，避免两个合法 i64 计数相加时回绕并破坏排序。
fn accountTotalBytes(account: &AccountView) -> i128 {
    i128::from(account.uploadedBytes) + i128::from(account.downloadedBytes)
}

/// 固定密码按 Argon2id 校验；任意密码模式仍要求 RFC 1929 密码字段非空。
fn validateAccountAuthentication(
    account: &StoredAccount,
    password: &str,
    serverTime: i64,
) -> Result<()> {
    if password.is_empty() || account.policy.disabled() || account.policy.expired(serverTime) {
        return Err(AccountServiceError::SocksAuthenticationFailed);
    }
    match (&account.passwordHash, &account.passwordSalt) {
        (None, None) => Ok(()),
        (Some(hash), Some(salt)) if verifyPassword(password, hash, salt) => Ok(()),
        _ => Err(AccountServiceError::SocksAuthenticationFailed),
    }
}

/// 把内部账号、累计流量和实时在线态组合成公共视图。
fn accountViewWithUsage(
    leases: &LeaseRegistry,
    account: StoredAccount,
    usage: &HashMap<String, (i64, i64)>,
) -> Result<AccountView> {
    let (activeConnections, onlineIps) = leases.accountPresence(&account.accountId);
    let (uploadedBytes, downloadedBytes) =
        usage.get(&account.accountId).copied().unwrap_or_default();
    let passwordMode = if account.passwordHash.is_some() {
        PasswordMode::Fixed
    } else {
        PasswordMode::Any
    };
    Ok(AccountView {
        accountId: account.accountId,
        username: account.username,
        passwordMode,
        policy: account.policy,
        policyRevision: account.policyRevision,
        remark: account.remark,
        createdAt: account.createdAt,
        updatedAt: account.updatedAt,
        uploadedBytes,
        downloadedBytes,
        activeConnections,
        onlineIps,
    })
}
