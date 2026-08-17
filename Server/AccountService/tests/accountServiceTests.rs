#![allow(non_snake_case)]

use std::{
    net::SocketAddr,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use account_service::{
    AccountDomainService, AccountPolicy, AccountServerConfig, AccountServiceError, AccountStore,
    BatchAccountSelection, BatchDeleteAccountsRequest, BatchDeleteRuleSetsRequest,
    BatchUpdateAccountsRequest, CreateAccountRequest, CreateRuleSetRequest,
    LeaseAuthenticationRequest, LeaseProgress, LeaseSynchronizationRequest,
    SetRuleSetEnabledRequest, UpdateAccountRequest, UpdateRuleSetRequest, startAccountService,
};
use axum::{
    Json, Router,
    routing::{get, post},
};
use reqwest::{Client, StatusCode, header};
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;

/// 返回与服务端一致的 Unix 毫秒时间，仅用于构造临近到期的测试策略。
fn currentTimeMilliseconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间必须晚于 Unix 纪元")
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// 构造不限制流量和连接的基线策略，单项测试只覆盖自己关心的边界。
fn unlimitedPolicy() -> AccountPolicy {
    AccountPolicy {
        maxUploadBytesPerSecond: -1,
        maxDownloadBytesPerSecond: -1,
        maxConnections: -1,
        maxOnlineIps: -1,
        expiresAt: -1,
    }
}

/// 构造覆盖指定 DNS、普通、全局与按应用段的最小可下发 routing.txt。
///
/// 运行上下文：规则集领域和 HTTP 集成测试共用，参数限定普通段的最终动作。
/// 失败语义：返回值是确定的有效文本，如果服务端拒绝它则表示协议校验已漂移。
fn validRoutingContent(finalAction: &str) -> String {
    format!(
        "[DNS]\nPRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\n\n[RoutingRule]\nDOMAIN,selected.example,PROXY\nFINAL,{finalAction}\n\n[GRoutingRule]\nDOMAIN,global.example,PROXY\nFINAL,{finalAction}\n\n[proxy_app]\ncom.example.client\n"
    )
}

/// 验证远程监听只暴露一个带认证的 Sprak Capture Web 入口，并把已授权控制 API 转发到回环服务。
///
/// 运行上下文：使用临时 Web 目录和回环控制夹具启动真实双监听服务；未登录请求不得抵达控制夹具。
/// 失败语义：静态入口缺失、未授权控制请求被放行、登录 Cookie 不持久或转发响应改变均表示统一入口边界失效。
#[tokio::test]
async fn remoteWebEntryAuthenticatesAndForwardsControlApi() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let webAssetsDirectory = temporaryDirectory.path().join("web");
    std::fs::create_dir(&webAssetsDirectory).expect("创建 Web 资源目录");
    std::fs::write(
        webAssetsDirectory.join("index.html"),
        "<!doctype html><title>remote-workspace</title>",
    )
    .expect("写入 Web 入口");
    let controlListener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定控制夹具");
    let controlAddress = controlListener.local_addr().expect("读取控制夹具地址");
    let controlTask = tokio::spawn(async move {
        axum::serve(
            controlListener,
            Router::new()
                .route(
                    "/api/v1/health",
                    get(|| async { Json(json!({ "status": "ok" })) }),
                )
                .route(
                    "/api/v1/clientPackages/download",
                    post(|| async {
                        (
                            [(
                                header::CONTENT_TYPE,
                                "application/vnd.android.package-archive",
                            )],
                            "apk-fixture",
                        )
                    }),
                ),
        )
        .await
        .expect("运行控制夹具");
    });
    let running = startAccountService(AccountServerConfig {
        databasePath: temporaryDirectory.path().join("accounts.db"),
        publicAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalToken: "remote-entry-internal-token-at-least-32-bytes".to_owned(),
        controlBaseUrl: format!("http://{controlAddress}"),
        webAssetsDirectory: Some(webAssetsDirectory),
    })
    .await
    .expect("启动远程 Web 入口");
    let baseUrl = format!("http://{}", running.publicAddress);
    let client = Client::new();
    let workspace = client.get(&baseUrl).send().await.expect("读取远程工作台");
    assert_eq!(workspace.status(), StatusCode::OK);
    assert!(
        workspace
            .text()
            .await
            .expect("读取工作台正文")
            .contains("remote-workspace")
    );
    let unauthorized = client
        .get(format!("{baseUrl}/api/v1/health"))
        .send()
        .await
        .expect("访问未授权控制接口");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let login = client
        .post(format!("{baseUrl}/api/v1/auth/login"))
        .json(&json!({ "username": "Admin", "password": "Admin123" }))
        .send()
        .await
        .expect("登录远程工作台");
    let cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .expect("读取登录 Cookie")
        .split(';')
        .next()
        .expect("读取 Cookie 首段")
        .to_owned();
    let health: serde_json::Value = client
        .get(format!("{baseUrl}/api/v1/health"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .expect("访问已授权控制接口")
        .json()
        .await
        .expect("读取控制响应");
    assert_eq!(health, json!({ "status": "ok" }));
    let publicDownload = client
        .post(format!("{baseUrl}/api/v1/clientPackages/download"))
        .json(&json!({ "username": "fixture", "password": "fixture-password" }))
        .send()
        .await
        .expect("访问免管理会话的客户端生成入口");
    assert_eq!(publicDownload.status(), StatusCode::OK);
    assert_eq!(
        publicDownload.headers()[header::CONTENT_TYPE],
        "application/vnd.android.package-archive"
    );
    running.stop().await.expect("停止远程 Web 入口");
    controlTask.abort();
}

/// 验证规则集创建、编辑、互斥启用和批量删除全部由 SQLite 事务维护。
///
/// 运行上下文：两个规则集先后启用，旧项必须同步关闭并推进修订；批量包含未知 ID 时整批回滚。
/// 失败语义：出现两个启用项、ETag 修订未推进、旧 revision 覆盖成功或半删除均表示云规则一致性失效。
#[test]
fn ruleSetTransactionsKeepSingleEnabledRevisionAndAtomicDeletion() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("创建内存数据库"));
    let first = service
        .createRuleSet(&CreateRuleSetRequest {
            name: "主规则".to_owned(),
            content: validRoutingContent("DIRECT"),
            enabled: true,
        })
        .expect("创建首个规则集");
    let second = service
        .createRuleSet(&CreateRuleSetRequest {
            name: "备用规则".to_owned(),
            content: validRoutingContent("PROXY"),
            enabled: false,
        })
        .expect("创建备用规则集");

    let enabledSecond = service
        .setRuleSetEnabled(
            &second.ruleSetId,
            &SetRuleSetEnabledRequest {
                revision: second.revision,
                enabled: true,
            },
        )
        .expect("启用备用规则集");
    let disabledFirst = service.ruleSet(&first.ruleSetId).expect("读取首个规则集");
    assert!(!disabledFirst.enabled);
    assert!(disabledFirst.revision > first.revision);
    assert!(enabledSecond.enabled);
    assert_eq!(
        service.activeRuleSet().expect("读取启用规则"),
        enabledSecond
    );
    assert!(matches!(
        service.updateRuleSet(
            &second.ruleSetId,
            &UpdateRuleSetRequest {
                revision: second.revision,
                name: "过期编辑".to_owned(),
                content: validRoutingContent("DIRECT"),
            },
        ),
        Err(AccountServiceError::RuleSetRevisionConflict { .. })
    ));

    let batchResult = service.deleteRuleSetsBatch(&BatchDeleteRuleSetsRequest {
        ruleSetIds: vec![first.ruleSetId.clone(), "missing-rule-set".to_owned()],
    });
    assert!(matches!(
        batchResult,
        Err(AccountServiceError::RuleSetNotFound)
    ));
    assert!(service.ruleSet(&first.ruleSetId).is_ok());
    assert!(service.ruleSet(&second.ruleSetId).is_ok());
}

