#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    collections::BTreeSet,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    thread::JoinHandle,
    time::Duration,
};

use serde_json::{Value, json};

const expectedTools: [&str; 62] = [
    "capture_auto_save_get",
    "capture_auto_save_now",
    "capture_auto_save_update",
    "capture_breakpoint_abort",
    "capture_breakpoint_continue",
    "capture_breakpoint_get_settings",
    "capture_breakpoint_list_suspended",
    "capture_breakpoint_update",
    "capture_client_package_get",
    "capture_config_get",
    "capture_config_update",
    "capture_export_har",
    "capture_mirror_get",
    "capture_mirror_update",
    "capture_plugin_list",
    "capture_plugin_set_enabled",
    "capture_port_forward_get",
    "capture_port_forward_update",
    "capture_protobuf_decode",
    "capture_protobuf_get",
    "capture_protobuf_update",
    "capture_protobuf_upload",
    "capture_recording_clear",
    "capture_recording_get",
    "capture_recording_update",
    "capture_service_get_snapshot",
    "capture_ui_get_context",
    "capture_service_start",
    "capture_service_stop",
    "capture_sessions_clear_finished",
    "capture_ssl_export_root",
    "capture_ssl_get",
    "capture_ssl_regenerate_root",
    "capture_ssl_update",
    "capture_tool_block_cookies_get",
    "capture_tool_block_cookies_update",
    "capture_tool_block_get",
    "capture_tool_block_update",
    "capture_tool_map_local_get",
    "capture_tool_map_local_update",
    "capture_tool_map_remote_get",
    "capture_tool_map_remote_update",
    "capture_tool_no_caching_get",
    "capture_tool_no_caching_update",
    "capture_tool_packet_filters_get",
    "capture_tool_packet_filters_update",
    "capture_tool_rewrite_get",
    "capture_tool_rewrite_update",
    "capture_tool_throttle_get",
    "capture_tool_throttle_update",
    "capture_tools_summary",
    "capture_transaction_repeat",
    "capture_transaction_repeat_advanced",
    "capture_transaction_repeat_edited",
    "capture_transaction_get",
    "capture_transaction_get_body",
    "capture_transaction_list",
    "capture_validate_get",
    "capture_validate_response",
    "capture_validate_update",
    "capture_reverse_proxy_get",
    "capture_reverse_proxy_update",
];

const collectionToken: &str = "recording-alpha:7&next=1";

/// 描述一次 stdio 工具调用应产生的控制请求及对应成功响应。
struct ExpectedControlRequest {
    method: &'static str,
    path: &'static str,
    responseBody: Value,
}

