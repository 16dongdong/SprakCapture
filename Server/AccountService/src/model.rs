use serde::{Deserialize, Serialize};

/// 固定账号策略；所有限制统一使用 -1、0、正数三态，避免不同接口出现不同禁用语义。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountPolicy {
    pub maxUploadBytesPerSecond: i64,
    pub maxDownloadBytesPerSecond: i64,
    pub maxConnections: i64,
    pub maxOnlineIps: i64,
    pub expiresAt: i64,
}

impl AccountPolicy {
    /// 校验全部三态字段；失败返回具体字段，数据库约束作为最后一道一致性边界。
    pub fn validate(&self) -> crate::Result<()> {
        for (name, value) in [
            ("maxUploadBytesPerSecond", self.maxUploadBytesPerSecond),
            ("maxDownloadBytesPerSecond", self.maxDownloadBytesPerSecond),
            ("maxConnections", self.maxConnections),
            ("maxOnlineIps", self.maxOnlineIps),
            ("expiresAt", self.expiresAt),
        ] {
            if value < -1 {
                return Err(crate::AccountServiceError::Validation(format!(
                    "{name} 不能小于 -1"
                )));
            }
        }
        Ok(())
    }

    /// 任一限制为零都代表账号整体禁用，这一规则同时用于新认证和存量租约撤销。
    pub fn disabled(&self) -> bool {
        self.maxUploadBytesPerSecond == 0
            || self.maxDownloadBytesPerSecond == 0
            || self.maxConnections == 0
            || self.maxOnlineIps == 0
            || self.expiresAt == 0
    }

    /// 只用账号服务时间判定到期；负一永不过期，零已由 disabled 处理。
    pub fn expired(&self, serverTime: i64) -> bool {
        self.expiresAt > 0 && self.expiresAt < serverTime
    }
}

/// 内部账号记录包含密码摘要；只能停留在存储和认证边界，禁止直接序列化到公共 API。
#[derive(Clone, Debug)]
pub struct StoredAccount {
    pub accountId: String,
    pub username: String,
    pub passwordHash: Option<String>,
    pub passwordSalt: Option<String>,
    pub policy: AccountPolicy,
    pub policyRevision: i64,
    pub remark: Option<String>,
    pub createdAt: i64,
    pub updatedAt: i64,
}

/// 公共账号视图只暴露密码模式，不暴露摘要、盐或历史明文。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub accountId: String,
    pub username: String,
    pub passwordMode: PasswordMode,
    pub policy: AccountPolicy,
    pub policyRevision: i64,
    pub remark: Option<String>,
    pub createdAt: i64,
    pub updatedAt: i64,
    pub uploadedBytes: i64,
    pub downloadedBytes: i64,
    pub activeConnections: usize,
    pub onlineIps: usize,
}

/// 账号列表的业务状态筛选；省略该参数表示不过滤，未知文本由 Serde 直接拒绝。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum AccountStatusFilter {
    Available,
    Disabled,
    Expired,
}

/// 账号列表的到期类型筛选；零值禁用属于状态而不是到期类型，避免两个筛选维度互相覆盖。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum AccountExpirationFilter {
    Never,
    Scheduled,
    Expired,
}

/// 账号列表允许排序的字段白名单；枚举会在进入领域层前消除动态 SQL 字段注入面。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum AccountSortField {
    #[default]
    CreatedAt,
    Username,
    ExpiresAt,
    UploadedBytes,
    DownloadedBytes,
    TotalBytes,
    ActiveConnections,
    OnlineIps,
}

/// 账号列表排序方向；默认倒序与既有“最新创建”列表行为保持兼容。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SortOrder {
    Asc,
    #[default]
    Desc,
}

/// 描述外部账号查询的完整投影参数；过滤和排序必须先于 offset/limit 执行。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountQuery {
    #[serde(default)]
    pub offset: i64,
    #[serde(default = "defaultAccountQueryLimit")]
    pub limit: i64,
    pub search: Option<String>,
    pub status: Option<AccountStatusFilter>,
    pub expiration: Option<AccountExpirationFilter>,
    #[serde(default)]
    pub sort: AccountSortField,
    #[serde(default)]
    pub order: SortOrder,
}