/// 验证服务端接受双作用域混合规则，并拒绝缺段、未知动作、范围不成对和本机路径文本。
///
/// 运行上下文：直接调用领域服务的保存边界，无效文本必须在 SQLite 写入前失败。
/// 失败语义：任一非法正文被接受都意味着客户端可能收到不可执行的配置。
#[test]
fn ruleSetValidationAcceptsRoutingTextOnly() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("创建内存数据库"));
    let mixedRuleSet = service.createRuleSet(&CreateRuleSetRequest {
        name: "混合范围".to_owned(),
        content: "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nDOMAIN,abc.com,PROXY\nFINAL,DIRECT\n[GRoutingRule]\nDOMAIN,aaa.com,PROXY\nFINAL,DIRECT\n[proxy_app]\ncom.example.client\n".to_owned(),
        enabled: false,
    });
    assert!(mixedRuleSet.is_ok());
    for invalidContent in [
        r"D:\Desktop\T\SprakCapture\Client\app\src\main\assets\bootstrap\routing.txt",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nFINAL,UNKNOWN\n[GRoutingRule]\n[proxy_app]\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nFINAL,DIRECT\n[proxy_app]\n",
        "[RoutingRule]\nFINAL,DIRECT\n[GRoutingRule]\nFINAL,PROXY\n[proxy_app]\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nPORT,70000,PROXY\n[GRoutingRule]\nFINAL,DIRECT\n[proxy_app]\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nIP-CIDR,300.1.1.1/99,PROXY\n[GRoutingRule]\nFINAL,DIRECT\n[proxy_app]\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nFINAL,DIRECT\n[GRoutingRule]\n[proxy_app]\n1invalid.package\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nFINAL,PROXY\n[GRoutingRule]\n[proxy_app]\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\n[GRoutingRule]\n[proxy_app]\ncom.example.client\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\n[GRoutingRule]\n[proxy app]\ncom.example.client\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nFINAL,PROXY\n[RoutingRule]\nFINAL,DIRECT\n[GRoutingRule]\n[proxy_app]\ncom.example.client\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nFINAL,PROXY\n[GRoutingRule]\n[proxy_app]\ncom.example.client,unexpected\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nFINAL,DIRECT\nDOMAIN,unreachable.example,PROXY\n[GRoutingRule]\n[proxy_app]\ncom.example.client\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\n[GRoutingRule]\nFINAL,PROXY\nFINAL,DIRECT\n[proxy_app]\n",
        "ORPHAN,DIRECT\n[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\n[GRoutingRule]\nFINAL,PROXY\n[proxy_app]\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[UnknownRule]\nFINAL,PROXY\n[RoutingRule]\n[GRoutingRule]\nFINAL,DIRECT\n[proxy_app]\n",
        "[DNS]\nPRIMARY,223.5.5.5\n[RoutingRule]\nFINAL,PROXY\n[GRoutingRule]\nFINAL,DIRECT\n[proxy_app]\ncom.example.client\ncom.example.client\n",
    ] {
        assert!(matches!(
            service.createRuleSet(&CreateRuleSetRequest {
                name: format!("无效规则-{}", invalidContent.len()),
                content: invalidContent.to_owned(),
                enabled: false,
            }),
            Err(AccountServiceError::Validation(_))
        ));
    }
}

/// 验证 DNS 段仅接受唯一 PRIMARY、可选唯一 SECONDARY 和 IP 字面量。
///
/// 运行上下文：服务端保存边界同时覆盖 IPv4/IPv6 成功样例及缺失、重复、未知键和主机名失败样例。
/// 失败语义：失败样例写入或成功样例被拒绝，都会使 DNS 直连策略在终端产生歧义。
#[test]
fn ruleSetDnsRequiresUniqueLiteralAddresses() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("创建内存数据库"));
    let ipv6RuleSet = service.createRuleSet(&CreateRuleSetRequest {
        name: "IPv6 DNS".to_owned(),
        content: "[DNS]\nPRIMARY,2606:4700:4700::1111\nSECONDARY,2001:4860:4860::8888\n[RoutingRule]\n[GRoutingRule]\nFINAL,PROXY\n[proxy_app]\n".to_owned(),
        enabled: false,
    });
    assert!(ipv6RuleSet.is_ok());

    for invalidDns in [
        "SECONDARY,1.1.1.1",
        "PRIMARY,dns.example.com",
        "PRIMARY,223.5.5.5\nPRIMARY,1.1.1.1",
        "PRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\nSECONDARY,8.8.8.8",
        "PRIMARY,223.5.5.5\nTERTIARY,8.8.8.8",
        "PRIMARY,223.5.5.5\n[DNS]\nSECONDARY,1.1.1.1",
    ] {
        let result = service.createRuleSet(&CreateRuleSetRequest {
            name: format!("DNS 非法-{}", invalidDns.len()),
            content: format!(
                "[DNS]\n{invalidDns}\n[RoutingRule]\n[GRoutingRule]\nFINAL,PROXY\n[proxy_app]\n"
            ),
            enabled: false,
        });
        assert!(matches!(result, Err(AccountServiceError::Validation(_))));
    }
}

/// 验证规则单行上限按 UTF-8 字节而非 Unicode 字符数执行，防止含表情符号的正文跨端判定不一致。
///
/// 运行上下文：使用注释行承载多字节字符，不改变 routing.txt 的业务语义，并精确覆盖 8192/8193 字节边界。
/// 失败语义：边界行被拒绝或超限行被接受，都表示服务端可能与 Android 客户端解析器发生配置漂移。
#[test]
fn ruleSetLineLimitUsesUtf8Bytes() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("创建内存数据库"));
    let boundaryLine = format!("#{}abc", "😀".repeat(2_047));
    let oversizedLine = format!("{boundaryLine}d");
    assert_eq!(boundaryLine.len(), 8_192);
    assert_eq!(oversizedLine.len(), 8_193);

    let accepted = service.createRuleSet(&CreateRuleSetRequest {
        name: "UTF-8 字节边界".to_owned(),
        content: format!("{boundaryLine}\n{}", validRoutingContent("PROXY")),
        enabled: false,
    });
    assert!(accepted.is_ok());

    let rejected = service.createRuleSet(&CreateRuleSetRequest {
        name: "UTF-8 字节超限".to_owned(),
        content: format!("{oversizedLine}\n{}", validRoutingContent("PROXY")),
        enabled: false,
    });
    assert!(matches!(rejected, Err(AccountServiceError::Validation(_))));
}