/// 启动真实 stdio 子进程并走通服务、录制、事务和正文读取，验证 MCP 不会裁剪或改写
/// 控制面的 revision envelope；stdout 只能承载 JSON-RPC，任一协议漂移都会令测试失败。
#[test]
fn stdioLifecycleCallsReachControlPlane() {
    let stoppedSnapshot = controlSnapshot(10, "stopped", None);
    let runningSnapshot = controlSnapshot(11, "running", Some("HTTP fixture listener unavailable"));
    let stoppedAgainSnapshot = controlSnapshot(12, "stopped", None);
    let recording = recordingResponse(12);
    let transactions = transactionPage(12, 25);
    let transaction = transactionDetail(12);
    let body = encodedBodyResponse(12);
    let (controlBase, fixture) = startControlFixture(vec![
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/snapshot",
            responseBody: stoppedSnapshot.clone(),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/service/start",
            responseBody: runningSnapshot.clone(),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/service/stop",
            responseBody: stoppedAgainSnapshot.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/recording",
            responseBody: recording.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/transactions?offset=0&limit=25&collectionToken=recording-alpha%3A7%26next%3D1",
            responseBody: transactions.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/transactions/transaction-alpha",
            responseBody: transaction.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/transactions/transaction-alpha/response/body",
            responseBody: body.clone(),
        },
    ]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_captureMcp"))
        .env("CAPTURE_LOCALE", "en")
        .env("CAPTURE_CONTROL_BASE", controlBase)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("应能启动 captureMcp 测试构建产物");

    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "capture-stdio-smoke",
                    "version": "1.0.0"
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "capture_service_get_snapshot",
                "arguments": {}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "capture_service_start",
                "arguments": {}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "capture_service_stop",
                "arguments": {}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "capture_recording_get",
                "arguments": {}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "capture_transaction_list",
                "arguments": {
                    "offset": 0,
                    "limit": 25,
                    "collectionToken": collectionToken
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "capture_transaction_get",
                "arguments": {
                    "transactionId": "transaction-alpha"
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "capture_transaction_get_body",
                "arguments": {
                    "transactionId": "transaction-alpha",
                    "side": "response"
                }
            }
        }),
    ];
    let mut stdin = child.stdin.take().expect("子进程必须提供 stdin 管道");
    for request in requests {
        writeln!(stdin, "{request}").expect("应能写入完整 JSON-RPC 消息");
    }
    drop(stdin);

    let output = child.wait_with_output().expect("应能等待 MCP 子进程退出");
    assert!(
        output.status.success(),
        "MCP 子进程异常退出：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout 必须是 UTF-8 JSON-RPC 流");
    let responses = stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("stdout 每行必须是合法 JSON-RPC"))
        .collect::<Vec<_>>();
    let initializeResponse = findResponse(&responses, 1);
    assert_eq!(
        initializeResponse["result"]["protocolVersion"],
        "2025-11-25"
    );
    assert_eq!(
        initializeResponse["result"]["serverInfo"]["name"],
        "capture"
    );

    let toolListResponse = findResponse(&responses, 2);
    let actualTools = toolListResponse["result"]["tools"]
        .as_array()
        .expect("tools/list 必须返回工具数组")
        .iter()
        .map(|tool| {
            assert!(
                !tool["inputSchema"].to_string().contains("\"description\""),
                "参数 schema 不应导出未本地化的内部源码注释：{}",
                tool["name"]
            );
            tool["name"].as_str().expect("每个工具必须包含字符串名称")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(actualTools, expectedTools.into_iter().collect());
    let recordingUpdate = toolListResponse["result"]["tools"]
        .as_array()
        .expect("tools/list 必须返回工具数组")
        .iter()
        .find(|tool| tool["name"] == "capture_recording_update")
        .expect("录制更新工具必须存在");
    assert_eq!(
        recordingUpdate["annotations"]["destructiveHint"], false,
        "录制设置不再允许删除正文或淘汰事务，tool 注解必须保持非破坏性"
    );
    assert!(
        recordingUpdate["inputSchema"]
            .to_string()
            .find("limits")
            .is_none(),
        "MCP 录制更新不得暴露可重新启用正文裁剪或事务淘汰的 limits"
    );
    let configurationUpdate = toolListResponse["result"]["tools"]
        .as_array()
        .expect("tools/list 必须返回工具数组")
        .iter()
        .find(|tool| tool["name"] == "capture_config_update")
        .expect("配置更新工具必须存在");
    let configurationRequired = configurationUpdate["inputSchema"]["$defs"]["ConfigurationPayload"]
        ["required"]
        .as_array()
        .expect("配置更新 schema 必须声明 ConfigurationPayload.required");
    assert!(
        configurationRequired
            .iter()
            .any(|field| field == "httpProxy"),
        "MCP 配置更新必须要求完整 httpProxy 对象"
    );
    assert!(
        configurationRequired
            .iter()
            .any(|field| field == "upstreamProxy"),
        "MCP 配置更新必须要求完整 upstreamProxy 对象"
    );
    assert!(
        configurationRequired
            .iter()
            .any(|field| field == "processCapture"),
        "MCP 配置更新必须要求完整 processCapture 对象"
    );
    assert_eq!(
        configurationUpdate["inputSchema"]["$defs"]["ProcessIdPayload"]["minimum"], 1,
        "MCP schema 必须在控制请求发出前拒绝 PID 0"
    );

    assertStructuredContent(findResponse(&responses, 3), &stoppedSnapshot);
    assertStructuredContent(findResponse(&responses, 4), &runningSnapshot);
    assertStructuredContent(findResponse(&responses, 5), &stoppedAgainSnapshot);
    assertStructuredContent(findResponse(&responses, 6), &recording);
    assertStructuredContent(findResponse(&responses, 7), &transactions);
    assertStructuredContent(findResponse(&responses, 8), &transaction);
    assertStructuredContent(findResponse(&responses, 9), &body);

    let runningContent = &findResponse(&responses, 4)["result"]["structuredContent"];
    assert_eq!(runningContent["revision"], 11);
    assert_eq!(runningContent["listeners"]["httpProxy"]["state"], "failed");
    assert_eq!(
        runningContent["listeners"]["httpProxy"]["error"]["messageKey"],
        "error.httpProxyListenerFailed"
    );
    assert_eq!(
        runningContent["configuration"]["httpProxy"]["listenPort"],
        8888
    );
    assert_eq!(
        findResponse(&responses, 7)["result"]["structuredContent"]["recordingSessionId"],
        "recording-alpha"
    );
    assert_eq!(
        findResponse(&responses, 7)["result"]["structuredContent"]["collectionToken"],
        collectionToken
    );
    assert_eq!(
        findResponse(&responses, 8)["result"]["structuredContent"]["revision"],
        12
    );
    assert_eq!(
        findResponse(&responses, 9)["result"]["structuredContent"]["base64"],
        "b2s="
    );
    fixture.join().expect("MCP 控制 fixture 线程失败");
}

/// 构造完整 ControlSnapshot golden；监听失败仅影响 HTTP 监听状态，SOCKS 仍可令服务进入 running。
fn controlSnapshot(revision: u64, serviceState: &str, httpError: Option<&str>) -> Value {
    let running = serviceState == "running";
    json!({
        "revision": revision,
        "serviceState": serviceState,
        "metrics": {
            "acceptedConnections": 3,
            "activeConnections": 0,
            "failedConnections": 1,
            "bytesUp": 512,
            "bytesDown": 1024,
            "udpPacketsUp": 2,
            "udpPacketsDown": 2,
            "droppedUdpPackets": 0
        },
        "sessions": [],
        "configuration": {
            "listenHost": "127.0.0.1",
            "listenPort": 1080,
            "authenticationMode": "none",
            "authenticationUsernames": [],
            "maxConnections": 1024,
            "connectTimeout": 10.0,
            "bindTimeout": 30.0,
            "idleTimeout": 300.0,
            "shutdownTimeout": 10.0,
            "readTimeout": 30.0,
            "relayBufferSize": 65536,
            "udpBindHost": "127.0.0.1",
            "udpMaxPacketSize": 65507,
            "httpProxy": {
                "enabled": true,
                "listenHost": "127.0.0.1",
                "listenPort": 8888,
                "maxConnections": 512,
                "maxHeaderBytes": 65536,
                "maxCaptureBodyBytes": 262144,
                "connectTimeoutMilliseconds": 10000,
                "requestTimeoutMilliseconds": 60000,
                "headerReadTimeoutMilliseconds": 15000,
                "shutdownTimeoutMilliseconds": 5000
            }
        },
        "listeners": {
            "socks5": {
                "enabled": true,
                "state": if running { "running" } else { "stopped" },
                "boundEndpoint": running.then_some("127.0.0.1:1080"),
                "error": null
            },
            "httpProxy": {
                "enabled": true,
                "state": if running && httpError.is_some() {
                    "failed"
                } else if running {
                    "running"
                } else {
                    "stopped"
                },
                "boundEndpoint": if running && httpError.is_none() {
                    Some("127.0.0.1:8888")
                } else {
                    None
                },
                "error": httpError.map(|_| json!({
                    "code": "httpProxyFixtureFailure",
                    "messageKey": "error.httpProxyListenerFailed",
                    "params": {
                        "reasonCode": "httpProxyFixtureFailure"
                    }
                }))
            }
        },
        "recording": recordingSnapshot(),
        "transactions": transactionPage(revision, 500)
    })
}

/// 构造完整 RecordingSnapshot golden；正文预算与事务正文元信息保持可核对的一致总数。
fn recordingSnapshot() -> Value {
    json!({
        "recordingSessionId": "recording-alpha",
        "state": "recording",
        "startedAtMilliseconds": 1720000000000_u64,
        "transactionCount": 1,
        "droppedCount": 0,
        "totalBodyBytes": 6,
        "totalMetadataBytes": 512,
        "metadataMemoryBudgetBytes": 67108864,
        "pendingCleanupCount": 0,
        "limits": {
            "maxTransactions": 10000,
            "maxBodyBytes": 4194304,
            "maxTotalBodyBytes": 268435456
        },
        "ignoreLocations": [{
            "protocol": "https",
            "host": "*.ignored.test",
            "port": "*",
            "path": "*",
            "query": null
        }],
        "recordTunnelMetadata": true
    })
}

/// 构造完整 TransactionSummary golden；列表模型刻意不放请求头、响应头和正文字节。
fn transactionSummary() -> Value {
    json!({
        "transactionId": "transaction-alpha",
        "recordingSessionId": "recording-alpha",
        "sequence": 1,
        "protocol": "http",
        "method": "GET",
        "host": "alpha.example",
        "port": 80,
        "path": "/resource",
        "query": "page=1",
        "urlDisplay": "http://alpha.example/resource?page=1",
        "status": "complete",
        "statusCode": 200,
        "clientAddress": "127.0.0.1:50100",
        "clientProcessName": null,
        "clientProcessId": null,
        "contentType": "text/plain",
        "timings": {
            "startAtMilliseconds": 1720000000000_u64,
            "dnsEndAtMilliseconds": null,
            "connectEndAtMilliseconds": 1720000000010_u64,
            "tlsEndAtMilliseconds": null,
            "requestSentAtMilliseconds": 1720000000020_u64,
            "responseStartAtMilliseconds": 1720000000030_u64,
            "endAtMilliseconds": 1720000000040_u64
        },
        "sizes": {
            "requestHeaderBytes": 96,
            "requestBodyBytes": 4,
            "responseHeaderBytes": 128,
            "responseBodyBytes": 2
        },
        "flags": {
            "mappedLocal": false,
            "mappedRemote": false,
            "rewritten": false,
            "breakpointHit": false,
            "throttled": false,
            "mitmDecrypted": false,
            "bodyTruncated": false,
            "headersTruncated": false,
            "fromCache": false
        },
        "error": null,
        "notes": "",
        "tags": [],
        "appliedTools": []
    })
}

/// 构造完整 TransactionPage golden；调用方显式提供 limit 以区分快照窗口与列表查询窗口。
fn transactionPage(revision: u64, limit: usize) -> Value {
    json!({
        "revision": revision,
        "collectionToken": collectionToken,
        "recordingSessionId": "recording-alpha",
        "total": 1,
        "offset": 0,
        "limit": limit,
        "hasPrevious": false,
        "hasMore": false,
        "nextOffset": null,
        "truncated": false,
        "itemsTruncated": false,
        "items": [transactionSummary()]
    })
}

/// 构造带全局 revision 的 RecordingResponse golden，避免把内部 RecordingSnapshot 误当顶层响应。
fn recordingResponse(revision: u64) -> Value {
    json!({
        "revision": revision,
        "recording": recordingSnapshot()
    })
}

/// 构造事务详情 golden；头字段保留顺序，正文只返回元信息而不内联字节。
fn transactionDetail(revision: u64) -> Value {
    json!({
        "revision": revision,
        "transaction": transactionSummary(),
        "requestHeaders": [
            { "name": "host", "value": "alpha.example" },
            { "name": "content-length", "value": "4" }
        ],
        "responseHeaders": [
            { "name": "content-type", "value": "text/plain" },
            { "name": "content-length", "value": "2" }
        ],
        "requestBody": bodyMeta("request", 4),
        "responseBody": bodyMeta("response", 2)
    })
}

/// 构造一侧正文元信息；storedBytes 与 originalBytes 相同表示 golden 正文未截断。
fn bodyMeta(side: &str, byteCount: usize) -> Value {
    json!({
        "transactionId": "transaction-alpha",
        "side": side,
        "contentType": "text/plain",
        "encoding": "identity",
        "storedBytes": byteCount,
        "originalBytes": byteCount,
        "truncated": false
    })
}

/// 构造完整 EncodedBodyResponse golden；短 base64 足以验证 envelope 且不会扩大 stdio 输出。
fn encodedBodyResponse(revision: u64) -> Value {
    json!({
        "revision": revision,
        "meta": bodyMeta("response", 2),
        "base64": "b2s="
    })
}

/// 按 JSON-RPC 请求编号查找响应，避免依赖服务端通知与响应的具体到达顺序；
/// `responses` 是已完成语法校验的 stdout 消息，缺少指定响应时直接令测试失败。
fn findResponse(responses: &[Value], responseId: u64) -> &Value {
    responses
        .iter()
        .find(|response| response["id"].as_u64() == Some(responseId))
        .unwrap_or_else(|| panic!("缺少 JSON-RPC 响应 id={responseId}"))
}

/// 断言一次真实 tools/call 完整保留 golden JSON；文本 content 不作为权威协议来源。
fn assertStructuredContent(response: &Value, expectedContent: &Value) {
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        &response["result"]["structuredContent"], expectedContent,
        "MCP structuredContent 与控制面 golden envelope 不一致"
    );
}

