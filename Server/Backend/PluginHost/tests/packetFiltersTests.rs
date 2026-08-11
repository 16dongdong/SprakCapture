use plugin_host::{
    ConnectionMetadata, DataPlaneActionResult, PacketFilterAction, PacketFilterConfiguration,
    PacketFilterDirection, PacketFilterError, PacketFilterResult, PacketFilterRule,
    PacketFilterRuntime, PacketFilterTransport, PluginHost, StreamDirection, TransportKind,
};

/// 创建测试规则并显式填写全部稳定字段，避免 Default 掩盖序列化契约变化。
fn rule(id: &str, action: PacketFilterAction) -> PacketFilterRule {
    PacketFilterRule {
        id: id.to_owned(),
        name: format!("规则 {id}"),
        enabled: true,
        transport: PacketFilterTransport::Tcp,
        direction: PacketFilterDirection::Up,
        host: "*.example.com".to_owned(),
        port: Some(443),
        minimumLength: Some(4),
        maximumLength: Some(64),
        pattern: "01 ?? 03 04".to_owned(),
        replacement: if action == PacketFilterAction::Modify {
            "AA ?? BB CC".to_owned()
        } else {
            String::new()
        },
        action,
        replaceAll: false,
        continueMatching: false,
    }
}

/// 创建与规则匹配的 TCP 连接元数据；目标主机大小写用于覆盖规范化路径。
fn connection() -> ConnectionMetadata {
    ConnectionMetadata {
        transport: TransportKind::Tcp,
        clientAddress: "127.0.0.1:45000".to_owned(),
        targetHost: "API.Example.com.".to_owned(),
        targetPort: 443,
    }
}

#[test]
fn modifies_matching_bytes_and_preserves_wildcard_positions() {
    let runtime = PacketFilterRuntime::new(PacketFilterConfiguration {
        enabled: true,
        rules: vec![rule("modify", PacketFilterAction::Modify)],
    })
    .expect("有效规则应完成编译");

    let result = runtime.process(
        &connection(),
        StreamDirection::ClientToServer,
        vec![0, 1, 2, 3, 4, 5],
    );

    assert_eq!(
        result,
        PacketFilterResult::Forward {
            bytes: vec![0, 0xAA, 2, 0xBB, 0xCC, 5]
        }
    );
}

#[test]
fn applies_rules_in_order_and_hot_replacement_is_immediate() {
    let mut first = rule("first", PacketFilterAction::Modify);
    first.continueMatching = true;
    let mut second = rule("second", PacketFilterAction::Drop);
    second.pattern = "AA ?? BB CC".to_owned();
    let runtime = PacketFilterRuntime::new(PacketFilterConfiguration {
        enabled: true,
        rules: vec![first, second],
    })
    .expect("有序规则应完成编译");

    assert_eq!(
        runtime.process(
            &connection(),
            StreamDirection::ClientToServer,
            vec![1, 2, 3, 4]
        ),
        PacketFilterResult::Drop
    );

    runtime
        .replaceConfiguration(PacketFilterConfiguration {
            enabled: true,
            rules: vec![rule("close", PacketFilterAction::Close)],
        })
        .expect("热更新应原子替换完整快照");
    assert_eq!(
        runtime.process(
            &connection(),
            StreamDirection::ClientToServer,
            vec![1, 2, 3, 4]
        ),
        PacketFilterResult::Close
    );
}

#[test]
fn ignores_metadata_mismatches_and_disabled_configuration() {
    let runtime = PacketFilterRuntime::new(PacketFilterConfiguration {
        enabled: false,
        rules: vec![rule("drop", PacketFilterAction::Drop)],
    })
    .expect("禁用配置仍应校验成功");
    let original = vec![1, 2, 3, 4];
    assert_eq!(
        runtime.process(
            &connection(),
            StreamDirection::ClientToServer,
            original.clone()
        ),
        PacketFilterResult::Forward {
            bytes: original.clone()
        }
    );

    runtime
        .replaceConfiguration(PacketFilterConfiguration {
            enabled: true,
            rules: vec![rule("drop", PacketFilterAction::Drop)],
        })
        .expect("启用配置应替换成功");
    assert_eq!(
        runtime.process(
            &connection(),
            StreamDirection::ServerToClient,
            original.clone()
        ),
        PacketFilterResult::Forward { bytes: original }
    );
}

#[test]
fn rejects_invalid_replacement_and_duplicate_identifiers() {
    let mut invalid_replacement = rule("modify", PacketFilterAction::Modify);
    invalid_replacement.replacement = "AA GG".to_owned();
    assert!(matches!(
        PacketFilterRuntime::new(PacketFilterConfiguration {
            enabled: true,
            rules: vec![invalid_replacement]
        }),
        Err(PacketFilterError::InvalidReplacement)
    ));

    let duplicate = rule("same", PacketFilterAction::Drop);
    assert!(matches!(
        PacketFilterRuntime::new(PacketFilterConfiguration {
            enabled: true,
            rules: vec![duplicate.clone(), duplicate]
        }),
        Err(PacketFilterError::DuplicateIdentifier)
    ));
}