/// 验证客户端规则下载复用 SOCKS5 凭据且不创建租约，并严格支持 ETag 条件请求。
///
/// 运行上下文：数据库先写入任意密码账号和唯一启用规则，再启动真实公共/内部监听完成端到端请求。
/// 失败语义：错误凭据可下载、正文/修订头错误、304 携带正文或内部校验占用连接数均视为协议失效。
#[tokio::test]
async fn clientRuleDownloadUsesBasicCredentialsEtagAndNoLeaseVerification() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("rules.db");
    let service =
        AccountDomainService::new(AccountStore::open(&databasePath).expect("创建规则数据库"));
    service
        .bootstrapManagement("Admin", "Admin123")
        .expect("初始化管理身份");
    createAnyPasswordAccount(&service, "download-user", unlimitedPolicy()).await;
    service
        .createRuleSet(&CreateRuleSetRequest {
            name: "客户端下载规则".to_owned(),
            content: validRoutingContent("DIRECT"),
            enabled: true,
        })
        .expect("创建启用规则集");
    drop(service);
    let internalToken = "rules-download-internal-token-at-least-32-bytes";
    let running = startAccountService(AccountServerConfig {
        databasePath,
        publicAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalToken: internalToken.to_owned(),
        controlBaseUrl: "http://127.0.0.1:9".to_owned(),
        webAssetsDirectory: None,
    })
    .await
    .expect("启动账号服务");
    let client = Client::new();
    let publicUrl = format!("http://{}/api/v1/client/routing.txt", running.publicAddress);
    let rejected = client
        .get(&publicUrl)
        .basic_auth("download-user", Some(""))
        .send()
        .await
        .expect("发送空密码请求");
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    let downloaded = client
        .get(&publicUrl)
        .basic_auth("download-user", Some("any-password"))
        .send()
        .await
        .expect("下载规则集");
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(
        downloaded.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    let etag = downloaded.headers()[header::ETAG]
        .to_str()
        .expect("读取 ETag")
        .to_owned();
    assert!(
        downloaded
            .text()
            .await
            .expect("读取规则正文")
            .contains("[GRoutingRule]")
    );
    let notModified = client
        .get(&publicUrl)
        .basic_auth("download-user", Some("any-password"))
        .header(header::IF_NONE_MATCH, etag)
        .send()
        .await
        .expect("条件下载规则集");
    assert_eq!(notModified.status(), StatusCode::NOT_MODIFIED);
    assert!(notModified.bytes().await.expect("读取 304 正文").is_empty());

    let verified = client
        .post(format!(
            "http://{}/internal/v1/accounts/verify",
            running.internalAddress
        ))
        .header("x-account-service-token", internalToken)
        .json(&json!({ "username": "download-user", "password": "another-password" }))
        .send()
        .await
        .expect("调用内部账号校验");
    assert_eq!(verified.status(), StatusCode::NO_CONTENT);
    let activeMetadata: serde_json::Value = client
        .get(format!(
            "http://{}/internal/v1/ruleSets/active",
            running.internalAddress
        ))
        .header("x-account-service-token", internalToken)
        .send()
        .await
        .expect("读取启用规则元数据")
        .json()
        .await
        .expect("解析启用规则元数据");
    assert!(
        activeMetadata["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(activeMetadata["revision"], 1);
    let statistics: serde_json::Value = client
        .get(format!(
            "http://{}/internal/v1/statistics",
            running.internalAddress
        ))
        .header("x-account-service-token", internalToken)
        .send()
        .await
        .expect("读取内部统计")
        .json()
        .await
        .expect("解析内部统计");
    assert_eq!(statistics["activeConnections"], 0);
    running.stop().await.expect("停止账号服务");
}

/// 验证批量策略事务按每个账号原到期时间加时，并对永不过期与禁用特殊值保持不变。
///
/// 运行上下文：两个明确日期分别代表尚未过期和已经过期；服务端不得用当前时间重置任何基准。
/// 失败语义：任一策略字段、修订号或特殊到期值不符都表示批量事务产生了错误业务语义。
#[tokio::test]
async fn batchUpdateAddsDurationToOriginalExpirationAndUpdatesLimits() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("创建内存数据库"));
    service
        .bootstrapManagement("Admin", "Admin123")
        .expect("初始化管理身份");
    let futureExpiration = 1_776_038_400_000_i64;
    let pastExpiration = 1_775_865_600_000_i64;
    let mut expiringPolicy = unlimitedPolicy();
    expiringPolicy.expiresAt = futureExpiration;
    let futureId = createAnyPasswordAccount(&service, "future-user", expiringPolicy.clone()).await;
    expiringPolicy.expiresAt = pastExpiration;
    let pastId = createAnyPasswordAccount(&service, "past-user", expiringPolicy).await;
    let neverId = createAnyPasswordAccount(&service, "never-user", unlimitedPolicy()).await;
    let selections = [&futureId, &pastId, &neverId]
        .into_iter()
        .map(|accountId| {
            let account = service.account(accountId).expect("读取账号");
            BatchAccountSelection {
                accountId: account.accountId,
                policyRevision: account.policyRevision,
            }
        })
        .collect();

    let response = service
        .updateAccountsBatch(&BatchUpdateAccountsRequest {
            accounts: selections,
            maxOnlineIps: Some(2),
            maxConnections: Some(3),
            maxUploadBytesPerSecond: Some(4_096),
            maxDownloadBytesPerSecond: Some(8_192),
            extendByMilliseconds: Some(86_400_000),
        })
        .await
        .expect("批量更新账号");

    assert_eq!(response.updatedAccounts, 3);
    assert_eq!(
        service
            .account(&futureId)
            .expect("未来账号")
            .policy
            .expiresAt,
        futureExpiration + 86_400_000
    );
    assert_eq!(
        service.account(&pastId).expect("过期账号").policy.expiresAt,
        pastExpiration + 86_400_000
    );
    let never = service.account(&neverId).expect("永不过期账号");
    assert_eq!(never.policy.expiresAt, -1);
    assert_eq!(never.policy.maxOnlineIps, 2);
    assert_eq!(never.policy.maxConnections, 3);
    assert_eq!(never.policy.maxUploadBytesPerSecond, 4_096);
    assert_eq!(never.policy.maxDownloadBytesPerSecond, 8_192);
}

