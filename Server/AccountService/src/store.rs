use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use crate::{
    AccountPolicy, AccountServiceError, AccountUsageView, ApiKeyResponse, AuditLogView,
    BatchAccountSelection, BatchDeleteAccountsRequest, BatchUpdateAccountsRequest,
    CreateAccountRequest, DailyUsageView, ManagementIdentityView, Result, StoredAccount,
    UpdateAccountRequest,
    credential::{
        ApiKeyDerivation, apiKeyMaterialIsCurrent, deriveApiKey, hashApiKey, hashPassword,
        newApiKeyMaterial, upgradeApiKeySalt, verifyApiKey, verifyPassword,
    },
    ruleSetStore::migrateLegacyRuleSetContent,
};

const schemaVersion: i64 = 4;
const defaultBusyTimeoutMilliseconds: i64 = 5_000;
const maximumUsernameBytes: usize = u8::MAX as usize;
const maximumPasswordBytes: usize = u8::MAX as usize;
const maximumRemarkBytes: usize = 512;
const maximumBatchAccounts: usize = 500;

/// 管理身份只在存储模块内部存在，调用方只能取得脱敏视图或当次派生的完整 Key。
struct ManagementIdentityRecord {
    username: String,
    passwordHash: String,
    passwordSalt: String,
    credentialRevision: i64,
    apiKeyHash: String,
    apiKeyPrefix: String,
    apiKeySalt: String,
    apiKeyId: String,
    apiKeyCreatedAt: i64,
    databaseInstanceId: String,
    browserSessionRevision: i64,
    updatedAt: i64,
}

/// SQLite 是账号服务唯一持久化实现；连接互斥只保护短事务，网络等待不得持有该锁。
pub struct AccountStore {
    pub(crate) connection: Mutex<Connection>,
}