/// 保持公共账号列表的默认页大小为 100；最大页大小由领域层统一校验。
fn defaultAccountQueryLimit() -> i64 {
    100
}

/// 区分任意非空密码和固定密码，避免用空字符串作为隐式控制值。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PasswordMode {
    Any,
    Fixed,
}

/// 创建账号时 password=null 表示任意非空密码，空字符串属于非法固定密码。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAccountRequest {
    pub username: String,
    pub password: Option<String>,
    #[serde(flatten)]
    pub policy: AccountPolicy,
    pub remark: Option<String>,
}

/// 更新只修改策略和备注；账号稳定标识与用户名不可变，密码使用独立端点处理。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateAccountRequest {
    pub policyRevision: i64,
    #[serde(flatten)]
    pub policy: AccountPolicy,
    pub remark: Option<String>,
}

/// 批量操作使用账号 ID 与策略修订号组成稳定选择，避免页面基于旧列表覆盖并发修改。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchAccountSelection {
    pub accountId: String,
    pub policyRevision: i64,
}

/// 批量策略更新只修改显式提供的字段；加时量只作用于已有正数到期时间。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchUpdateAccountsRequest {
    pub accounts: Vec<BatchAccountSelection>,
    pub maxOnlineIps: Option<i64>,
    pub maxConnections: Option<i64>,
    pub maxUploadBytesPerSecond: Option<i64>,
    pub maxDownloadBytesPerSecond: Option<i64>,
    pub extendByMilliseconds: Option<i64>,
}

/// 批量删除同样携带修订号，使“选择后删除”保持所见即所删的并发语义。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchDeleteAccountsRequest {
    pub accounts: Vec<BatchAccountSelection>,
}

/// 批量更新返回实际修改数；永不过期或禁用账号在仅加时时不会计入修改数。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateAccountsResponse {
    pub accounts: Vec<AccountView>,
    pub updatedAccounts: usize,
}

/// 批量删除返回事务内实际删除数量，页面据此给出准确反馈。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteAccountsResponse {
    pub deletedAccounts: usize,
}

/// 规则集视图保留正文和乐观锁修订号；客户端只通过当前启用规则下载端点读取正文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleSetView {
    pub ruleSetId: String,
    pub name: String,
    pub content: String,
    pub enabled: bool,
    pub revision: i64,
    pub createdAt: i64,
    pub updatedAt: i64,
}

/// 内部打包前置检查只暴露当前规则稳定标识和修订，不复制可能较大的 routing.txt 正文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRuleSetMetadata {
    pub id: String,
    pub revision: i64,
}

/// 新建规则集显式携带名称和完整 routing.txt 正文；默认关闭避免新记录替换线上配置。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRuleSetRequest {
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub enabled: bool,
}

/// 编辑规则集使用修订号保护并发管理页面，启用状态由独立互斥端点维护。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRuleSetRequest {
    pub revision: i64,
    pub name: String,
    pub content: String,
}

/// 开关操作携带当前修订号；开启目标时存储事务会同时关闭其它规则集。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetRuleSetEnabledRequest {
    pub revision: i64,
    pub enabled: bool,
}

/// 批量删除只接受稳定 ID；服务端在单事务内验证去重和存在性后整批提交。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchDeleteRuleSetsRequest {
    pub ruleSetIds: Vec<String>,
}

/// 批量删除返回实际删除数量，管理页面据此显示精确操作结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteRuleSetsResponse {
    pub deletedRuleSets: usize,
}

/// 打包器和规则下载共用无租约凭据校验请求，避免用虚假连接占用在线额度。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifyAccountCredentialsRequest {
    pub username: String,
    pub password: String,
}

/// 管理身份公开信息只提供账号、修订号和 API Key 指纹。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementIdentityView {
    pub username: String,
    pub credentialRevision: i64,
    pub apiKeyPrefix: String,
    pub apiKeyCreatedAt: i64,
}

/// 完整 API Key 只出现在直接凭据操作响应中，调用方不得放入长期快照。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyResponse {
    pub identity: ManagementIdentityView,
    pub apiKey: String,
}

/// 登录请求与 SOCKS5 账号完全分离，只认证唯一管理身份。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagementLoginRequest {
    pub username: String,
    pub password: String,
}