/// 验证批量删除在提交前校验全部策略修订号，冲突时不允许部分账号被删除。
#[tokio::test]
async fn batchDeleteRollsBackWhenAnyRevisionIsStale() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("创建内存数据库"));
    service
        .bootstrapManagement("Admin", "Admin123")
        .expect("初始化管理身份");
    let firstId = createAnyPasswordAccount(&service, "batch-delete-a", unlimitedPolicy()).await;
    let secondId = createAnyPasswordAccount(&service, "batch-delete-b", unlimitedPolicy()).await;
    let first = service.account(&firstId).expect("读取第一账号");
    let second = service.account(&secondId).expect("读取第二账号");

    let result = service
        .deleteAccountsBatch(&BatchDeleteAccountsRequest {
            accounts: vec![
                BatchAccountSelection {
                    accountId: first.accountId,
                    policyRevision: first.policyRevision,
                },
                BatchAccountSelection {
                    accountId: second.accountId,
                    policyRevision: second.policyRevision + 1,
                },
            ],
        })
        .await;

    assert!(matches!(
        result,
        Err(AccountServiceError::PolicyRevisionConflict { .. })
    ));
    service.account(&firstId).expect("冲突后第一账号仍存在");
    service.account(&secondId).expect("冲突后第二账号仍存在");
}

/// 创建任意密码账号，返回公共账号 ID。
async fn createAnyPasswordAccount(
    service: &AccountDomainService,
    username: &str,
    policy: AccountPolicy,
) -> String {
    service
        .createAccount(&CreateAccountRequest {
            username: username.to_owned(),
            password: None,
            policy,
            remark: None,
        })
        .await
        .expect("创建测试账号")
        .accountId
}

/// 验证已授权入口无需重新接收密码即可恢复同一 Key，修改凭据后旧 Key 立即失效且新 Key 不同。
#[test]
fn managementIdentityAndApiKeyStayConsistent() {
    let store = AccountStore::openInMemory().expect("创建内存数据库");
    let first = store
        .bootstrapManagement("Admin", "Admin123")
        .expect("初始化管理身份");
    let derived = store.managementApiKey().expect("恢复当前 Key");
    assert_eq!(first.apiKey, derived.apiKey);
    store
        .authenticateApiKey(&first.apiKey)
        .expect("当前 Key 应通过认证");

    let changed = store
        .updateManagementIdentity("Operator", "NewPassword")
        .expect("更新管理身份");
    assert_ne!(first.apiKey, changed.apiKey);
    assert!(matches!(
        store.authenticateApiKey(&first.apiKey),
        Err(AccountServiceError::ManagementAuthenticationFailed)
    ));
    store
        .authenticateApiKey(&changed.apiKey)
        .expect("新 Key 应通过认证");
    assert!(matches!(
        store.authenticateManagement("Admin", "Admin123"),
        Err(AccountServiceError::ManagementAuthenticationFailed)
    ));
    store
        .authenticateManagement("Operator", "NewPassword")
        .expect("新管理身份应生效");
    assert_eq!(
        changed.apiKey,
        store.managementApiKey().expect("恢复修改后的 Key").apiKey
    );
}

/// 验证旧派生盐只迁移一次，而带当前版本标记的摘要损坏会明确失败。
///
/// 运行上下文：测试直接改写隔离 SQLite 以模拟升级前记录与当前记录损坏；失败语义用于保证兼容迁移
/// 不会退化成“任意摘要不一致都静默覆盖”，从而掩盖真实数据库完整性问题。
#[test]
fn legacyApiKeyMaterialMigratesWithoutMaskingCurrentCorruption() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("accounts.db");
    let store = AccountStore::open(&databasePath).expect("创建账号数据库");
    store
        .bootstrapManagement("Admin", "Admin123")
        .expect("初始化管理身份");
    drop(store);

    let connection = Connection::open(&databasePath).expect("打开迁移夹具数据库");
    connection
        .execute(
            "UPDATE managementIdentity SET apiKeySalt = substr(apiKeySalt, 4), apiKeyHash = ?1 WHERE singletonId = 1",
            params!["legacy-algorithm-hash"],
        )
        .expect("模拟旧算法材料");
    drop(connection);

    let store = AccountStore::open(&databasePath).expect("重新打开旧数据库");
    let migrated = store.managementApiKey().expect("迁移并恢复当前 Key");
    store
        .authenticateApiKey(&migrated.apiKey)
        .expect("迁移后的 Key 摘要必须生效");
    drop(store);

    let connection = Connection::open(&databasePath).expect("打开已迁移数据库");
    let salt: String = connection
        .query_row(
            "SELECT apiKeySalt FROM managementIdentity WHERE singletonId = 1",
            [],
            |row| row.get(0),
        )
        .expect("读取当前派生盐");
    assert!(salt.starts_with("v2."));
    connection
        .execute(
            "UPDATE managementIdentity SET apiKeyHash = ?1 WHERE singletonId = 1",
            params!["current-material-corruption"],
        )
        .expect("注入当前摘要损坏");
    drop(connection);

    let store = AccountStore::open(&databasePath).expect("重新打开损坏数据库");
    assert!(matches!(
        store.managementApiKey(),
        Err(AccountServiceError::Credential)
    ));
}

/// 验证空密码模式接受任意非空密码，固定密码模式只接受精确值。
#[tokio::test]
async fn anyAndFixedPasswordModesKeepDistinctSemantics() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("内存数据库"));
    createAnyPasswordAccount(&service, "any-user", unlimitedPolicy()).await;
    service
        .authenticateLease(&LeaseAuthenticationRequest {
            connectionId: "any-1".to_owned(),
            username: "any-user".to_owned(),
            password: "任意非空值".to_owned(),
            sourceIp: "127.0.0.1".to_owned(),
        })
        .await
        .expect("任意密码账号应认证成功");
    assert!(matches!(
        service
            .authenticateLease(&LeaseAuthenticationRequest {
                connectionId: "any-2".to_owned(),
                username: "any-user".to_owned(),
                password: String::new(),
                sourceIp: "127.0.0.1".to_owned(),
            })
            .await,
        Err(AccountServiceError::SocksAuthenticationFailed)
    ));

    service
        .createAccount(&CreateAccountRequest {
            username: "fixed-user".to_owned(),
            password: Some("correct-password".to_owned()),
            policy: unlimitedPolicy(),
            remark: None,
        })
        .await
        .expect("创建固定密码账号");
    assert!(matches!(
        service
            .authenticateLease(&LeaseAuthenticationRequest {
                connectionId: "fixed-1".to_owned(),
                username: "fixed-user".to_owned(),
                password: "wrong-password".to_owned(),
                sourceIp: "127.0.0.1".to_owned(),
            })
            .await,
        Err(AccountServiceError::SocksAuthenticationFailed)
    ));
    service
        .authenticateLease(&LeaseAuthenticationRequest {
            connectionId: "fixed-2".to_owned(),
            username: "fixed-user".to_owned(),
            password: "correct-password".to_owned(),
            sourceIp: "127.0.0.1".to_owned(),
        })
        .await
        .expect("正确固定密码应通过认证");
}