#[test]
fn enforces_the_512_byte_search_and_replacement_limit() {
    let maximum_pattern = vec!["AA"; 512].join(" ");
    let oversized_pattern = vec!["AA"; 513].join(" ");

    let mut valid = rule("maximum", PacketFilterAction::Modify);
    valid.pattern = maximum_pattern.clone();
    valid.replacement = maximum_pattern;
    PacketFilterRuntime::new(PacketFilterConfiguration {
        enabled: true,
        rules: vec![valid],
    })
    .expect("搜索与替换恰好 512 字节时应完成编译");

    let mut invalid_pattern = rule("pattern", PacketFilterAction::Drop);
    invalid_pattern.pattern = oversized_pattern.clone();
    assert!(matches!(
        PacketFilterRuntime::new(PacketFilterConfiguration {
            enabled: true,
            rules: vec![invalid_pattern]
        }),
        Err(PacketFilterError::InvalidPattern)
    ));

    let mut invalid_replacement = rule("replacement", PacketFilterAction::Modify);
    invalid_replacement.pattern = "AA".to_owned();
    invalid_replacement.replacement = oversized_pattern;
    assert!(matches!(
        PacketFilterRuntime::new(PacketFilterConfiguration {
            enabled: true,
            rules: vec![invalid_replacement]
        }),
        Err(PacketFilterError::InvalidReplacement)
    ));
}

#[test]
fn expands_sparse_search_to_the_variable_replacement_width() {
    let mut variable = rule("variable", PacketFilterAction::Modify);
    variable.pattern = "01 00 ?? 03 00".to_owned();
    variable.replacement = "01 00 06 03 00 03 03".to_owned();
    let runtime = PacketFilterRuntime::new(PacketFilterConfiguration {
        enabled: true,
        rules: vec![variable],
    })
    .expect("独立长度的搜索与替换应完成编译");

    assert_eq!(
        runtime.process(
            &connection(),
            StreamDirection::ClientToServer,
            vec![0x01, 0x00, 0x05, 0x03, 0x00, 0x01, 0x01]
        ),
        PacketFilterResult::Forward {
            bytes: vec![0x01, 0x00, 0x06, 0x03, 0x00, 0x03, 0x03]
        }
    );
}

#[test]
fn shortens_each_non_overlapping_match_without_shifting_later_offsets() {
    let mut variable = rule("shorten", PacketFilterAction::Modify);
    variable.pattern = "01 ?? 03 04".to_owned();
    variable.replacement = "AA BB".to_owned();
    variable.replaceAll = true;
    let runtime = PacketFilterRuntime::new(PacketFilterConfiguration {
        enabled: true,
        rules: vec![variable],
    })
    .expect("较短替换应完成编译");

    assert_eq!(
        runtime.process(
            &connection(),
            StreamDirection::ClientToServer,
            vec![0x01, 0x02, 0x03, 0x04, 0xFF, 0x01, 0x09, 0x03, 0x04]
        ),
        PacketFilterResult::Forward {
            bytes: vec![0xAA, 0xBB, 0xFF, 0xAA, 0xBB]
        }
    );
}

/// 验证滤镜位于完整插件数据面之后，并且无需重建连接即可读取最新规则快照。
#[tokio::test]
async fn plugin_data_plane_applies_current_packet_filter_snapshot() {
    let host = PluginHost::disabled();
    host.packetFilters()
        .replaceConfiguration(PacketFilterConfiguration {
            enabled: true,
            rules: vec![rule("modify", PacketFilterAction::Modify)],
        })
        .expect("封包滤镜配置应热更新成功");
    let metadata = connection();
    let connection = host.openConnection(metadata.clone());

    assert_eq!(
        host.processDataPlaneBytes(
            &connection,
            StreamDirection::ClientToServer,
            vec![1, 2, 3, 4]
        )
        .await,
        DataPlaneActionResult::Forward {
            bytes: vec![0xAA, 2, 0xBB, 0xCC]
        }
    );
    assert_eq!(
        host.processFinalWireBytes(&metadata, StreamDirection::ClientToServer, vec![1, 2, 3, 4]),
        DataPlaneActionResult::Forward {
            bytes: vec![0xAA, 2, 0xBB, 0xCC]
        },
        "代理连接与 WinDivert 适配器必须汇合到同一个最终写线入口"
    );

    host.packetFilters()
        .replaceConfiguration(PacketFilterConfiguration::default())
        .expect("关闭滤镜应原子替换运行态快照");
    assert_eq!(
        host.processDataPlaneBytes(
            &connection,
            StreamDirection::ClientToServer,
            vec![1, 2, 3, 4]
        )
        .await,
        DataPlaneActionResult::Forward {
            bytes: vec![1, 2, 3, 4]
        }
    );
    host.closeConnection(connection);
}