/// 启动逐请求关闭连接的控制面 fixture；线程严格校验 stdio MCP 发出的 HTTP 方法与路径。
fn startControlFixture(
    mut expectedRequests: Vec<ExpectedControlRequest>,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 stdio 控制 fixture");
    let address = listener.local_addr().expect("读取 stdio 控制 fixture 地址");
    let task = std::thread::spawn(move || {
        while !expectedRequests.is_empty() {
            let (mut stream, _) = listener.accept().expect("接受 stdio 控制请求");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("设置 stdio 控制 fixture 读取超时");
            let request = readHttpRequest(&mut stream);
            let requestLine = request.lines().next().expect("控制请求必须包含请求行");
            let mut segments = requestLine.split_whitespace();
            let method = segments.next().expect("控制请求缺少方法");
            let path = segments.next().expect("控制请求缺少路径");
            let expectedIndex = expectedRequests
                .iter()
                .position(|expected| expected.method == method && expected.path == path)
                .unwrap_or_else(|| panic!("收到未声明的控制请求：{method} {path}"));
            let expected = expectedRequests.swap_remove(expectedIndex);
            let responseBody = expected.responseBody.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                responseBody.len(),
                responseBody
            )
            .expect("写入 stdio 控制 fixture 响应");
        }
    });
    (format!("http://{address}"), task)
}

/// 读取一个完整 HTTP/1.1 控制请求；正文只按 Content-Length 定界，避免等待连接关闭。
fn readHttpRequest(stream: &mut TcpStream) -> String {
    let mut requestBytes = Vec::new();
    let mut buffer = [0_u8; 1_024];
    loop {
        let byteCount = stream.read(&mut buffer).expect("读取 stdio 控制请求");
        assert!(byteCount > 0, "stdio 控制请求提前结束");
        requestBytes.extend_from_slice(&buffer[..byteCount]);
        let Some(headerEnd) = requestBytes
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
        else {
            continue;
        };
        let headerLength = headerEnd + 4;
        let headers = String::from_utf8_lossy(&requestBytes[..headerEnd]);
        let contentLength = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("解析 Content-Length"))
                })
            })
            .unwrap_or(0);
        if requestBytes.len() >= headerLength + contentLength {
            requestBytes.truncate(headerLength + contentLength);
            return String::from_utf8(requestBytes).expect("控制请求必须是 UTF-8");
        }
    }
}