/// 验证连接数与去重来源 IP 在并发租约注册表中严格生效。
#[tokio::test]
async fn connectionAndIpLimitsCannotBeExceeded() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("内存数据库"));
    let mut policy = unlimitedPolicy();
    policy.maxConnections = 2;
    policy.maxOnlineIps = 1;
    createAnyPasswordAccount(&service, "limited", policy).await;
    for (connectionId, sourceIp) in [
        ("connection-1", "127.0.0.1"),
        ("connection-2", "::ffff:127.0.0.1"),
    ] {
        service
            .authenticateLease(&LeaseAuthenticationRequest {
                connectionId: connectionId.to_owned(),
                username: "limited".to_owned(),
                password: "value".to_owned(),
                sourceIp: sourceIp.to_owned(),
            })
            .await
            .expect("同一规范化 IP 的两条连接应被允许");
    }
    assert!(matches!(
        service
            .authenticateLease(&LeaseAuthenticationRequest {
                connectionId: "connection-3".to_owned(),
                username: "limited".to_owned(),
                password: "value".to_owned(),
                sourceIp: "127.0.0.2".to_owned(),
            })
            .await,
        Err(AccountServiceError::SocksAuthenticationFailed)
    ));
}

/// 验证管理概览使用相邻同步区间计算实时速率，并与逐连接快照保持同一口径。
///
/// 运行上下文：数据面上报连接生命周期累计字节，账号服务负责按心跳间隔换算每秒速率。
/// 失败语义：速率为零或聚合值与连接值不一致，表示概览展示了累计量或丢失了租约带宽。
#[tokio::test]
async fn overviewStatisticsExposeRealtimeTransferRates() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("内存数据库"));
    createAnyPasswordAccount(&service, "realtime-user", unlimitedPolicy()).await;
    let authentication = service
        .authenticateLease(&LeaseAuthenticationRequest {
            connectionId: "realtime-connection".to_owned(),
            username: "realtime-user".to_owned(),
            password: "value".to_owned(),
            sourceIp: "192.0.2.80".to_owned(),
        })
        .await
        .expect("创建实时统计租约");
    tokio::time::sleep(Duration::from_millis(25)).await;
    service
        .synchronizeLeases(&LeaseSynchronizationRequest {
            serviceInstanceId: service.serviceInstanceId().to_owned(),
            batchId: "realtime-rate-batch".to_owned(),
            leases: vec![LeaseProgress {
                leaseId: authentication.leaseId,
                connectionId: "realtime-connection".to_owned(),
                uploadedBytes: 2_000,
                downloadedBytes: 4_000,
                final_: false,
            }],
        })
        .await
        .expect("同步实时统计租约");

    let statistics = service.statistics().expect("读取概览统计");
    let connections = service.connections(None);
    assert_eq!(statistics.activeConnections, 1);
    assert_eq!(connections.len(), 1);
    assert!(statistics.uploadBytesPerSecond > 0);
    assert!(statistics.downloadBytesPerSecond > statistics.uploadBytesPerSecond);
    assert_eq!(
        statistics.uploadBytesPerSecond,
        connections[0].uploadBytesPerSecond
    );
    assert_eq!(
        statistics.downloadBytesPerSecond,
        connections[0].downloadBytesPerSecond
    );
}

/// 验证同步批次重试不会重复累计流量，final 会回收活动租约。
#[tokio::test]
async fn leaseSynchronizationIsIdempotentAndFinalReleasesLease() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("内存数据库"));
    let accountId = createAnyPasswordAccount(&service, "traffic-user", unlimitedPolicy()).await;
    let authentication = service
        .authenticateLease(&LeaseAuthenticationRequest {
            connectionId: "traffic-connection".to_owned(),
            username: "traffic-user".to_owned(),
            password: "value".to_owned(),
            sourceIp: "192.0.2.1".to_owned(),
        })
        .await
        .expect("认证并创建租约");
    let request = LeaseSynchronizationRequest {
        serviceInstanceId: service.serviceInstanceId().to_owned(),
        batchId: "batch-1".to_owned(),
        leases: vec![LeaseProgress {
            leaseId: authentication.leaseId,
            connectionId: "traffic-connection".to_owned(),
            uploadedBytes: 100,
            downloadedBytes: 200,
            final_: true,
        }],
    };
    service.synchronizeLeases(&request).await.expect("首次同步");
    service
        .synchronizeLeases(&request)
        .await
        .expect("重复批次应幂等成功");
    let account = service.account(&accountId).expect("读取账号统计");
    assert_eq!(account.uploadedBytes, 100);
    assert_eq!(account.downloadedBytes, 200);
    assert_eq!(account.activeConnections, 0);
    let usage = service.accountUsage(&accountId).expect("读取账号用量");
    assert_eq!(usage.uploadedBytes, 100);
    assert_eq!(usage.downloadedBytes, 200);
    assert_eq!(usage.acceptedConnections, 1);
    assert_eq!(usage.daily.len(), 1);
}

/// 验证未来到期时间抵达后，持续心跳的存量租约会在下一同步被服务端撤销。
#[tokio::test]
async fn existingLeaseIsRevokedWhenExpirationArrives() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("内存数据库"));
    let mut policy = unlimitedPolicy();
    policy.expiresAt = currentTimeMilliseconds() + 50;
    createAnyPasswordAccount(&service, "expiring-user", policy).await;
    let authentication = service
        .authenticateLease(&LeaseAuthenticationRequest {
            connectionId: "expiring-connection".to_owned(),
            username: "expiring-user".to_owned(),
            password: "value".to_owned(),
            sourceIp: "192.0.2.40".to_owned(),
        })
        .await
        .expect("到期前应允许认证");
    tokio::time::sleep(Duration::from_millis(80)).await;
    let response = service
        .synchronizeLeases(&LeaseSynchronizationRequest {
            serviceInstanceId: service.serviceInstanceId().to_owned(),
            batchId: "expiration-batch".to_owned(),
            leases: vec![LeaseProgress {
                leaseId: authentication.leaseId,
                connectionId: "expiring-connection".to_owned(),
                uploadedBytes: 10,
                downloadedBytes: 20,
                final_: false,
            }],
        })
        .await
        .expect("到期同步仍需确认最后流量");
    assert!(response.leases[0].revoked);
}

