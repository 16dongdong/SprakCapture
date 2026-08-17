#![allow(non_snake_case)]

use account_service::{AccountPolicy, AccountServiceError, AccountStore, CreateAccountRequest};
use rusqlite::{Connection, params};
use tempfile::TempDir;

/// 构造迁移测试使用的无限制账号策略，避免数据库升级断言混入策略边界。
///
/// 运行上下文：仅用于创建第一版数据库中的存量账号。
/// 失败语义：本函数不执行 I/O，固定字段发生变化会由账号保留断言直接暴露。
fn unlimitedPolicy() -> AccountPolicy {
    AccountPolicy {
        maxUploadBytesPerSecond: -1,
        maxDownloadBytesPerSecond: -1,
        maxConnections: -1,
        maxOnlineIps: -1,
        expiresAt: -1,
    }
}

/// 构造当前协议的混合范围规则，供“已是最新正文”迁移分支验证字节保持。
///
/// 运行上下文：普通段仅作用于 `[proxy_app]`，全局段仅作用于其余应用。
/// 失败语义：返回值是确定的有效文本，被迁移器改写即表示幂等性失效。
fn validRoutingContent(finalAction: &str) -> String {
    format!(
        "[DNS]\nPRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\n\n[RoutingRule]\nDOMAIN,selected.example,PROXY\nFINAL,{finalAction}\n\n[GRoutingRule]\nDOMAIN,global.example,PROXY\nFINAL,{finalAction}\n\n[proxy_app]\ncom.example.client\n"
    )
}

/// 验证既有第一版账号数据库会连续升级到当前版本且保留账号。
///
/// 运行上下文：先用当前代码建库，再精确移除后续结构和记录来模拟真实 v1 存量文件。
/// 失败语义：账号丢失、规则表未创建或版本未到当前版本都表示链式迁移不完整。
#[test]
fn schemaOneDatabaseMigratesToRuleSetsWithoutLosingAccounts() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("legacy.db");
    {
        let store = AccountStore::open(&databasePath).expect("创建当前数据库");
        store
            .createAccount(&CreateAccountRequest {
                username: "legacy-account".to_owned(),
                password: None,
                policy: unlimitedPolicy(),
                remark: None,
            })
            .expect("写入既有账号");
    }
    {
        let connection = Connection::open(&databasePath).expect("打开迁移夹具");
        connection
            .execute_batch(
                "DROP INDEX ruleSetsSingleEnabledIndex;
                 DROP INDEX ruleSetsUpdatedAtIndex;
                 DROP TABLE ruleSets;
                 DELETE FROM schemaMigrations WHERE version >= 2;",
            )
            .expect("还原第一版结构");
    }

    let migrated = AccountStore::open(&databasePath).expect("迁移第一版数据库");
    assert_eq!(migrated.currentSchemaVersion().expect("读取迁移版本"), 4);
    assert!(
        migrated
            .accountByUsername("legacy-account")
            .expect("读取既有账号")
            .is_some()
    );
    assert!(migrated.listRuleSets().expect("读取新规则集表").is_empty());
}

/// 验证 v2→v3 补齐 DNS 并把旧 `[proxy app]` 段迁移为 `[proxy_app]`。
///
/// 运行上下文：直接在 SQLite 中构造当时 v2 允许的旧正文和已提前配置 DNS 的正文，再重开存储。
/// 失败语义：未前置默认 DNS、ETag 依赖的 revision/updatedAt 未推进，或已有 DNS 被改写都视为迁移失败。
#[test]
fn schemaTwoMigrationAddsDnsAndPreservesCurrentRules() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("rules-v2.db");
    AccountStore::open(&databasePath).expect("创建当前数据库");
    let legacyContent = "[RoutingRule]\nDOMAIN,legacy.example,PROXY\n[GRoutingRule]\nFINAL,PROXY\n[proxy app]\ncom.example.legacy\n";
    let currentContent = validRoutingContent("PROXY");
    {
        let connection = Connection::open(&databasePath).expect("打开 v2 迁移夹具");
        connection
            .execute("DELETE FROM schemaMigrations WHERE version >= 3", [])
            .expect("降级版本记录");
        connection
            .execute(
                "INSERT INTO ruleSets (
                    ruleSetId, name, content, enabled, revision, createdAt, updatedAt
                 ) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
                params![
                    "legacy-rule",
                    "旧规则",
                    legacyContent,
                    7_i64,
                    10_i64,
                    10_i64
                ],
            )
            .expect("写入缺少 DNS 的 v2 规则");
        connection
            .execute(
                "INSERT INTO ruleSets (
                    ruleSetId, name, content, enabled, revision, createdAt, updatedAt
                 ) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)",
                params![
                    "current-rule",
                    "当前规则",
                    currentContent,
                    4_i64,
                    20_i64,
                    20_i64
                ],
            )
            .expect("写入已含 DNS 的 v2 规则");
    }

    let migrated = AccountStore::open(&databasePath).expect("执行 v2 到 v3 迁移");
    assert_eq!(migrated.currentSchemaVersion().expect("读取迁移版本"), 4);
    let legacyRule = migrated.ruleSetById("legacy-rule").expect("读取迁移规则");
    assert_eq!(
        legacyRule.content,
        "[DNS]\nPRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\n\n[RoutingRule]\nDOMAIN,legacy.example,PROXY\n[GRoutingRule]\nFINAL,PROXY\n[proxy_app]\ncom.example.legacy\n"
    );
    assert_eq!(legacyRule.revision, 8);
    assert!(legacyRule.updatedAt > 10);
    let currentRule = migrated.ruleSetById("current-rule").expect("读取当前规则");
    assert_eq!(currentRule.content, currentContent);
    assert_eq!(currentRule.revision, 4);
    assert_eq!(currentRule.updatedAt, 20);
}