impl AccountStore {
    /// 打开独占数据库并应用 WAL、外键和迁移；任何迁移失败都会阻止服务进入就绪状态。
    pub fn open(databasePath: &Path) -> Result<Self> {
        let mut connection = Connection::open(databasePath)?;
        configureConnection(&connection)?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// 创建测试专用内存数据库，执行与生产完全相同的约束和迁移。
    pub fn openInMemory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        configureConnection(&connection)?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// 判断管理身份是否已经初始化；启动流程用它区分首次建库与普通重启，避免用默认凭据覆盖或校验已修改身份。
    pub fn managementInitialized(&self) -> Result<bool> {
        let connection = self.connection.lock();
        Ok(readManagementIdentity(&connection)?.is_some())
    }

    /// 首次创建默认管理身份；重复调用会校验现有身份并重新派生当前 Key，不重置修订号。
    pub fn bootstrapManagement(&self, username: &str, password: &str) -> Result<ApiKeyResponse> {
        validateCredentialText("管理员账号", username, maximumUsernameBytes)?;
        validateCredentialText("管理员密码", password, maximumPasswordBytes)?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        if let Some(mut identity) = readManagementIdentity(&transaction)? {
            if identity.username != username
                || !verifyPassword(password, &identity.passwordHash, &identity.passwordSalt)
            {
                return Err(AccountServiceError::ManagementAuthenticationFailed);
            }
            let apiKey = recoverManagementApiKey(&transaction, &mut identity)?;
            transaction.commit()?;
            return Ok(ApiKeyResponse {
                identity: managementView(&identity),
                apiKey,
            });
        }

        let now = currentTimeMilliseconds();
        let passwordDigest = hashPassword(password)?;
        let (apiKeySalt, apiKeyId) = newApiKeyMaterial();
        let databaseInstanceId = Uuid::new_v4().to_string();
        let credentialRevision = 1_i64;
        let apiKey = deriveApiKey(ApiKeyDerivation {
            username,
            passwordHash: &passwordDigest.encodedHash,
            credentialRevision,
            databaseInstanceId: &databaseInstanceId,
            encodedSalt: &apiKeySalt,
            keyId: &apiKeyId,
        })?;
        let apiKeyHash = hashApiKey(&apiKey);
        let apiKeyPrefix = displayApiKeyPrefix(&apiKeyId);
        transaction.execute(
            "INSERT INTO managementIdentity (
                singletonId, username, passwordHash, passwordSalt, credentialRevision,
                apiKeyHash, apiKeyPrefix, apiKeySalt, apiKeyId, apiKeyCreatedAt,
                databaseInstanceId, browserSessionRevision, updatedAt
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?9)",
            params![
                username,
                passwordDigest.encodedHash,
                passwordDigest.encodedSalt,
                credentialRevision,
                apiKeyHash,
                apiKeyPrefix,
                apiKeySalt,
                apiKeyId,
                now,
                databaseInstanceId,
            ],
        )?;
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "internal",
                action: "management.bootstrap",
                accountId: None,
                result: "success",
                details: "{}",
            },
        )?;
        transaction.commit()?;
        Ok(ApiKeyResponse {
            identity: ManagementIdentityView {
                username: username.to_owned(),
                credentialRevision,
                apiKeyPrefix,
                apiKeyCreatedAt: now,
            },
            apiKey,
        })
    }

    /// 从当前不可逆凭据摘要恢复完整 Key；调用入口必须已经完成管理会话或内部令牌授权。
    ///
    /// 旧数据库首次读取时会把早期明文密码派生方式留下的摘要原子更新为当前算法结果。迁移只发生一次，
    /// 后续读取不改变修订号、Key ID 或创建时间，因此“获取 Key”本身没有轮换语义。
    pub fn managementApiKey(&self) -> Result<ApiKeyResponse> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let mut identity = readManagementIdentity(&transaction)?
            .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
        let apiKey = recoverManagementApiKey(&transaction, &mut identity)?;
        transaction.commit()?;
        Ok(ApiKeyResponse {
            identity: managementView(&identity),
            apiKey,
        })
    }

    /// 替换管理身份并在同一事务中按新账号密码派生 Key、撤销会话和写入审计记录。
    ///
    /// 运行上下文：调用入口已经用浏览器会话或内部令牌完成授权；参数校验或 SQLite 提交失败时不改变旧身份。
    pub fn updateManagementIdentity(
        &self,
        username: &str,
        password: &str,
    ) -> Result<ApiKeyResponse> {
        validateCredentialText("管理员账号", username, maximumUsernameBytes)?;
        validateCredentialText("管理员密码", password, maximumPasswordBytes)?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let current = readManagementIdentity(&transaction)?
            .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
        let now = currentTimeMilliseconds();
        let passwordDigest = hashPassword(password)?;
        let (apiKeySalt, apiKeyId) = newApiKeyMaterial();
        let credentialRevision = current.credentialRevision.saturating_add(1);
        let browserSessionRevision = current.browserSessionRevision.saturating_add(1);
        let apiKey = deriveApiKey(ApiKeyDerivation {
            username,
            passwordHash: &passwordDigest.encodedHash,
            credentialRevision,
            databaseInstanceId: &current.databaseInstanceId,
            encodedSalt: &apiKeySalt,
            keyId: &apiKeyId,
        })?;
        let apiKeyHash = hashApiKey(&apiKey);
        let apiKeyPrefix = displayApiKeyPrefix(&apiKeyId);
        transaction.execute(
            "UPDATE managementIdentity SET
                username = ?1, passwordHash = ?2, passwordSalt = ?3,
                credentialRevision = ?4, apiKeyHash = ?5, apiKeyPrefix = ?6,
                apiKeySalt = ?7, apiKeyId = ?8, apiKeyCreatedAt = ?9,
                browserSessionRevision = ?10, updatedAt = ?9
             WHERE singletonId = 1",
            params![
                username,
                passwordDigest.encodedHash,
                passwordDigest.encodedSalt,
                credentialRevision,
                apiKeyHash,
                apiKeyPrefix,
                apiKeySalt,
                apiKeyId,
                now,
                browserSessionRevision,
            ],
        )?;
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "management.identity.update",
                accountId: None,
                result: "success",
                details: "{}",
            },
        )?;
        transaction.commit()?;
        Ok(ApiKeyResponse {
            identity: ManagementIdentityView {
                username: username.to_owned(),
                credentialRevision,
                apiKeyPrefix,
                apiKeyCreatedAt: now,
            },
            apiKey,
        })
    }

    /// 校验管理登录并返回当前会话修订号；调用方用该值批量撤销旧 Cookie。
    pub fn authenticateManagement(&self, username: &str, password: &str) -> Result<i64> {
        let connection = self.connection.lock();
        let identity = readManagementIdentity(&connection)?
            .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
        if identity.username != username
            || !verifyPassword(password, &identity.passwordHash, &identity.passwordSalt)
        {
            return Err(AccountServiceError::ManagementAuthenticationFailed);
        }
        Ok(identity.browserSessionRevision)
    }

    /// 返回持久浏览器会话的签名材料与当前修订；材料源自管理密码摘要，不新增旁路秘密文件。
    ///
    /// 运行上下文：HTTP 层只用材料签名或验证 HttpOnly Cookie；身份更新会同时改变修订号和摘要。
    pub fn browserSessionMaterial(&self) -> Result<(String, i64)> {
        let connection = self.connection.lock();
        let identity = readManagementIdentity(&connection)?
            .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
        Ok((
            format!("{}:{}", identity.passwordHash, identity.passwordSalt),
            identity.browserSessionRevision,
        ))
    }

    /// 校验 Bearer Key 摘要；Key ID 只是展示信息，不能替代完整摘要比较。
    pub fn authenticateApiKey(&self, apiKey: &str) -> Result<()> {
        let connection = self.connection.lock();
        let identity = readManagementIdentity(&connection)?
            .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
        if !verifyApiKey(apiKey, &identity.apiKeyHash) {
            return Err(AccountServiceError::ManagementAuthenticationFailed);
        }
        Ok(())
    }

    /// 返回脱敏管理身份，用于设置页和远程管理页面展示。
    pub fn managementIdentity(&self) -> Result<ManagementIdentityView> {
        let connection = self.connection.lock();
        let identity = readManagementIdentity(&connection)?
            .ok_or(AccountServiceError::ManagementAuthenticationFailed)?;
        Ok(managementView(&identity))
    }

    /// 创建账号及零值统计行；用户名冲突转换成稳定领域错误。
    pub fn createAccount(&self, request: &CreateAccountRequest) -> Result<StoredAccount> {
        validateAccountRequest(request)?;
        let passwordDigest = request.password.as_deref().map(hashPassword).transpose()?;
        let now = currentTimeMilliseconds();
        let accountId = Uuid::new_v4().to_string();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let insertResult = transaction.execute(
            "INSERT INTO accounts (
                accountId, username, passwordHash, passwordSalt,
                maxUploadBytesPerSecond, maxDownloadBytesPerSecond,
                maxConnections, maxOnlineIps, expiresAt, policyRevision,
                remark, createdAt, updatedAt
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?11)",
            params![
                accountId,
                request.username,
                passwordDigest.as_ref().map(|digest| &digest.encodedHash),
                passwordDigest.as_ref().map(|digest| &digest.encodedSalt),
                request.policy.maxUploadBytesPerSecond,
                request.policy.maxDownloadBytesPerSecond,
                request.policy.maxConnections,
                request.policy.maxOnlineIps,
                request.policy.expiresAt,
                request.remark,
                now,
            ],
        );
        if let Err(error) = insertResult {
            return if isUniqueConstraint(&error) {
                Err(AccountServiceError::AccountConflict)
            } else {
                Err(error.into())
            };
        }
        transaction.execute(
            "INSERT INTO usageCounters (
                accountId, uploadedBytes, downloadedBytes, acceptedConnections,
                rejectedAuthentications, lastConnectedAt, updatedAt
             ) VALUES (?1, 0, 0, 0, 0, NULL, ?2)",
            params![accountId, now],
        )?;
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "account.create",
                accountId: Some(&accountId),
                result: "success",
                details: "{}",
            },
        )?;
        transaction.commit()?;
        // 事务提交后先释放 SQLite 连接锁，再通过公共查询入口读取完整记录，禁止同线程重入互斥锁。
        drop(connection);
        self.accountById(&accountId)
    }

    /// 按稳定账号 ID 返回内部记录；不存在时不暴露数据库查询细节。
    pub fn accountById(&self, accountId: &str) -> Result<StoredAccount> {
        let connection = self.connection.lock();
        queryAccount(&connection, "accountId = ?1", accountId)?
            .ok_or(AccountServiceError::AccountNotFound)
    }

    /// 按 RFC 1929 用户名精确匹配内部记录，不做大小写或 Unicode 规范化。
    pub fn accountByUsername(&self, username: &str) -> Result<Option<StoredAccount>> {
        let connection = self.connection.lock();
        queryAccount(&connection, "username = ?1", username)
    }

    /// 一次读取完整账号快照，供领域层先执行安全白名单过滤排序、再应用外部分页。
    ///
    /// 运行上下文：公共查询不能把 offset/limit 下推到 SQLite 后再过滤，否则搜索结果会跨页遗漏。
    /// 失败语义：数据库读取失败返回原始存储错误，不返回部分账号集合。
    pub fn listAllAccounts(&self) -> Result<Vec<StoredAccount>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT accountId, username, passwordHash, passwordSalt,
                    maxUploadBytesPerSecond, maxDownloadBytesPerSecond,
                    maxConnections, maxOnlineIps, expiresAt, policyRevision,
                    remark, createdAt, updatedAt
             FROM accounts",
        )?;
        let rows = statement.query_map([], mapAccountRow)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// 在 SQLite 事务内校验并递增策略修订号，调用方持有账号操作锁后再发布内存策略。
    pub fn updateAccount(
        &self,
        accountId: &str,
        request: &UpdateAccountRequest,
    ) -> Result<StoredAccount> {
        request.policy.validate()?;
        validateRemark(request.remark.as_deref())?;
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let currentRevision: Option<i64> = transaction
            .query_row(
                "SELECT policyRevision FROM accounts WHERE accountId = ?1",
                [accountId],
                |row| row.get(0),
            )
            .optional()?;
        let currentRevision = currentRevision.ok_or(AccountServiceError::AccountNotFound)?;
        if currentRevision != request.policyRevision {
            return Err(AccountServiceError::PolicyRevisionConflict { currentRevision });
        }
        let nextRevision = currentRevision.saturating_add(1);
        transaction.execute(
            "UPDATE accounts SET
                maxUploadBytesPerSecond = ?2, maxDownloadBytesPerSecond = ?3,
                maxConnections = ?4, maxOnlineIps = ?5, expiresAt = ?6,
                policyRevision = ?7, remark = ?8, updatedAt = ?9
             WHERE accountId = ?1",
            params![
                accountId,
                request.policy.maxUploadBytesPerSecond,
                request.policy.maxDownloadBytesPerSecond,
                request.policy.maxConnections,
                request.policy.maxOnlineIps,
                request.policy.expiresAt,
                nextRevision,
                request.remark,
                now,
            ],
        )?;
        let auditDetails = format!("{{\"policyRevision\":{nextRevision}}}");
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "account.update",
                accountId: Some(accountId),
                result: "success",
                details: &auditDetails,
            },
        )?;
        transaction.commit()?;
        // 与创建路径相同，重新读取前必须释放连接锁，否则 parking_lot 互斥锁会发生自锁。
        drop(connection);
        self.accountById(accountId)
    }

    /// 在单个 SQLite 事务内校验并更新全部选中账号，任一账号缺失或修订冲突都会回滚整批。
    ///
    /// 运行上下文：领域层持有账号操作锁；可选字段代表“保持原值”，加时只基于各账号当前正数
    /// 到期时间计算，已过期时间不会改用服务器当前时间。失败时不返回部分成功结果。
    pub fn updateAccountsBatch(
        &self,
        request: &BatchUpdateAccountsRequest,
    ) -> Result<(Vec<StoredAccount>, usize)> {
        validateBatchSelections(&request.accounts)?;
        validateBatchMutation(request)?;
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let mut accounts = Vec::with_capacity(request.accounts.len());
        let mut updatedAccounts = 0_usize;

        for selection in &request.accounts {
            let mut account = queryAccount(&transaction, "accountId = ?1", &selection.accountId)?
                .ok_or(AccountServiceError::AccountNotFound)?;
            if account.policyRevision != selection.policyRevision {
                return Err(AccountServiceError::PolicyRevisionConflict {
                    currentRevision: account.policyRevision,
                });
            }
            let changed = applyBatchPolicy(&mut account.policy, request)?;
            if changed {
                account.policy.validate()?;
                account.policyRevision = account.policyRevision.saturating_add(1);
                account.updatedAt = now;
                updateStoredAccountPolicy(&transaction, &account)?;
                let auditDetails = format!("{{\"policyRevision\":{}}}", account.policyRevision);
                insertAudit(
                    &transaction,
                    AuditEntry {
                        occurredAt: now,
                        actorType: "management",
                        action: "account.batchUpdate",
                        accountId: Some(&account.accountId),
                        result: "success",
                        details: &auditDetails,
                    },
                )?;
                updatedAccounts = updatedAccounts.saturating_add(1);
            }
            accounts.push(account);
        }
        transaction.commit()?;
        Ok((accounts, updatedAccounts))
    }

    /// 在单个事务内删除完整选择集；修订校验发生在任何删除之前，避免批量操作产生半成功状态。
    pub fn deleteAccountsBatch(&self, request: &BatchDeleteAccountsRequest) -> Result<usize> {
        validateBatchSelections(&request.accounts)?;
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        for selection in &request.accounts {
            let currentRevision = transaction
                .query_row(
                    "SELECT policyRevision FROM accounts WHERE accountId = ?1",
                    [&selection.accountId],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or(AccountServiceError::AccountNotFound)?;
            if currentRevision != selection.policyRevision {
                return Err(AccountServiceError::PolicyRevisionConflict { currentRevision });
            }
        }
        for selection in &request.accounts {
            transaction.execute(
                "DELETE FROM accounts WHERE accountId = ?1",
                [&selection.accountId],
            )?;
            insertAudit(
                &transaction,
                AuditEntry {
                    occurredAt: now,
                    actorType: "management",
                    action: "account.batchDelete",
                    accountId: Some(&selection.accountId),
                    result: "success",
                    details: "{}",
                },
            )?;
        }
        transaction.commit()?;
        Ok(request.accounts.len())
    }

    /// 设置固定密码；空字符串必须在事务外拒绝，不能误写成任意密码模式。
    pub fn setAccountPassword(&self, accountId: &str, password: &str) -> Result<()> {
        validateCredentialText("账号密码", password, maximumPasswordBytes)?;
        let digest = hashPassword(password)?;
        self.replaceAccountPassword(accountId, Some(digest))
    }

    /// 清除摘要后账号接受任意非空 RFC 1929 密码。
    pub fn clearAccountPassword(&self, accountId: &str) -> Result<()> {
        self.replaceAccountPassword(accountId, None)
    }

    /// 删除账号、统计和每日聚合；外键级联保持数据库中不残留孤儿行。
    pub fn deleteAccount(&self, accountId: &str) -> Result<()> {
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        if transaction.execute("DELETE FROM accounts WHERE accountId = ?1", [accountId])? == 0 {
            return Err(AccountServiceError::AccountNotFound);
        }
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "account.delete",
                accountId: Some(accountId),
                result: "success",
                details: "{}",
            },
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 批量累加已确认流量和连接统计；每个账号只执行一次 UPSERT，避免每个网络块写库。
    pub fn applyUsage(&self, usageByAccount: &HashMap<String, UsageIncrement>) -> Result<()> {
        if usageByAccount.is_empty() {
            return Ok(());
        }
        let now = currentTimeMilliseconds();
        let utcDate = now / 86_400_000;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        for (accountId, usage) in usageByAccount {
            transaction.execute(
                "UPDATE usageCounters SET
                    uploadedBytes = uploadedBytes + ?2,
                    downloadedBytes = downloadedBytes + ?3,
                    acceptedConnections = acceptedConnections + ?4,
                    rejectedAuthentications = rejectedAuthentications + ?5,
                    lastConnectedAt = CASE WHEN ?4 > 0 THEN ?6 ELSE lastConnectedAt END,
                    updatedAt = ?6
                 WHERE accountId = ?1",
                params![
                    accountId,
                    usage.uploadedBytes,
                    usage.downloadedBytes,
                    usage.acceptedConnections,
                    usage.rejectedAuthentications,
                    now,
                ],
            )?;
            transaction.execute(
                "INSERT INTO usageDaily (
                    accountId, utcDate, uploadedBytes, downloadedBytes,
                    acceptedConnections, rejectedAuthentications
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(accountId, utcDate) DO UPDATE SET
                    uploadedBytes = uploadedBytes + excluded.uploadedBytes,
                    downloadedBytes = downloadedBytes + excluded.downloadedBytes,
                    acceptedConnections = acceptedConnections + excluded.acceptedConnections,
                    rejectedAuthentications = rejectedAuthentications + excluded.rejectedAuthentications",
                params![
                    accountId,
                    utcDate,
                    usage.uploadedBytes,
                    usage.downloadedBytes,
                    usage.acceptedConnections,
                    usage.rejectedAuthentications,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// 返回每个账号累计字节，用于把内存在线态与持久化统计组合成公开视图。
    pub fn usageTotals(&self) -> Result<HashMap<String, (i64, i64)>> {
        let connection = self.connection.lock();
        let mut statement = connection
            .prepare("SELECT accountId, uploadedBytes, downloadedBytes FROM usageCounters")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?;
        rows.collect::<std::result::Result<HashMap<_, _>, _>>()
            .map_err(Into::into)
    }

    /// 返回账号总数和累计流量，实时在线值由租约注册表补齐。
    pub fn aggregateStatistics(&self) -> Result<(i64, i64, i64)> {
        let connection = self.connection.lock();
        connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM accounts),
                    COALESCE((SELECT SUM(uploadedBytes) FROM usageCounters), 0),
                    COALESCE((SELECT SUM(downloadedBytes) FROM usageCounters), 0)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(Into::into)
    }

    /// 读取数据库迁移版本，供内部健康检查判定持久化层是否完整就绪。
    pub fn currentSchemaVersion(&self) -> Result<i64> {
        let connection = self.connection.lock();
        connection
            .query_row("SELECT MAX(version) FROM schemaMigrations", [], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .map(|version| version.unwrap_or_default())
            .map_err(Into::into)
    }

    /// 按时间倒序返回脱敏审计记录；分页上限由 HTTP 层约束，数据库层只负责稳定排序。
    pub fn listAuditLogs(&self, offset: i64, limit: i64) -> Result<Vec<AuditLogView>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT auditId, occurredAt, actorType, action, accountId, result, details
             FROM auditLogs ORDER BY occurredAt DESC, auditId ASC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![limit, offset], |row| {
            Ok(AuditLogView {
                auditId: row.get(0)?,
                occurredAt: row.get(1)?,
                actorType: row.get(2)?,
                action: row.get(3)?,
                targetId: row.get(4)?,
                result: row.get(5)?,
                detailsJson: row.get(6)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// 查询单账号累计和每日用量；账号不存在与尚无流量明确区分，避免页面误显示空账号。
    pub fn accountUsage(&self, accountId: &str) -> Result<AccountUsageView> {
        let connection = self.connection.lock();
        let totals = connection
            .query_row(
                "SELECT uploadedBytes, downloadedBytes, acceptedConnections,
                        rejectedAuthentications, lastConnectedAt
                 FROM usageCounters WHERE accountId = ?1",
                [accountId],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AccountServiceError::AccountNotFound)?;
        let mut statement = connection.prepare(
            "SELECT utcDate, uploadedBytes, downloadedBytes, acceptedConnections,
                    rejectedAuthentications
             FROM usageDaily WHERE accountId = ?1 ORDER BY utcDate ASC",
        )?;
        let daily = statement
            .query_map([accountId], |row| {
                Ok(DailyUsageView {
                    utcDate: row.get(0)?,
                    uploadedBytes: row.get(1)?,
                    downloadedBytes: row.get(2)?,
                    acceptedConnections: row.get(3)?,
                    rejectedAuthentications: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(AccountUsageView {
            accountId: accountId.to_owned(),
            uploadedBytes: totals.0,
            downloadedBytes: totals.1,
            acceptedConnections: totals.2,
            rejectedAuthentications: totals.3,
            lastConnectedAt: totals.4,
            daily,
        })
    }

    /// 统一替换账号密码摘要并递增策略修订，确保现有租约同步能观察到凭据变化。
    fn replaceAccountPassword(
        &self,
        accountId: &str,
        digest: Option<crate::credential::PasswordDigest>,
    ) -> Result<()> {
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE accounts SET passwordHash = ?2, passwordSalt = ?3,
                policyRevision = policyRevision + 1, updatedAt = ?4
             WHERE accountId = ?1",
            params![
                accountId,
                digest.as_ref().map(|value| &value.encodedHash),
                digest.as_ref().map(|value| &value.encodedSalt),
                now,
            ],
        )?;
        if updated == 0 {
            return Err(AccountServiceError::AccountNotFound);
        }
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "account.password.update",
                accountId: Some(accountId),
                result: "success",
                details: "{}",
            },
        )?;
        transaction.commit()?;
        Ok(())
    }
}

/// 聚合单次数据库提交的账号流量和连接计数。
#[derive(Clone, Debug, Default)]
pub struct UsageIncrement {
    pub uploadedBytes: i64,
    pub downloadedBytes: i64,
    pub acceptedConnections: i64,
    pub rejectedAuthentications: i64,
}

/// 为 SQLite 连接设置生产一致性约束；测试内存库不支持 WAL 时 SQLite 会保留 memory 模式。
fn configureConnection(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "busy_timeout", defaultBusyTimeoutMilliseconds)?;
    let _ = connection.pragma_update(None, "journal_mode", "WAL");
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// 在单一排他事务中依次建表并升级规则协议；版本摘要标识每个已应用阶段。
///
/// 运行上下文：数据库连接完成 WAL/外键配置后、对外服务前调用；`connection` 必须由启动线程独占。
/// 修复理由：v3 必须在下发前为旧规则补齐显式 DNS，并复用当前正文校验器阻止坏规则进入就绪态。
/// 失败语义：任一 SQL 或正文校验失败都回滚全部变更，数据库版本不推进，服务启动失败。
fn migrate(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS schemaMigrations (
            version INTEGER PRIMARY KEY,
            appliedAt INTEGER NOT NULL,
            checksum TEXT NOT NULL
         );",
    )?;
    let existingVersion: Option<i64> = transaction
        .query_row("SELECT MAX(version) FROM schemaMigrations", [], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    if existingVersion.unwrap_or_default() > schemaVersion {
        return Err(AccountServiceError::StateConflict(
            "数据库版本高于当前服务支持版本".to_owned(),
        ));
    }
    let mut currentVersion = existingVersion.unwrap_or_default();
    if currentVersion == 0 {
        transaction.execute_batch(
            "CREATE TABLE managementIdentity (
                singletonId INTEGER PRIMARY KEY CHECK(singletonId = 1),
                username TEXT NOT NULL,
                passwordHash TEXT NOT NULL,
                passwordSalt TEXT NOT NULL,
                credentialRevision INTEGER NOT NULL CHECK(credentialRevision > 0),
                apiKeyHash TEXT NOT NULL,
                apiKeyPrefix TEXT NOT NULL,
                apiKeySalt TEXT NOT NULL,
                apiKeyId TEXT NOT NULL,
                apiKeyCreatedAt INTEGER NOT NULL,
                databaseInstanceId TEXT NOT NULL,
                browserSessionRevision INTEGER NOT NULL CHECK(browserSessionRevision > 0),
                updatedAt INTEGER NOT NULL
             );
             CREATE TABLE accounts (
                accountId TEXT PRIMARY KEY,
                username TEXT NOT NULL UNIQUE,
                passwordHash TEXT,
                passwordSalt TEXT,
                maxUploadBytesPerSecond INTEGER NOT NULL CHECK(maxUploadBytesPerSecond >= -1),
                maxDownloadBytesPerSecond INTEGER NOT NULL CHECK(maxDownloadBytesPerSecond >= -1),
                maxConnections INTEGER NOT NULL CHECK(maxConnections >= -1),
                maxOnlineIps INTEGER NOT NULL CHECK(maxOnlineIps >= -1),
                expiresAt INTEGER NOT NULL CHECK(expiresAt >= -1),
                policyRevision INTEGER NOT NULL CHECK(policyRevision > 0),
                remark TEXT,
                createdAt INTEGER NOT NULL,
                updatedAt INTEGER NOT NULL,
                CHECK((passwordHash IS NULL AND passwordSalt IS NULL)
                    OR (passwordHash IS NOT NULL AND passwordSalt IS NOT NULL))
             );
             CREATE TABLE usageCounters (
                accountId TEXT PRIMARY KEY REFERENCES accounts(accountId) ON DELETE CASCADE,
                uploadedBytes INTEGER NOT NULL CHECK(uploadedBytes >= 0),
                downloadedBytes INTEGER NOT NULL CHECK(downloadedBytes >= 0),
                acceptedConnections INTEGER NOT NULL CHECK(acceptedConnections >= 0),
                rejectedAuthentications INTEGER NOT NULL CHECK(rejectedAuthentications >= 0),
                lastConnectedAt INTEGER,
                updatedAt INTEGER NOT NULL
             );
             CREATE TABLE usageDaily (
                accountId TEXT NOT NULL REFERENCES accounts(accountId) ON DELETE CASCADE,
                utcDate INTEGER NOT NULL,
                uploadedBytes INTEGER NOT NULL CHECK(uploadedBytes >= 0),
                downloadedBytes INTEGER NOT NULL CHECK(downloadedBytes >= 0),
                acceptedConnections INTEGER NOT NULL CHECK(acceptedConnections >= 0),
                rejectedAuthentications INTEGER NOT NULL CHECK(rejectedAuthentications >= 0),
                PRIMARY KEY(accountId, utcDate)
             );
             CREATE TABLE auditLogs (
                auditId TEXT PRIMARY KEY,
                occurredAt INTEGER NOT NULL,
                actorType TEXT NOT NULL,
                actorFingerprint TEXT NOT NULL,
                action TEXT NOT NULL,
                accountId TEXT,
                result TEXT NOT NULL,
                details TEXT NOT NULL
             );
             CREATE INDEX accountsCreatedAtIndex ON accounts(createdAt DESC);
             CREATE INDEX auditOccurredAtIndex ON auditLogs(occurredAt DESC);",
        )?;
        transaction.execute(
            "INSERT INTO schemaMigrations(version, appliedAt, checksum) VALUES (?1, ?2, ?3)",
            params![
                1_i64,
                currentTimeMilliseconds(),
                "account-service-schema-v1"
            ],
        )?;
        currentVersion = 1;
    }
    if currentVersion < 2 {
        transaction.execute_batch(
            "CREATE TABLE ruleSets (
                ruleSetId TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                content TEXT NOT NULL,
                enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
                revision INTEGER NOT NULL CHECK(revision > 0),
                createdAt INTEGER NOT NULL,
                updatedAt INTEGER NOT NULL
             );
             CREATE UNIQUE INDEX ruleSetsSingleEnabledIndex
                ON ruleSets(enabled) WHERE enabled = 1;
             CREATE INDEX ruleSetsUpdatedAtIndex ON ruleSets(updatedAt DESC);",
        )?;
        transaction.execute(
            "INSERT INTO schemaMigrations(version, appliedAt, checksum) VALUES (?1, ?2, ?3)",
            params![
                2_i64,
                currentTimeMilliseconds(),
                "account-service-rulesets-schema-v2"
            ],
        )?;
        currentVersion = 2;
    }
    if currentVersion < 3 {
        let ruleSets = {
            let mut statement = transaction
                .prepare("SELECT ruleSetId, content FROM ruleSets ORDER BY ruleSetId ASC")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let migrationTime = currentTimeMilliseconds();
        for (ruleSetId, content) in ruleSets {
            let Some(migratedContent) = migrateLegacyRuleSetContent(&content)? else {
                continue;
            };
            transaction.execute(
                "UPDATE ruleSets SET content = ?2, revision = revision + 1,
                    updatedAt = MAX(updatedAt + 1, ?3) WHERE ruleSetId = ?1",
                params![ruleSetId, migratedContent, migrationTime],
            )?;
        }
        transaction.execute(
            "INSERT INTO schemaMigrations(version, appliedAt, checksum) VALUES (?1, ?2, ?3)",
            params![
                3_i64,
                migrationTime,
                "account-service-explicit-dns-schema-v3"
            ],
        )?;
        currentVersion = 3;
    }
    if currentVersion < 4 {
        // SOCKS5 允许任意 UTF-8 用户名，但 HTTP Basic 使用冒号分隔用户名与密码；
        // 账号同时服务两条认证链，因此迁移必须在对外监听前拒绝无法无损编码的历史账号。
        let invalidUsername = {
            let mut statement =
                transaction.prepare("SELECT username FROM accounts ORDER BY accountId ASC")?;
            let usernames = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            usernames
                .into_iter()
                .find(|value| !isSharedProtocolUsername(value))
        };
        if invalidUsername.is_some() {
            // 账号名属于认证材料的一部分，迁移错误会被监督器写入启动日志；这里只公开静态原因，
            // 管理员可离线检查数据库，服务日志不得复制原始账号标识。
            return Err(AccountServiceError::Validation(
                "数据库存在无法用于共享认证协议的历史账号".to_owned(),
            ));
        }
        transaction.execute(
            "INSERT INTO schemaMigrations(version, appliedAt, checksum) VALUES (?1, ?2, ?3)",
            params![
                4_i64,
                currentTimeMilliseconds(),
                "account-service-basic-compatible-usernames-v4"
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

/// 从任意 Connection/Transaction 读取唯一管理身份。
fn readManagementIdentity(connection: &Connection) -> Result<Option<ManagementIdentityRecord>> {
    connection
        .query_row(
            "SELECT username, passwordHash, passwordSalt, credentialRevision,
                    apiKeyHash, apiKeyPrefix, apiKeySalt, apiKeyId, apiKeyCreatedAt,
                    databaseInstanceId, browserSessionRevision, updatedAt
             FROM managementIdentity WHERE singletonId = 1",
            [],
            |row| {
                Ok(ManagementIdentityRecord {
                    username: row.get(0)?,
                    passwordHash: row.get(1)?,
                    passwordSalt: row.get(2)?,
                    credentialRevision: row.get(3)?,
                    apiKeyHash: row.get(4)?,
                    apiKeyPrefix: row.get(5)?,
                    apiKeySalt: row.get(6)?,
                    apiKeyId: row.get(7)?,
                    apiKeyCreatedAt: row.get(8)?,
                    databaseInstanceId: row.get(9)?,
                    browserSessionRevision: row.get(10)?,
                    updatedAt: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

/// 只执行确定性派生；调用方负责验证或迁移持久化摘要，禁止把密码摘要带出存储模块。
fn deriveCurrentApiKeyUnchecked(identity: &ManagementIdentityRecord) -> Result<String> {
    let apiKey = deriveApiKey(ApiKeyDerivation {
        username: &identity.username,
        passwordHash: &identity.passwordHash,
        credentialRevision: identity.credentialRevision,
        databaseInstanceId: &identity.databaseInstanceId,
        encodedSalt: &identity.apiKeySalt,
        keyId: &identity.apiKeyId,
    })?;
    Ok(apiKey)
}

/// 恢复当前 Key，并以盐版本标记区分“旧算法迁移”与“当前摘要损坏”。
///
/// 运行上下文：调用方持有管理身份写事务。旧盐在成功解码和派生后原子升级；当前版本摘要不一致
/// 直接返回凭据错误，禁止把数据库损坏误判为兼容迁移并静默覆盖。
fn recoverManagementApiKey(
    transaction: &Transaction<'_>,
    identity: &mut ManagementIdentityRecord,
) -> Result<String> {
    let apiKey = deriveCurrentApiKeyUnchecked(identity)?;
    if apiKeyMaterialIsCurrent(&identity.apiKeySalt) {
        if !verifyApiKey(&apiKey, &identity.apiKeyHash) {
            return Err(AccountServiceError::Credential);
        }
        return Ok(apiKey);
    }

    identity.apiKeySalt = upgradeApiKeySalt(&identity.apiKeySalt);
    identity.apiKeyHash = hashApiKey(&apiKey);
    transaction.execute(
        "UPDATE managementIdentity SET apiKeyHash = ?1, apiKeySalt = ?2 WHERE singletonId = 1",
        params![identity.apiKeyHash, identity.apiKeySalt],
    )?;
    Ok(apiKey)
}

/// 生成不含密钥材料的管理身份视图。
fn managementView(identity: &ManagementIdentityRecord) -> ManagementIdentityView {
    let _ = identity.updatedAt;
    ManagementIdentityView {
        username: identity.username.clone(),
        credentialRevision: identity.credentialRevision,
        apiKeyPrefix: identity.apiKeyPrefix.clone(),
        apiKeyCreatedAt: identity.apiKeyCreatedAt,
    }
}

/// 查询账号的 SQL 前缀固定，whereClause 只能由本模块常量调用，不能接收外部文本。
fn queryAccount(
    connection: &Connection,
    whereClause: &str,
    value: &str,
) -> Result<Option<StoredAccount>> {
    let sql = format!(
        "SELECT accountId, username, passwordHash, passwordSalt,
                maxUploadBytesPerSecond, maxDownloadBytesPerSecond,
                maxConnections, maxOnlineIps, expiresAt, policyRevision,
                remark, createdAt, updatedAt
         FROM accounts WHERE {whereClause}"
    );
    connection
        .query_row(&sql, [value], mapAccountRow)
        .optional()
        .map_err(Into::into)
}

/// 把固定列序映射成内部账号记录；列顺序只在本模块维护。
fn mapAccountRow(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredAccount> {
    Ok(StoredAccount {
        accountId: row.get(0)?,
        username: row.get(1)?,
        passwordHash: row.get(2)?,
        passwordSalt: row.get(3)?,
        policy: AccountPolicy {
            maxUploadBytesPerSecond: row.get(4)?,
            maxDownloadBytesPerSecond: row.get(5)?,
            maxConnections: row.get(6)?,
            maxOnlineIps: row.get(7)?,
            expiresAt: row.get(8)?,
        },
        policyRevision: row.get(9)?,
        remark: row.get(10)?,
        createdAt: row.get(11)?,
        updatedAt: row.get(12)?,
    })
}

/// 验证批量选择非空、数量有界、ID 唯一且修订号有效，防止重复账号在同一事务内被更新两次。
fn validateBatchSelections(selections: &[BatchAccountSelection]) -> Result<()> {
    if selections.is_empty() || selections.len() > maximumBatchAccounts {
        return Err(AccountServiceError::Validation(format!(
            "批量账号数量必须位于 1..={maximumBatchAccounts}"
        )));
    }
    let mut accountIds = HashSet::with_capacity(selections.len());
    if selections.iter().any(|selection| {
        selection.accountId.is_empty()
            || selection.policyRevision <= 0
            || !accountIds.insert(selection.accountId.as_str())
    }) {
        return Err(AccountServiceError::Validation(
            "批量账号 ID 必须唯一且修订号必须为正数".to_owned(),
        ));
    }
    Ok(())
}

/// 验证至少存在一个批量修改项，并复用账号策略的三态数值边界。
fn validateBatchMutation(request: &BatchUpdateAccountsRequest) -> Result<()> {
    let values = [
        request.maxOnlineIps,
        request.maxConnections,
        request.maxUploadBytesPerSecond,
        request.maxDownloadBytesPerSecond,
    ];
    if values.iter().flatten().any(|value| *value < -1) {
        return Err(AccountServiceError::Validation(
            "批量策略值只能为 -1、0 或正数".to_owned(),
        ));
    }
    if request.extendByMilliseconds.is_some_and(|value| value <= 0) {
        return Err(AccountServiceError::Validation(
            "批量加时必须为正毫秒数".to_owned(),
        ));
    }
    if values.iter().all(Option::is_none) && request.extendByMilliseconds.is_none() {
        return Err(AccountServiceError::Validation(
            "批量编辑至少选择一个修改项".to_owned(),
        ));
    }
    Ok(())
}

/// 将显式批量字段应用到一个账号；正数到期时间严格以原值为基线加时，-1/0 保持不变。
fn applyBatchPolicy(
    policy: &mut AccountPolicy,
    request: &BatchUpdateAccountsRequest,
) -> Result<bool> {
    let original = policy.clone();
    if let Some(value) = request.maxOnlineIps {
        policy.maxOnlineIps = value;
    }
    if let Some(value) = request.maxConnections {
        policy.maxConnections = value;
    }
    if let Some(value) = request.maxUploadBytesPerSecond {
        policy.maxUploadBytesPerSecond = value;
    }
    if let Some(value) = request.maxDownloadBytesPerSecond {
        policy.maxDownloadBytesPerSecond = value;
    }
    if let Some(extension) = request.extendByMilliseconds
        && policy.expiresAt > 0
    {
        policy.expiresAt = policy
            .expiresAt
            .checked_add(extension)
            .ok_or_else(|| AccountServiceError::Validation("加时结果超出时间戳范围".to_owned()))?;
    }
    Ok(*policy != original)
}

/// 写回已在事务内验证的策略记录；调用方负责修订递增、策略校验和审计。
fn updateStoredAccountPolicy(transaction: &Transaction<'_>, account: &StoredAccount) -> Result<()> {
    transaction.execute(
        "UPDATE accounts SET
            maxUploadBytesPerSecond = ?2, maxDownloadBytesPerSecond = ?3,
            maxConnections = ?4, maxOnlineIps = ?5, expiresAt = ?6,
            policyRevision = ?7, updatedAt = ?8
         WHERE accountId = ?1",
        params![
            account.accountId,
            account.policy.maxUploadBytesPerSecond,
            account.policy.maxDownloadBytesPerSecond,
            account.policy.maxConnections,
            account.policy.maxOnlineIps,
            account.policy.expiresAt,
            account.policyRevision,
            account.updatedAt,
        ],
    )?;
    Ok(())
}

/// 验证创建请求的协议字段，防止 SQLite 只按字符数而非 UTF-8 字节数接受超长值。
fn validateAccountRequest(request: &CreateAccountRequest) -> Result<()> {
    validateUsernameText("账号", &request.username)?;
    if let Some(password) = request.password.as_deref() {
        validateCredentialText("账号密码", password, maximumPasswordBytes)?;
    }
    request.policy.validate()?;
    validateRemark(request.remark.as_deref())
}

/// 校验同时用于 SOCKS5 RFC 1929 与 HTTP Basic 的账号名；冒号和控制字符无法在两条协议间无损共享。
fn validateUsernameText(name: &str, value: &str) -> Result<()> {
    validateCredentialText(name, value, maximumUsernameBytes)?;
    if !isSharedProtocolUsername(value) {
        return Err(AccountServiceError::Validation(format!(
            "{name}不能包含冒号或控制字符"
        )));
    }
    Ok(())
}

/// 判断账号名能否同时进入 RFC 1929 长度字段和 HTTP Basic 的首个冒号分隔字段。
fn isSharedProtocolUsername(value: &str) -> bool {
    !value.contains(':') && !value.chars().any(char::is_control)
}

/// 账号和密码都必须满足 RFC 1929 的非空单字节长度字段。
fn validateCredentialText(name: &str, value: &str, maximumBytes: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximumBytes {
        return Err(AccountServiceError::Validation(format!(
            "{name} 长度必须位于 1..={maximumBytes} 个 UTF-8 字节"
        )));
    }
    Ok(())
}

/// 限制管理备注的存储大小，备注为空字符串仍是用户明确内容而不是 NULL。
fn validateRemark(remark: Option<&str>) -> Result<()> {
    if remark.is_some_and(|value| value.len() > maximumRemarkBytes) {
        return Err(AccountServiceError::Validation(format!(
            "备注不能超过 {maximumRemarkBytes} 个 UTF-8 字节"
        )));
    }
    Ok(())
}

/// 聚合一条不含凭据的审计事实；所有字段只在所属业务事务提交前借用。
pub(crate) struct AuditEntry<'a> {
    pub occurredAt: i64,
    pub actorType: &'a str,
    pub action: &'a str,
    pub accountId: Option<&'a str>,
    pub result: &'a str,
    pub details: &'a str,
}

/// 写入不含凭据的审计事件；调用方必须在同一业务事务中调用，失败让业务事务整体回滚。
pub(crate) fn insertAudit(transaction: &Transaction<'_>, entry: AuditEntry<'_>) -> Result<()> {
    let AuditEntry {
        occurredAt,
        actorType,
        action,
        accountId,
        result,
        details,
    } = entry;
    transaction.execute(
        "INSERT INTO auditLogs (
            auditId, occurredAt, actorType, actorFingerprint, action,
            accountId, result, details
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            Uuid::new_v4().to_string(),
            occurredAt,
            actorType,
            actorType,
            action,
            accountId,
            result,
            details,
        ],
    )?;
    Ok(())
}

/// 只把 Key ID 的短前缀放入快照，避免界面误把它当作可调用凭据。
fn displayApiKeyPrefix(keyId: &str) -> String {
    format!("sak_v1_{keyId}_••••")
}

/// 唯一约束失败只用于用户名冲突；其它约束错误仍保留为数据库一致性故障。
pub(crate) fn isUniqueConstraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

/// 返回 UTC Unix 毫秒；系统时间早于纪元时明确归零而不是产生负溢出。
pub fn currentTimeMilliseconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}