/// 验证 SQLite 暂态失败后更换批次仍会提交全部差值，且失败的 final 不会提前删除租约。
#[tokio::test]
async fn usagePersistenceRecoversAcrossBatchAndFinalFailure() {
    let directory = tempfile::tempdir().expect("创建数据库目录");
    let databasePath = directory.path().join("usage-recovery.sqlite3");
    let service = AccountDomainService::new(AccountStore::open(&databasePath).expect("创建数据库"));
    let accountId = createAnyPasswordAccount(&service, "recovery-user", unlimitedPolicy()).await;
    let authentication = service
        .authenticateLease(&LeaseAuthenticationRequest {
            connectionId: "recovery-connection".to_owned(),
            username: "recovery-user".to_owned(),
            password: "value".to_owned(),
            sourceIp: "198.51.100.40".to_owned(),
        })
        .await
        .expect("创建恢复测试租约");
    let faultConnection = rusqlite::Connection::open(&databasePath).expect("打开故障注入连接");
    faultConnection
        .execute_batch(
            "CREATE TRIGGER rejectUsageUpdate BEFORE UPDATE ON usageCounters
             BEGIN SELECT RAISE(FAIL, 'fixture usage failure'); END;",
        )
        .expect("安装写入失败夹具");
    let failedFinal = LeaseSynchronizationRequest {
        serviceInstanceId: service.serviceInstanceId().to_owned(),
        batchId: "failed-final-batch".to_owned(),
        leases: vec![LeaseProgress {
            leaseId: authentication.leaseId.clone(),
            connectionId: "recovery-connection".to_owned(),
            uploadedBytes: 100,
            downloadedBytes: 200,
            final_: true,
        }],
    };
    assert!(service.synchronizeLeases(&failedFinal).await.is_err());
    assert_eq!(service.connections(Some(&accountId)).len(), 1);
    faultConnection
        .execute_batch("DROP TRIGGER rejectUsageUpdate;")
        .expect("移除写入失败夹具");
    service
        .synchronizeLeases(&LeaseSynchronizationRequest {
            serviceInstanceId: service.serviceInstanceId().to_owned(),
            batchId: "replacement-final-batch".to_owned(),
            leases: vec![LeaseProgress {
                leaseId: authentication.leaseId,
                connectionId: "recovery-connection".to_owned(),
                uploadedBytes: 150,
                downloadedBytes: 250,
                final_: true,
            }],
        })
        .await
        .expect("新批次应提交此前全部待写流量");
    let usage = service.accountUsage(&accountId).expect("读取恢复后的流量");
    assert_eq!(usage.uploadedBytes, 150);
    assert_eq!(usage.downloadedBytes, 250);
    assert!(service.connections(Some(&accountId)).is_empty());
}

/// 删除账号必须同步清除待写尾流量，旧租约后续同步只返回 missing 而不触发外键错误。
#[tokio::test]
async fn deletingAccountClearsPendingUsageAndOldLease() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("内存数据库"));
    let accountId = createAnyPasswordAccount(&service, "deleted-user", unlimitedPolicy()).await;
    let authentication = service
        .authenticateLease(&LeaseAuthenticationRequest {
            connectionId: "deleted-connection".to_owned(),
            username: "deleted-user".to_owned(),
            password: "value".to_owned(),
            sourceIp: "203.0.113.40".to_owned(),
        })
        .await
        .expect("创建删除测试租约");
    service.deleteAccount(&accountId).await.expect("删除账号");
    let response = service
        .synchronizeLeases(&LeaseSynchronizationRequest {
            serviceInstanceId: service.serviceInstanceId().to_owned(),
            batchId: "deleted-account-batch".to_owned(),
            leases: vec![LeaseProgress {
                leaseId: authentication.leaseId,
                connectionId: "deleted-connection".to_owned(),
                uploadedBytes: 100,
                downloadedBytes: 200,
                final_: true,
            }],
        })
        .await
        .expect("旧租约同步不得触发外键错误");
    assert!(response.leases[0].revoked);
    assert_eq!(
        response.leases[0].errorCode.as_deref(),
        Some("leaseNotFound")
    );
}

/// 验证策略版本冲突不会覆盖新值，零值策略会撤销现有租约。
#[tokio::test]
async fn policyRevisionConflictAndZeroValueRevokeExistingLease() {
    let service = AccountDomainService::new(AccountStore::openInMemory().expect("内存数据库"));
    let accountId = createAnyPasswordAccount(&service, "policy-user", unlimitedPolicy()).await;
    service
        .authenticateLease(&LeaseAuthenticationRequest {
            connectionId: "policy-connection".to_owned(),
            username: "policy-user".to_owned(),
            password: "value".to_owned(),
            sourceIp: "198.51.100.1".to_owned(),
        })
        .await
        .expect("创建活动租约");
    let mut disabledPolicy = unlimitedPolicy();
    disabledPolicy.maxConnections = 0;
    let updated = service
        .updateAccount(
            &accountId,
            &UpdateAccountRequest {
                policyRevision: 1,
                policy: disabledPolicy,
                remark: None,
            },
        )
        .await
        .expect("禁用账号");
    assert_eq!(updated.policyRevision, 2);
    assert!(service.connections(Some(&accountId))[0].revoked);
    assert!(matches!(
        service
            .updateAccount(
                &accountId,
                &UpdateAccountRequest {
                    policyRevision: 1,
                    policy: unlimitedPolicy(),
                    remark: None,
                },
            )
            .await,
        Err(AccountServiceError::PolicyRevisionConflict { currentRevision: 2 })
    ));
}

/// 启动真实双监听服务并验证默认登录、Cookie 管理和账号创建主链路。
#[tokio::test]
async fn publicHttpLoginAndAccountCreationWorkEndToEnd() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let running = startAccountService(AccountServerConfig {
        databasePath: temporaryDirectory.path().join("accounts.db"),
        publicAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalToken: "internal-token-with-at-least-32-bytes".to_owned(),
        controlBaseUrl: "http://127.0.0.1:17890".to_owned(),
        webAssetsDirectory: None,
    })
    .await
    .expect("启动账号服务");
    let baseUrl = format!("http://{}", running.publicAddress);
    let client = Client::new();
    let loginResponse = client
        .post(format!("{baseUrl}/api/v1/auth/login"))
        .json(&json!({ "username": "Admin", "password": "Admin123" }))
        .send()
        .await
        .expect("发送登录请求");
    assert_eq!(loginResponse.status(), StatusCode::OK);
    let sessionCookie = loginResponse
        .headers()
        .get(header::SET_COOKIE)
        .expect("登录响应 Cookie")
        .to_str()
        .expect("Cookie 文本")
        .split(';')
        .next()
        .expect("Cookie 首段")
        .to_owned();
    let createResponse = client
        .post(format!("{baseUrl}/account-management/api/v1/accounts"))
        .header(header::COOKIE, sessionCookie)
        .json(&json!({
            "username": "http-user",
            "password": null,
            "maxUploadBytesPerSecond": -1,
            "maxDownloadBytesPerSecond": -1,
            "maxConnections": 2,
            "maxOnlineIps": 1,
            "expiresAt": -1,
            "remark": "HTTP 测试"
        }))
        .send()
        .await
        .expect("创建账号请求");
    assert_eq!(createResponse.status(), StatusCode::CREATED);
    let body: serde_json::Value = createResponse.json().await.expect("账号响应 JSON");
    assert_eq!(body["username"], "http-user");
    assert_eq!(body["passwordMode"], "any");
    running.stop().await.expect("停止账号服务");
}