/// 验证 v2 中存在无法升级的规则时整个 v3 迁移回滚，服务不会带坏规则启动。
///
/// 运行上下文：迁移夹具包含未知动作，补齐 DNS 后仍必须被同一 validator 拒绝。
/// 失败语义：若存储打开成功、版本推进、正文被部分改写或 revision 变化，则迁移不具备原子失败语义。
#[test]
fn schemaTwoMigrationRejectsInvalidRuleAtomically() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("invalid-rules-v2.db");
    AccountStore::open(&databasePath).expect("创建当前数据库");
    let validLegacyContent = "[RoutingRule]\n[GRoutingRule]\nFINAL,PROXY\n[proxy app]\n";
    let invalidContent = "[RoutingRule]\nFINAL,UNKNOWN\n[GRoutingRule]\nFINAL,PROXY\n[proxy app]\n";
    {
        let connection = Connection::open(&databasePath).expect("打开 v2 失败夹具");
        connection
            .execute("DELETE FROM schemaMigrations WHERE version >= 3", [])
            .expect("降级版本记录");
        connection
            .execute(
                "INSERT INTO ruleSets (
                    ruleSetId, name, content, enabled, revision, createdAt, updatedAt
                 ) VALUES ('a-valid-rule', '先迁移规则', ?1, 0, 2, 25, 25)",
                [validLegacyContent],
            )
            .expect("写入排序在前的有效 v2 规则");
        connection
            .execute(
                "INSERT INTO ruleSets (
                    ruleSetId, name, content, enabled, revision, createdAt, updatedAt
                 ) VALUES ('z-invalid-rule', '无效规则', ?1, 1, 5, 30, 30)",
                [invalidContent],
            )
            .expect("写入无效 v2 规则");
    }

    let openError = match AccountStore::open(&databasePath) {
        Ok(_) => panic!("历史坏账号必须阻止迁移"),
        Err(error) => error,
    };
    assert!(matches!(openError, AccountServiceError::Validation(_)));
    assert!(
        !openError.to_string().contains("name:part"),
        "迁移诊断不得泄露原始账号标识"
    );
    let connection = Connection::open(&databasePath).expect("重新检查回滚数据库");
    let maximumVersion: i64 = connection
        .query_row("SELECT MAX(version) FROM schemaMigrations", [], |row| {
            row.get(0)
        })
        .expect("读取回滚版本");
    assert_eq!(maximumVersion, 2);
    let (storedContent, revision): (String, i64) = connection
        .query_row(
            "SELECT content, revision FROM ruleSets WHERE ruleSetId = 'z-invalid-rule'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("读取回滚规则");
    assert_eq!(storedContent, invalidContent);
    assert_eq!(revision, 5);
    let (validStoredContent, validRevision): (String, i64) = connection
        .query_row(
            "SELECT content, revision FROM ruleSets WHERE ruleSetId = 'a-valid-rule'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("读取已回滚的先行规则");
    assert_eq!(validStoredContent, validLegacyContent);
    assert_eq!(validRevision, 2);
}

/// 验证共享认证账号名在创建和历史迁移两条入口使用同一冒号/控制字符约束。
///
/// 运行上下文：先走公开存储 API 验证新账号，再把当前数据库降为 v3 并直接注入历史坏账号。
/// 失败语义：新账号被接受或历史数据库推进到 v4，都会导致 SOCKS5 成功但 HTTP Basic 永久失败。
#[test]
fn sharedProtocolUsernameRejectsAmbiguousAccountNames() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("username-v3.db");
    let store = AccountStore::open(&databasePath).expect("创建当前数据库");
    for username in ["name:part", "name\npart"] {
        let result = store.createAccount(&CreateAccountRequest {
            username: username.to_owned(),
            password: Some("password:with:colon".to_owned()),
            policy: unlimitedPolicy(),
            remark: None,
        });
        assert!(matches!(result, Err(AccountServiceError::Validation(_))));
    }
    drop(store);

    {
        let connection = Connection::open(&databasePath).expect("打开 v3 账号夹具");
        connection
            .execute("DELETE FROM schemaMigrations WHERE version = 4", [])
            .expect("降级到 v3");
        connection
            .execute(
                "INSERT INTO accounts (
                    accountId, username, passwordHash, passwordSalt,
                    maxUploadBytesPerSecond, maxDownloadBytesPerSecond,
                    maxConnections, maxOnlineIps, expiresAt, policyRevision,
                    remark, createdAt, updatedAt
                 ) VALUES ('ambiguous', 'name:part', NULL, NULL, -1, -1, -1, -1, -1, 1, NULL, 1, 1)",
                [],
            )
            .expect("写入历史坏账号");
    }

    let openResult = AccountStore::open(&databasePath);
    assert!(matches!(
        openResult,
        Err(AccountServiceError::Validation(_))
    ));
    let connection = Connection::open(&databasePath).expect("复查迁移回滚");
    let maximumVersion: i64 = connection
        .query_row("SELECT MAX(version) FROM schemaMigrations", [], |row| {
            row.get(0)
        })
        .expect("读取回滚版本");
    assert_eq!(maximumVersion, 3);
}
