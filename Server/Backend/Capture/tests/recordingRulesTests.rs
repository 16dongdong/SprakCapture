use capture_core::{
    BeginTransaction, RecordingConfiguration, RecordingRule, RecordingRuleAction,
    RecordingRuleConfiguration, RecordingRuleKind, RecordingRuleRuntime, RecordingRuleSet,
    RecordingSession, TransactionProtocol,
};
use location_core::ResolvedLocation;

/// 创建覆盖规则匹配所需字段的事务输入；单项测试只替换与目标条件有关的字段。
fn transaction(host: &str, protocol: &str) -> BeginTransaction {
    BeginTransaction {
        protocol: TransactionProtocol::Http,
        method: "GET".to_owned(),
        location: ResolvedLocation {
            protocol: protocol.to_owned(),
            host: host.to_owned(),
            port: 443,
            path: "/api/search".to_owned(),
            query: String::new(),
            display: format!("{protocol}://{host}/api/search"),
        },
        clientAddress: "127.0.0.1:49152".to_owned(),
        clientProcessName: Some("music.exe".to_owned()),
        clientProcessId: Some(1234),
        contentType: "application/json".to_owned(),
        startAtMilliseconds: 1,
    }
}

/// 创建有序规则配置；首条命中必须覆盖后续规则和默认动作。
fn configuration() -> RecordingRuleConfiguration {
    RecordingRuleConfiguration {
        enabled: true,
        defaultAction: RecordingRuleAction::DoNotRecord,
        ruleSets: vec![RecordingRuleSet {
            id: "primary".to_owned(),
            name: "主要规则".to_owned(),
            enabled: true,
            rules: vec![
                RecordingRule {
                    id: "reject-domain".to_owned(),
                    enabled: true,
                    kind: RecordingRuleKind::DomainSuffix,
                    value: "blocked.example".to_owned(),
                    action: RecordingRuleAction::Reject,
                },
                RecordingRule {
                    id: "record-process".to_owned(),
                    enabled: true,
                    kind: RecordingRuleKind::ProcessName,
                    value: "music.exe".to_owned(),
                    action: RecordingRuleAction::Record,
                },
            ],
        }],
    }
}

#[test]
fn applies_first_matching_rule_and_default_action() {
    let runtime = RecordingRuleRuntime::new(configuration()).expect("规则配置应有效");
    assert_eq!(
        runtime.decision(&transaction("api.blocked.example", "https")),
        RecordingRuleAction::Reject
    );
    assert_eq!(
        runtime.decision(&transaction("allowed.example", "https")),
        RecordingRuleAction::Record
    );

    let mut unmatched = transaction("allowed.example", "https");
    unmatched.clientProcessName = Some("other.exe".to_owned());
    assert_eq!(
        runtime.decision(&unmatched),
        RecordingRuleAction::DoNotRecord
    );
}

#[test]
fn rejects_duplicate_identifiers_and_invalid_port_ranges() {
    let mut duplicate = configuration();
    duplicate.ruleSets[0].rules[0].id = "primary".to_owned();
    assert!(RecordingRuleRuntime::new(duplicate).is_err());

    let mut invalid_port = configuration();
    invalid_port.ruleSets[0].rules[0].kind = RecordingRuleKind::Port;
    invalid_port.ruleSets[0].rules[0].value = "9000-8000".to_owned();
    assert!(RecordingRuleRuntime::new(invalid_port).is_err());
}

#[test]
fn replacing_configuration_changes_decision_without_restart() {
    let runtime = RecordingRuleRuntime::new(configuration()).expect("规则配置应有效");
    let input = transaction("api.blocked.example", "https");
    assert_eq!(runtime.decision(&input), RecordingRuleAction::Reject);

    runtime
        .replaceConfiguration(RecordingRuleConfiguration::default())
        .expect("默认配置应有效");
    assert_eq!(runtime.decision(&input), RecordingRuleAction::Record);
}

#[tokio::test]
async fn do_not_record_skips_transaction_while_reject_remains_visible() {
    let temporary_directory = tempfile::tempdir().expect("创建规则录制临时目录");
    let session = RecordingSession::new(RecordingConfiguration {
        recordingRules: configuration(),
        spillDirectory: temporary_directory.path().to_path_buf(),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("创建带规则的录制会话");

    let mut unrecorded = transaction("allowed.example", "https");
    unrecorded.clientProcessName = Some("other.exe".to_owned());
    assert!(
        session
            .beginTransaction(unrecorded)
            .await
            .expect("不录制规则判定应成功")
            .is_none(),
        "DoNotRecord 必须在分配事务 ID 前退出，避免产生空事务"
    );
    assert!(
        session
            .beginTransaction(transaction("api.blocked.example", "https"))
            .await
            .expect("拒绝规则判定应成功")
            .is_some(),
        "Reject 事务必须保留可见记录，便于定位被规则主动阻断的连接"
    );
}