/// 验证浏览器会话跨页面刷新和账号服务重启保持有效，只有管理身份变更才使旧 Cookie 失效。
///
/// 运行上下文：签名密钥与 SQLite 放在同一运行数据目录，Cookie 本身不依赖进程内会话表。
/// 失败语义：重启后未授权、Cookie 非持久化或凭据更新后仍授权都表示会话生命周期错误。
#[tokio::test]
async fn browserSessionPersistsAcrossRestartUntilIdentityChanges() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("accounts.db");
    let internalToken = "persistent-session-internal-token-at-least-32-bytes";
    let start = || AccountServerConfig {
        databasePath: databasePath.clone(),
        publicAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalToken: internalToken.to_owned(),
        controlBaseUrl: "http://127.0.0.1:17890".to_owned(),
        webAssetsDirectory: None,
    };
    let firstRun = startAccountService(start())
        .await
        .expect("首次启动账号服务");
    let client = Client::new();
    let loginResponse = client
        .post(format!(
            "http://{}/api/v1/auth/login",
            firstRun.publicAddress
        ))
        .json(&json!({ "username": "Admin", "password": "Admin123" }))
        .send()
        .await
        .expect("登录");
    let setCookie = loginResponse.headers()[header::SET_COOKIE]
        .to_str()
        .expect("持久 Cookie")
        .to_owned();
    assert!(setCookie.contains("Max-Age=315360000"));
    let cookie = setCookie.split(';').next().expect("Cookie 首段").to_owned();
    firstRun.stop().await.expect("停止首次服务");

    let secondRun = startAccountService(start()).await.expect("重启账号服务");
    let publicUrl = format!("http://{}", secondRun.publicAddress);
    let internalUrl = format!("http://{}", secondRun.internalAddress);
    let resumed = client
        .get(format!("{publicUrl}/api/v1/auth/session"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .expect("重启后校验会话");
    assert_eq!(resumed.status(), StatusCode::OK);

    let changed = client
        .put(format!("{internalUrl}/internal/v1/management/identity"))
        .header("x-account-service-token", internalToken)
        .json(&json!({ "username": "Admin2", "password": "Admin456" }))
        .send()
        .await
        .expect("修改管理身份");
    assert_eq!(changed.status(), StatusCode::OK);
    let revoked = client
        .get(format!("{publicUrl}/api/v1/auth/session"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .expect("校验旧会话");
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    secondRun.stop().await.expect("停止重启服务");
}

/// 验证完整 Key 不需要密码正文，且控制面入口不依赖客户端地址、只能消费一次并建立普通 HttpOnly 会话。
#[tokio::test]
async fn internalControlCreatesPasswordlessKeyAndOneTimeBrowserSession() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let internalToken = "session-test-internal-token-at-least-32-bytes";
    let running = startAccountService(AccountServerConfig {
        databasePath: temporaryDirectory.path().join("accounts.db"),
        publicAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalToken: internalToken.to_owned(),
        controlBaseUrl: "http://127.0.0.1:17890".to_owned(),
        webAssetsDirectory: None,
    })
    .await
    .expect("启动账号服务");
    let publicUrl = format!("http://{}", running.publicAddress);
    let internalUrl = format!("http://{}", running.internalAddress);
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("创建不跟随重定向客户端");

    let directSession = client
        .get(format!("{publicUrl}/api/v1/auth/session"))
        .send()
        .await
        .expect("直接访问会话接口");
    assert_eq!(directSession.status(), StatusCode::UNAUTHORIZED);

    let keyResponse = client
        .get(format!("{internalUrl}/internal/v1/management/apiKey"))
        .header("x-account-service-token", internalToken)
        .send()
        .await
        .expect("读取完整 Key");
    assert_eq!(keyResponse.status(), StatusCode::OK);
    let keyBody: serde_json::Value = keyResponse.json().await.expect("Key 响应 JSON");
    assert!(
        keyBody["apiKey"]
            .as_str()
            .is_some_and(|value| value.starts_with("sak_v1_"))
    );

    let statisticsBody: serde_json::Value = client
        .get(format!("{internalUrl}/internal/v1/statistics"))
        .header("x-account-service-token", internalToken)
        .send()
        .await
        .expect("读取内部实时摘要")
        .json()
        .await
        .expect("实时摘要 JSON");
    let statisticsObject = statisticsBody.as_object().expect("实时摘要对象");
    assert_eq!(statisticsObject.len(), 4);
    for field in [
        "onlineAccounts",
        "activeConnections",
        "uploadBytesPerSecond",
        "downloadBytesPerSecond",
    ] {
        assert!(
            statisticsObject.contains_key(field),
            "缺少摘要字段：{field}"
        );
    }

    let ticketResponse: serde_json::Value = client
        .post(format!("{internalUrl}/internal/v1/management/session"))
        .header("x-account-service-token", internalToken)
        .send()
        .await
        .expect("签发一次性入口")
        .json()
        .await
        .expect("入口响应 JSON");
    let ticketPath = ticketResponse["path"].as_str().expect("入口相对路径");
    assert!(ticketPath.starts_with("/api/v1/auth/local?ticket="));

    let firstVisit = client
        .get(format!("{publicUrl}/account-management{ticketPath}"))
        .send()
        .await
        .expect("首次消费入口");
    assert_eq!(firstVisit.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        firstVisit.headers()[header::LOCATION],
        "/account-management"
    );
    let sessionCookie = firstVisit.headers()[header::SET_COOKIE]
        .to_str()
        .expect("会话 Cookie")
        .split(';')
        .next()
        .expect("Cookie 首段")
        .to_owned();
    let sessionResponse = client
        .get(format!(
            "{publicUrl}/account-management/api/v1/auth/session"
        ))
        .header(header::COOKIE, &sessionCookie)
        .send()
        .await
        .expect("验证账号子路由持久会话");
    assert_eq!(sessionResponse.status(), StatusCode::OK);
    let repeatedVisit = client
        .get(format!("{publicUrl}/account-management{ticketPath}"))
        .send()
        .await
        .expect("重复消费入口");
    assert_eq!(repeatedVisit.status(), StatusCode::UNAUTHORIZED);
    running.stop().await.expect("停止账号服务");
}

/// 验证账号查询先过滤排序再分页，并严格拒绝未知参数和枚举值。
///
/// 运行上下文：真实公共 HTTP 路由同时覆盖 Serde 查询契约、领域投影和 OpenAPI 参数声明。
/// 失败语义：任一断言失败都表示自动化调用可能出现跨页遗漏、静默忽略拼错参数或文档漂移。
#[tokio::test]
async fn publicAccountQueryFiltersSortsAndValidatesStrictly() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let running = startAccountService(AccountServerConfig {
        databasePath: temporaryDirectory.path().join("accounts.db"),
        publicAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalToken: "query-test-internal-token-at-least-32-bytes".to_owned(),
        controlBaseUrl: "http://127.0.0.1:17890".to_owned(),
        webAssetsDirectory: None,
    })
    .await
    .expect("启动账号服务");
    let baseUrl = format!("http://{}", running.publicAddress);
    let client = Client::new();
    let loginResponse = client
        .post(format!("{baseUrl}/api/v1/auth/login"))
        .json(&json!({ "username": "Admin", "password": "Admin123" }))
        .send()
        .await
        .expect("登录账号服务");
    let sessionCookie = loginResponse
        .headers()
        .get(header::SET_COOKIE)
        .expect("登录响应 Cookie")
        .to_str()
        .expect("Cookie 文本")
        .split(';')
        .next()
        .expect("Cookie 首段")
        .to_owned();
    let now = currentTimeMilliseconds();
    for (username, expiresAt, maximumConnections, remark) in [
        ("alpha", -1, -1, "核心账号"),
        ("beta", -1, 0, "禁用账号"),
        ("gamma", now + 3_600_000, -1, "计划到期"),
        ("delta", now - 3_600_000, -1, "已经到期"),
    ] {
        let response = client
            .post(format!("{baseUrl}/account-management/api/v1/accounts"))
            .header(header::COOKIE, &sessionCookie)
            .json(&json!({
                "username": username,
                "password": null,
                "maxUploadBytesPerSecond": -1,
                "maxDownloadBytesPerSecond": -1,
                "maxConnections": maximumConnections,
                "maxOnlineIps": -1,
                "expiresAt": expiresAt,
                "remark": remark
            }))
            .send()
            .await
            .expect("创建查询夹具账号");
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let filteredPage = client
        .get(format!(
            "{baseUrl}/account-management/api/v1/accounts?status=available&sort=username&order=asc&offset=1&limit=1"
        ))
        .header(header::COOKIE, &sessionCookie)
        .send()
        .await
        .expect("查询过滤分页");
    assert_eq!(filteredPage.status(), StatusCode::OK);
    let filteredBody: serde_json::Value = filteredPage.json().await.expect("过滤响应 JSON");
    assert_eq!(filteredBody.as_array().expect("账号数组").len(), 1);
    assert_eq!(filteredBody[0]["username"], "gamma");

    let expiredSearch = client
        .get(format!(
            "{baseUrl}/account-management/api/v1/accounts?search=%E5%B7%B2%E7%BB%8F&expiration=expired"
        ))
        .header(header::COOKIE, &sessionCookie)
        .send()
        .await
        .expect("查询搜索和到期筛选");
    let expiredBody: serde_json::Value = expiredSearch.json().await.expect("到期响应 JSON");
    assert_eq!(expiredBody[0]["username"], "delta");

    for query in ["unknown=value", "status=active", "sort=username%20DESC"] {
        let response = client
            .get(format!(
                "{baseUrl}/account-management/api/v1/accounts?{query}"
            ))
            .header(header::COOKIE, &sessionCookie)
            .send()
            .await
            .expect("发送非法查询");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "query={query}");
    }

    let openApi: serde_json::Value = client
        .get(format!("{baseUrl}/account-management/api/v1/openapi.json"))
        .send()
        .await
        .expect("读取 OpenAPI")
        .json()
        .await
        .expect("OpenAPI JSON");
    let parameters = openApi["paths"]["/api/v1/accounts"]["get"]["parameters"]
        .as_array()
        .expect("账号查询参数数组");
    for requiredName in [
        "offset",
        "limit",
        "search",
        "status",
        "expiration",
        "sort",
        "order",
    ] {
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter["name"] == requiredName),
            "OpenAPI 缺少查询参数：{requiredName}"
        );
    }
    let managementServerUrl = openApi["servers"][0]["url"]
        .as_str()
        .expect("管理 OpenAPI 必须声明相对服务器地址");
    assert_eq!(managementServerUrl, "/account-management");
    for requiredPath in [
        "/api/v1/ruleSets",
        "/api/v1/ruleSets/batch",
        "/api/v1/ruleSets/{ruleSetId}",
        "/api/v1/ruleSets/{ruleSetId}/enabled",
    ] {
        assert!(
            openApi["paths"].get(requiredPath).is_some(),
            "OpenAPI 缺少路径：{requiredPath}"
        );
    }
    for publicPath in [
        "/api/v1/client/routing.txt",
        "/api/v1/clientPackages/download",
    ] {
        assert!(
            openApi["paths"].get(publicPath).is_none(),
            "管理 OpenAPI 不应混入根级公共路径：{publicPath}"
        );
    }
    let routingTextSchema = &openApi["components"]["schemas"]["RoutingText"];
    let routingTextDescription = routingTextSchema["description"]
        .as_str()
        .expect("OpenAPI 必须描述规则正文");
    for requiredDnsContract in [
        "[DNS]",
        "PRIMARY,<IPv4/IPv6>",
        "SECONDARY,<IPv4/IPv6>",
        "拒绝主机名",
        "重复键",
    ] {
        assert!(
            routingTextDescription.contains(requiredDnsContract),
            "OpenAPI 缺少 DNS 协议：{requiredDnsContract}"
        );
    }
    assert_eq!(
        routingTextSchema["examples"][0]
            .as_str()
            .expect("OpenAPI 必须提供规则示例"),
        "[DNS]\nPRIMARY,223.5.5.5\nSECONDARY,1.1.1.1\n\n[RoutingRule]\n\n[GRoutingRule]\nFINAL,PROXY\n\n[proxy_app]\n"
    );
    let documentedRuleSets = client
        .get(format!("{baseUrl}{managementServerUrl}/api/v1/ruleSets"))
        .header(header::COOKIE, &sessionCookie)
        .send()
        .await
        .expect("按 OpenAPI 服务器地址请求规则集");
    assert_eq!(documentedRuleSets.status(), StatusCode::OK);
    running.stop().await.expect("停止账号服务");
}

