//! 在账号服务数据库内维护云路由规则集；所有启用切换和批量删除都使用 SQLite 原子事务。

use std::{collections::HashSet, net::IpAddr};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use uuid::Uuid;

use crate::{
    AccountServiceError, BatchDeleteRuleSetsRequest, CreateRuleSetRequest, Result, RuleSetView,
    SetRuleSetEnabledRequest, UpdateRuleSetRequest,
    store::{AccountStore, AuditEntry, currentTimeMilliseconds, insertAudit, isUniqueConstraint},
};

const maximumRuleSetNameBytes: usize = 128;
const maximumRuleSetContentBytes: usize = 1024 * 1024;
const maximumRuleSetLineBytes: usize = 8_192;
const maximumBatchRuleSets: usize = 500;
const defaultDnsSection: &str = "[DNS]\nPRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\n\n";

impl AccountStore {
    /// 按更新时间倒序读取全部规则集；列表正文用于管理端编辑，不参与客户端启用态选择。
    pub fn listRuleSets(&self) -> Result<Vec<RuleSetView>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT ruleSetId, name, content, enabled, revision, createdAt, updatedAt
             FROM ruleSets ORDER BY updatedAt DESC, ruleSetId ASC",
        )?;
        let rows = statement.query_map([], mapRuleSetRow)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// 创建规则集并按需原子替换当前启用项；正文只接受 routing.txt 文本，不读取服务端路径。
    pub fn createRuleSet(&self, request: &CreateRuleSetRequest) -> Result<RuleSetView> {
        let name = validatedRuleSetName(&request.name)?;
        let content = validatedRuleSetContent(&request.content)?;
        let ruleSetId = Uuid::new_v4().to_string();
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        if request.enabled {
            disableActiveRuleSet(&transaction, now, Some(&ruleSetId))?;
        }
        let insertResult = transaction.execute(
            "INSERT INTO ruleSets (
                ruleSetId, name, content, enabled, revision, createdAt, updatedAt
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)",
            params![ruleSetId, name, content, request.enabled, now],
        );
        if let Err(error) = insertResult {
            return if isUniqueConstraint(&error) {
                Err(AccountServiceError::RuleSetConflict)
            } else {
                Err(error.into())
            };
        }
        let auditDetails = format!("{{\"enabled\":{}}}", request.enabled);
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "ruleSet.create",
                accountId: Some(&ruleSetId),
                result: "success",
                details: &auditDetails,
            },
        )?;
        transaction.commit()?;
        drop(connection);
        self.ruleSetById(&ruleSetId)
    }

    /// 返回单个规则集及其当前修订，不存在时映射为稳定的规则集错误。
    pub fn ruleSetById(&self, ruleSetId: &str) -> Result<RuleSetView> {
        let connection = self.connection.lock();
        queryRuleSet(&connection, ruleSetId)?.ok_or(AccountServiceError::RuleSetNotFound)
    }

    /// 保存名称和完整正文并递增修订号；启用状态只能通过互斥开关端点改变。
    pub fn updateRuleSet(
        &self,
        ruleSetId: &str,
        request: &UpdateRuleSetRequest,
    ) -> Result<RuleSetView> {
        let name = validatedRuleSetName(&request.name)?;
        let content = validatedRuleSetContent(&request.content)?;
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        requireRuleSetRevision(&transaction, ruleSetId, request.revision)?;
        let updateResult = transaction.execute(
            "UPDATE ruleSets SET name = ?2, content = ?3,
                revision = revision + 1, updatedAt = ?4 WHERE ruleSetId = ?1",
            params![ruleSetId, name, content, now],
        );
        if let Err(error) = updateResult {
            return if isUniqueConstraint(&error) {
                Err(AccountServiceError::RuleSetConflict)
            } else {
                Err(error.into())
            };
        }
        let auditDetails = format!("{{\"revision\":{}}}", request.revision.saturating_add(1));
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "ruleSet.update",
                accountId: Some(ruleSetId),
                result: "success",
                details: &auditDetails,
            },
        )?;
        transaction.commit()?;
        drop(connection);
        self.ruleSetById(ruleSetId)
    }

    /// 切换规则集启用状态；开启时先在同一事务关闭其它记录，数据库唯一索引再兜住单启用不变量。
    pub fn setRuleSetEnabled(
        &self,
        ruleSetId: &str,
        request: &SetRuleSetEnabledRequest,
    ) -> Result<RuleSetView> {
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let current =
            queryRuleSet(&transaction, ruleSetId)?.ok_or(AccountServiceError::RuleSetNotFound)?;
        if current.revision != request.revision {
            return Err(AccountServiceError::RuleSetRevisionConflict {
                currentRevision: current.revision,
            });
        }
        if current.enabled == request.enabled {
            transaction.commit()?;
            return Ok(current);
        }
        if request.enabled {
            disableActiveRuleSet(&transaction, now, Some(ruleSetId))?;
        }
        transaction.execute(
            "UPDATE ruleSets SET enabled = ?2, revision = revision + 1, updatedAt = ?3
             WHERE ruleSetId = ?1",
            params![ruleSetId, request.enabled, now],
        )?;
        let auditDetails = format!("{{\"enabled\":{}}}", request.enabled);
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "ruleSet.enable",
                accountId: Some(ruleSetId),
                result: "success",
                details: &auditDetails,
            },
        )?;
        transaction.commit()?;
        drop(connection);
        self.ruleSetById(ruleSetId)
    }

    /// 删除单个规则集；删除启用项后保持无启用状态，由管理员显式选择后继规则。
    pub fn deleteRuleSet(&self, ruleSetId: &str) -> Result<()> {
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        if transaction.execute("DELETE FROM ruleSets WHERE ruleSetId = ?1", [ruleSetId])? == 0 {
            return Err(AccountServiceError::RuleSetNotFound);
        }
        insertAudit(
            &transaction,
            AuditEntry {
                occurredAt: now,
                actorType: "management",
                action: "ruleSet.delete",
                accountId: Some(ruleSetId),
                result: "success",
                details: "{}",
            },
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 在任何删除发生前校验整批 ID，保证多选删除不会留下半完成结果。
    pub fn deleteRuleSetsBatch(&self, request: &BatchDeleteRuleSetsRequest) -> Result<usize> {
        validateRuleSetIds(&request.ruleSetIds)?;
        let now = currentTimeMilliseconds();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        for ruleSetId in &request.ruleSetIds {
            if queryRuleSet(&transaction, ruleSetId)?.is_none() {
                return Err(AccountServiceError::RuleSetNotFound);
            }
        }
        for ruleSetId in &request.ruleSetIds {
            transaction.execute("DELETE FROM ruleSets WHERE ruleSetId = ?1", [ruleSetId])?;
            insertAudit(
                &transaction,
                AuditEntry {
                    occurredAt: now,
                    actorType: "management",
                    action: "ruleSet.batchDelete",
                    accountId: Some(ruleSetId),
                    result: "success",
                    details: "{}",
                },
            )?;
        }
        transaction.commit()?;
        Ok(request.ruleSetIds.len())
    }

    /// 返回唯一启用规则集；不存在时客户端下载端点返回 404，禁止静默下发空配置。
    pub fn activeRuleSet(&self) -> Result<RuleSetView> {
        let connection = self.connection.lock();
        connection
            .query_row(
                "SELECT ruleSetId, name, content, enabled, revision, createdAt, updatedAt
                 FROM ruleSets WHERE enabled = 1",
                [],
                mapRuleSetRow,
            )
            .optional()?
            .ok_or(AccountServiceError::RuleSetNotFound)
    }
}

/// 读取规则集固定列顺序；所有查询共用映射，避免字段演进后列表和单项响应发生错位。
fn mapRuleSetRow(row: &rusqlite::Row<'_>) -> rusqlite::Result<RuleSetView> {
    Ok(RuleSetView {
        ruleSetId: row.get(0)?,
        name: row.get(1)?,
        content: row.get(2)?,
        enabled: row.get(3)?,
        revision: row.get(4)?,
        createdAt: row.get(5)?,
        updatedAt: row.get(6)?,
    })
}

/// 按稳定 ID 查询规则集；Connection 与 Transaction 都能通过解引用复用该只读边界。
fn queryRuleSet(connection: &Connection, ruleSetId: &str) -> Result<Option<RuleSetView>> {
    connection
        .query_row(
            "SELECT ruleSetId, name, content, enabled, revision, createdAt, updatedAt
             FROM ruleSets WHERE ruleSetId = ?1",
            [ruleSetId],
            mapRuleSetRow,
        )
        .optional()
        .map_err(Into::into)
}

/// 校验乐观锁修订号并返回当前值；调用方必须处于写事务中，避免校验与更新之间被插入修改。
fn requireRuleSetRevision(
    transaction: &Transaction<'_>,
    ruleSetId: &str,
    expectedRevision: i64,
) -> Result<i64> {
    let currentRevision = transaction
        .query_row(
            "SELECT revision FROM ruleSets WHERE ruleSetId = ?1",
            [ruleSetId],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(AccountServiceError::RuleSetNotFound)?;
    if currentRevision != expectedRevision {
        return Err(AccountServiceError::RuleSetRevisionConflict { currentRevision });
    }
    Ok(currentRevision)
}

/// 关闭当前启用规则并推进其修订号，使已缓存 ETag 在互斥切换后必然失效。
fn disableActiveRuleSet(
    transaction: &Transaction<'_>,
    now: i64,
    excludedRuleSetId: Option<&str>,
) -> Result<()> {
    transaction.execute(
        "UPDATE ruleSets SET enabled = 0, revision = revision + 1, updatedAt = ?1
         WHERE enabled = 1 AND (?2 IS NULL OR ruleSetId <> ?2)",
        params![now, excludedRuleSetId],
    )?;
    Ok(())
}

/// 规范规则集名称并限制 UTF-8 字节长度；前后空白不参与唯一性，避免视觉相同的重复项。
fn validatedRuleSetName(name: &str) -> Result<String> {
    let normalized = name.trim();
    if normalized.is_empty() || normalized.len() > maximumRuleSetNameBytes {
        return Err(AccountServiceError::Validation(format!(
            "规则集名称必须为 1..={maximumRuleSetNameBytes} 个 UTF-8 字节"
        )));
    }
    Ok(normalized.to_owned())
}

/// 校验并规范 routing.txt 正文；同时检查客户端解析器依赖的 DNS、路由和应用段。
///
/// 运行上下文：管理页面提交后、SQLite 事务开始前调用；`content` 是完整原始文本。
/// 换行统一为 LF，保证同一正文生成稳定 ETag 和下载字节。失败时返回 Validation，
/// 拒绝空文本、NUL、超限、缺段和客户端不支持的语法，不产生部分规范化结果。
fn validatedRuleSetContent(content: &str) -> Result<String> {
    if content.is_empty() || content.len() > maximumRuleSetContentBytes || content.contains('\0') {
        return Err(AccountServiceError::Validation(format!(
            "规则集正文必须为 1..={maximumRuleSetContentBytes} 个 UTF-8 字节且不能包含 NUL"
        )));
    }
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    validateRoutingSections(&normalized)?;
    Ok(format!("{}\n", normalized.trim_end()))
}

/// 把第二版数据库的旧 DNS 与 `[proxy app]` 段转换为当前协议。
///
/// 运行上下文：SQLite v2→v3 排他迁移事务逐条调用；`content` 是数据库内的原始正文。
/// 修复理由：旧服务允许不含 `[DNS]` 的规则，新客户端会拒绝该正文，因此必须在启动前
/// 原子补齐显式直连上游并统一段名。返回 `Some` 表示需写回，`None` 表示已经是当前格式；
/// 任一正文无法通过同一校验器时返回 Validation，调用方回滚整个迁移。
pub(crate) fn migrateLegacyRuleSetContent(content: &str) -> Result<Option<String>> {
    let mut changed = false;
    let normalizedSections = content
        .lines()
        .map(|sourceLine| {
            if normalizedRuleLine(sourceLine).eq_ignore_ascii_case("[proxy app]") {
                changed = true;
                "[proxy_app]".to_owned()
            } else {
                sourceLine.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let hasDnsSection = normalizedSections
        .lines()
        .map(normalizedRuleLine)
        .any(|line| line.eq_ignore_ascii_case("[DNS]"));
    if hasDnsSection && !changed {
        validatedRuleSetContent(content)?;
        return Ok(None);
    }
    let migratedContent = if hasDnsSection {
        normalizedSections
    } else {
        format!("{defaultDnsSection}{normalizedSections}")
    };
    validatedRuleSetContent(&migratedContent).map(Some)
}

/// 按客户端解析器的规则类型、动作集合和 DNS 协议执行语法检查。
///
/// 运行上下文：保存之前一次遍历 `content` 完整正文，单行上限按 UTF-8 字节数计算。
/// 修复理由：DNS 必须在下发前形成确定的直连上游，因此拒绝缺失、重复、未知键和非 IP 值，
/// 避免终端再隐式回退到系统 DNS。失败返回带行号的 Validation，数据库不产生写入。
fn validateRoutingSections(content: &str) -> Result<()> {
    let mut currentSection = "";
    let mut sections = HashSet::new();
    let mut dnsKeys = HashSet::new();
    let mut proxyPackages = HashSet::new();
    let mut hasPrimaryDns = false;
    let mut hasRoutingRules = false;
    let mut hasGlobalRules = false;
    let mut hasProxyPackages = false;
    let mut routingFinalSeen = false;
    let mut globalFinalSeen = false;
    for (index, sourceLine) in content.lines().enumerate() {
        if sourceLine.len() > maximumRuleSetLineBytes {
            return Err(AccountServiceError::Validation(format!(
                "规则集第 {} 行超过 {maximumRuleSetLineBytes} 个 UTF-8 字节",
                index + 1
            )));
        }
        let line = normalizedRuleLine(sourceLine);
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            currentSection = line;
            let normalizedSection = line.to_ascii_lowercase();
            let recognizedSection = matches!(
                normalizedSection.as_str(),
                "[dns]" | "[routingrule]" | "[groutingrule]" | "[proxy_app]"
            );
            if !recognizedSection {
                return Err(AccountServiceError::Validation(format!(
                    "规则集第 {} 行包含未知段 {line}",
                    index + 1
                )));
            }
            if !sections.insert(normalizedSection.clone()) {
                return Err(AccountServiceError::Validation(format!(
                    "规则集第 {} 行重复声明段 {line}",
                    index + 1,
                )));
            }
            continue;
        }
        if currentSection.eq_ignore_ascii_case("[DNS]") {
            if validateDnsLine(line, index + 1, &mut dnsKeys)? {
                hasPrimaryDns = true;
            }
        } else if currentSection.eq_ignore_ascii_case("[RoutingRule]")
            || currentSection.eq_ignore_ascii_case("[GRoutingRule]")
        {
            let isGlobalRule = currentSection.eq_ignore_ascii_case("[GRoutingRule]");
            let finalSeen = if isGlobalRule {
                &mut globalFinalSeen
            } else {
                &mut routingFinalSeen
            };
            // FINAL 是当前作用域的终止规则；继续接受后续规则会把不可达配置保存并下发，
            // 三端还可能因遍历策略差异产生不同结果，因此在服务端权威写入边界直接拒绝。
            if *finalSeen {
                return Err(AccountServiceError::Validation(format!(
                    "规则集第 {} 行位于当前规则段 FINAL 之后",
                    index + 1
                )));
            }
            *finalSeen = validateRoutingRuleLine(line, index + 1)?;
            if isGlobalRule {
                hasGlobalRules = true;
            } else {
                hasRoutingRules = true;
            }
        } else if currentSection.eq_ignore_ascii_case("[proxy_app]") {
            if line.contains(',') || !isAndroidPackageName(line) {
                return Err(AccountServiceError::Validation(format!(
                    "规则集第 {} 行必须是单个有效 Android 包名",
                    index + 1
                )));
            }
            if !proxyPackages.insert(line.to_owned()) {
                return Err(AccountServiceError::Validation(format!(
                    "规则集第 {} 行重复声明代理应用 {line}",
                    index + 1
                )));
            }
            hasProxyPackages = true;
        } else {
            // 固定协议只允许四个已知段；段外文本若被静默忽略，管理页会显示已保存，
            // 终端却完全不执行，形成不可观测的配置漂移。
            return Err(AccountServiceError::Validation(format!(
                "规则集第 {} 行不属于任何已知段",
                index + 1
            )));
        }
    }
    for required in ["[dns]", "[routingrule]", "[groutingrule]", "[proxy_app]"] {
        if !sections.contains(required) {
            return Err(AccountServiceError::Validation(format!(
                "规则集缺少必需段 {required}"
            )));
        }
    }
    if !hasPrimaryDns {
        return Err(AccountServiceError::Validation(
            "规则集 [DNS] 段必须包含 PRIMARY,<IP>".to_owned(),
        ));
    }
    if !hasGlobalRules && !hasRoutingRules {
        return Err(AccountServiceError::Validation(
            "规则集必须至少配置一条 [RoutingRule] 或 [GRoutingRule]".to_owned(),
        ));
    }
    if hasRoutingRules != hasProxyPackages {
        return Err(AccountServiceError::Validation(
            "[RoutingRule] 与 [proxy_app] 必须同时配置；[GRoutingRule] 可与它们混合".to_owned(),
        ));
    }
    Ok(())
}

/// 移除单行注释、UTF-8 BOM 和边界空白，供段检测与语法校验共用同一视图。
///
/// 运行上下文：参数 `sourceLine` 是从 routing.txt 切分的原始行，返回值借用其内存。
/// 失败语义：纯字符串规范化不产生错误；全注释或全空白行返回空字符串。
fn normalizedRuleLine(sourceLine: &str) -> &str {
    sourceLine
        .split('#')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches('\u{feff}')
        .trim()
}

/// 校验 DNS 段的单条上游；键不区分大小写，但每个角色只允许声明一次。
///
/// 运行上下文：规则段顺序遍历时调用，`dnsKeys` 跨整个 [DNS] 段保留已见角色。
/// 参数：`line` 是去除注释和空白后的 CSV，`lineNumber` 用于稳定诊断。
/// 失败语义：字段数、键、重复或 IPv4/IPv6 值无效时返回 Validation；成功返回该行是否为 PRIMARY。
fn validateDnsLine(line: &str, lineNumber: usize, dnsKeys: &mut HashSet<String>) -> Result<bool> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    if fields.len() != 2 {
        return Err(AccountServiceError::Validation(format!(
            "规则集第 {lineNumber} 行 DNS 配置必须为 PRIMARY,<IP> 或 SECONDARY,<IP>"
        )));
    }
    let key = fields[0].to_ascii_uppercase();
    if !matches!(key.as_str(), "PRIMARY" | "SECONDARY") {
        return Err(AccountServiceError::Validation(format!(
            "规则集第 {lineNumber} 行包含未知 DNS 键 {}",
            fields[0]
        )));
    }
    if !dnsKeys.insert(key.clone()) {
        return Err(AccountServiceError::Validation(format!(
            "规则集第 {lineNumber} 行重复声明 DNS 键 {key}"
        )));
    }
    if fields[1].parse::<IpAddr>().is_err() {
        return Err(AccountServiceError::Validation(format!(
            "规则集第 {lineNumber} 行 DNS 地址必须是有效 IPv4 或 IPv6"
        )));
    }
    Ok(key == "PRIMARY")
}

/// 按 applicationId 约束包名：至少两段、每段字母起始且只允许 ASCII 字母数字下划线。
fn isAndroidPackageName(packageName: &str) -> bool {
    let segments = packageName.split('.').collect::<Vec<_>>();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphabetic())
                && segment
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
}

/// 校验单条路由规则的 CSV 结构并返回它是否为终止规则。
///
/// 运行上下文：保存正文时按原始显示顺序调用，调用方用返回值禁止同段出现第二个 FINAL
/// 或 FINAL 后的不可达规则。字段数、类型、取值或动作无效时返回带行号的 Validation。
fn validateRoutingRuleLine(line: &str, lineNumber: usize) -> Result<bool> {
    let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
    let ruleType = fields.first().copied().unwrap_or_default();
    let valid = if ruleType.eq_ignore_ascii_case("FINAL") {
        fields.len() == 2 && isRuleAction(fields[1])
    } else {
        fields.len() == 3
            && matches!(
                ruleType.to_ascii_uppercase().as_str(),
                "PORT" | "IP-CIDR" | "DOMAIN" | "DOMAIN-KEYWORD"
            )
            && !fields[1].is_empty()
            && isRuleAction(fields[2])
    };
    if !valid {
        return Err(AccountServiceError::Validation(format!(
            "规则集第 {lineNumber} 行的类型、字段数或动作无效"
        )));
    }
    if ruleType.eq_ignore_ascii_case("PORT") {
        validateRulePort(fields[1], lineNumber)?;
    } else if ruleType.eq_ignore_ascii_case("IP-CIDR") {
        validateRuleCidr(fields[1], lineNumber)?;
    }
    Ok(ruleType.eq_ignore_ascii_case("FINAL"))
}

/// 校验单端口或闭区间，与客户端一致拒绝零端口、逆序区间、非数字和超过 65535 的值。
fn validateRulePort(value: &str, lineNumber: usize) -> Result<()> {
    let boundaries = value
        .splitn(2, '-')
        .map(|boundary| boundary.trim().parse::<u16>().ok())
        .collect::<Vec<_>>();
    let start = boundaries.first().copied().flatten();
    let end = boundaries.last().copied().flatten();
    if start.is_none() || end.is_none() || start == Some(0) || end < start {
        return Err(AccountServiceError::Validation(format!(
            "规则集第 {lineNumber} 行端口或区间无效"
        )));
    }
    Ok(())
}

/// 校验 IPv4 CIDR；省略前缀按 `/32`，地址或前缀无效时在服务端保存边界直接拒绝。
fn validateRuleCidr(value: &str, lineNumber: usize) -> Result<()> {
    let mut parts = value.splitn(2, '/');
    let addressValid = parts
        .next()
        .is_some_and(|address| address.parse::<std::net::Ipv4Addr>().is_ok());
    let prefixValid = parts
        .next()
        .map(|prefix| prefix.parse::<u8>().is_ok_and(|value| value <= 32))
        .unwrap_or(true);
    if !addressValid || !prefixValid {
        return Err(AccountServiceError::Validation(format!(
            "规则集第 {lineNumber} 行 IPv4 CIDR 无效"
        )));
    }
    Ok(())
}

/// 规则动作与客户端解析器保持同一闭集，防止未知动作到达终端后才导致数据面启动失败。
fn isRuleAction(action: &str) -> bool {
    matches!(
        action.to_ascii_uppercase().as_str(),
        "PROXY" | "DIRECT" | "REJECT"
    )
}

/// 校验批量删除 ID 的数量、空值和重复项；失败时数据库尚未发生任何写入。
fn validateRuleSetIds(ruleSetIds: &[String]) -> Result<()> {
    if ruleSetIds.is_empty() || ruleSetIds.len() > maximumBatchRuleSets {
        return Err(AccountServiceError::Validation(format!(
            "规则集选择数量必须位于 1..={maximumBatchRuleSets}"
        )));
    }
    let mut uniqueIds = HashSet::with_capacity(ruleSetIds.len());
    if ruleSetIds
        .iter()
        .any(|ruleSetId| ruleSetId.is_empty() || !uniqueIds.insert(ruleSetId))
    {
        return Err(AccountServiceError::Validation(
            "规则集 ID 不能为空或重复".to_owned(),
        ));
    }
    Ok(())
}