/// 修改管理身份只接收新账号和新密码；调用入口自身的管理会话或内部令牌已完成授权。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateManagementIdentityRequest {
    pub username: String,
    pub password: String,
}

/// 设置固定账号密码；空字符串在进入存储事务前明确拒绝。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetPasswordRequest {
    pub password: String,
}

/// 内部认证请求的 sourceIp 只能由受信任 SOCKS5 接受循环填写。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseAuthenticationRequest {
    pub connectionId: String,
    pub username: String,
    pub password: String,
    pub sourceIp: String,
}

/// 认证成功把稳定账号、租约和策略实例一次性绑定给数据面。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseAuthenticationResponse {
    pub serviceInstanceId: String,
    pub accountId: String,
    pub leaseId: String,
    pub username: String,
    pub policyRevision: i64,
    pub maxUploadBytesPerSecond: i64,
    pub maxDownloadBytesPerSecond: i64,
}

/// 数据面每次同步上报租约生命周期内的单调累计值，final 表示不再继续使用该租约。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseProgress {
    pub leaseId: String,
    pub connectionId: String,
    pub uploadedBytes: u64,
    pub downloadedBytes: u64,
    #[serde(rename = "final")]
    pub final_: bool,
}

/// 批次 ID 只在账号服务当前实例内有效，实例变化时数据面必须关闭旧连接。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseSynchronizationRequest {
    pub serviceInstanceId: String,
    pub batchId: String,
    pub leases: Vec<LeaseProgress>,
}

/// 单租约同步结果同时承载确认累计值、最新策略和撤销状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseSynchronizationResult {
    pub leaseId: String,
    pub acknowledgedUploadedBytes: u64,
    pub acknowledgedDownloadedBytes: u64,
    pub policyRevision: i64,
    pub maxUploadBytesPerSecond: i64,
    pub maxDownloadBytesPerSecond: i64,
    pub revoked: bool,
    pub errorCode: Option<String>,
}

/// 同步响应回显批次和服务实例，调用方可安全匹配并发中的旧响应。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseSynchronizationResponse {
    pub serviceInstanceId: String,
    pub batchId: String,
    pub leases: Vec<LeaseSynchronizationResult>,
}

/// 公共连接视图只展示身份、来源、时间和流量，不暴露目标地址或内部令牌。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    pub leaseId: String,
    pub accountId: String,
    pub connectionId: String,
    pub sourceIp: String,
    pub createdAt: i64,
    pub lastHeartbeatAt: i64,
    pub uploadedBytes: u64,
    pub downloadedBytes: u64,
    pub uploadBytesPerSecond: u64,
    pub downloadBytesPerSecond: u64,
    pub revoked: bool,
}

/// 管理首页统计来自账号表与当前内存租约的组合快照。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatistics {
    pub totalAccounts: i64,
    pub onlineAccounts: usize,
    pub onlineIps: usize,
    pub activeConnections: usize,
    pub uploadedBytes: i64,
    pub downloadedBytes: i64,
    pub uploadBytesPerSecond: u64,
    pub downloadBytesPerSecond: u64,
}

/// 审计日志只暴露管理操作的结构化结果，不包含密码、Key 或内部令牌等敏感材料。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuditLogView {
    pub auditId: String,
    pub occurredAt: i64,
    pub actorType: String,
    pub action: String,
    pub targetId: Option<String>,
    pub result: String,
    pub detailsJson: String,
}

/// 单日用量以 UTC 自 Unix 纪元起的天序号标识，避免数据库依赖本地时区规则。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsageView {
    pub utcDate: i64,
    pub uploadedBytes: i64,
    pub downloadedBytes: i64,
    pub acceptedConnections: i64,
    pub rejectedAuthentications: i64,
}

/// 账号用量响应组合累计计数与每日趋势，不暴露租约内部批次状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsageView {
    pub accountId: String,
    pub uploadedBytes: i64,
    pub downloadedBytes: i64,
    pub acceptedConnections: i64,
    pub rejectedAuthentications: i64,
    pub lastConnectedAt: Option<i64>,
    pub daily: Vec<DailyUsageView>,
}