/// 修改默认管理身份后重启真实服务，验证启动流程不会再次使用默认凭据校验或覆盖数据库。
#[tokio::test]
async fn changedManagementIdentitySurvivesHttpServiceRestart() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("accounts.db");
    let config = || AccountServerConfig {
        databasePath: databasePath.clone(),
        publicAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalAddress: SocketAddr::from(([127, 0, 0, 1], 0)),
        internalToken: "restart-test-internal-token-32-bytes".to_owned(),
        controlBaseUrl: "http://127.0.0.1:17890".to_owned(),
        webAssetsDirectory: None,
    };
    let first = startAccountService(config())
        .await
        .expect("首次启动账号服务");
    first.stop().await.expect("停止首次服务");
    {
        let store = AccountStore::open(&databasePath).expect("打开身份数据库");
        store
            .updateManagementIdentity("Operator", "NewPassword")
            .expect("修改管理身份");
    }

    let restarted = startAccountService(config())
        .await
        .expect("使用现有身份重启服务");
    let response = Client::new()
        .post(format!(
            "http://{}/api/v1/auth/login",
            restarted.publicAddress
        ))
        .json(&json!({ "username": "Operator", "password": "NewPassword" }))
        .send()
        .await
        .expect("发送新身份登录请求");
    assert_eq!(response.status(), StatusCode::OK);
    restarted.stop().await.expect("停止重启后的服务");
}

/// 打开同一个 SQLite 文件两次时账号数据保持，证明数据库由独立服务正常持久化。
#[test]
fn sqliteDatabasePersistsAccountsAcrossServiceRestarts() {
    let temporaryDirectory = TempDir::new().expect("创建临时目录");
    let databasePath = temporaryDirectory.path().join("accounts.db");
    {
        let store = AccountStore::open(&databasePath).expect("首次打开数据库");
        store
            .createAccount(&CreateAccountRequest {
                username: "persisted".to_owned(),
                password: None,
                policy: unlimitedPolicy(),
                remark: Some("持久化".to_owned()),
            })
            .expect("写入账号");
    }
    let reopened = AccountStore::open(Path::new(&databasePath)).expect("重新打开数据库");
    assert_eq!(
        reopened
            .accountByUsername("persisted")
            .expect("查询账号")
            .expect("账号应存在")
            .remark
            .as_deref(),
        Some("持久化")
    );
}
