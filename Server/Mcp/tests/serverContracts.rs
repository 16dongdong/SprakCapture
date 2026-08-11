#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread::JoinHandle,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as base64Standard};
use serde_json::{Value, json};

const expectedTools: [&str; 60] = [
    "capture_auto_save_get",
    "capture_auto_save_now",
    "capture_auto_save_update",
    "capture_breakpoint_abort",
    "capture_breakpoint_continue",
    "capture_breakpoint_get_settings",
    "capture_breakpoint_list_suspended",
    "capture_breakpoint_update",
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

/// 描述一次经过真实 stdio MCP 服务发出的控制请求，fixture 按顺序校验方法、路径、语言和正文。
struct ExpectedControlRequest {
    method: &'static str,
    path: &'static str,
    locale: &'static str,
    body: RequestBodyExpectation,
    responseStatus: u16,
    responseContentType: &'static str,
    responseBody: Value,
}

/// 限定 fixture 对请求正文的期望，所有检查均发生在 MCP 的公开 stdio 接口之后。
enum RequestBodyExpectation {
    Empty,
    Json(Value),
}

/// 封装真实 MCP 二进制的 JSON-RPC stdio 会话；测试只通过生产传输协议调用，不依赖服务内部路由或测试分支。
struct McpStdioSession {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
    nextRequestId: u64,
}

impl McpStdioSession {
    /// 启动生产 MCP 二进制并完成 initialize 握手，控制地址和启动语言都经进程环境传递。
    fn start(controlBase: &str, locale: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_captureMcp"))
            .env("CAPTURE_CONTROL_BASE", controlBase)
            .env("CAPTURE_LOCALE", locale)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("启动 captureMcp 测试构建产物失败");
        let input = child.stdin.take().expect("MCP 子进程缺少 stdin 管道");
        let output = BufReader::new(child.stdout.take().expect("MCP 子进程缺少 stdout 管道"));
        let mut session = Self {
            child,
            input: Some(input),
            output: Some(output),
            nextRequestId: 1,
        };
        let initialize = session.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "stdio-contract-tests", "version": "1.0.0" }
            }),
        );
        assert_eq!(initialize["result"]["serverInfo"]["name"], "capture");
        session.notification("notifications/initialized", json!({}));
        session
    }

    /// 发送 JSON-RPC 请求并读取具有相同 id 的响应；通知不会打乱请求响应对应关系。
    fn request(&mut self, method: &str, params: Value) -> Value {
        let requestId = self.nextRequestId;
        self.nextRequestId += 1;
        let message = json!({
            "jsonrpc": "2.0",
            "id": requestId,
            "method": method,
            "params": params,
        });
        let input = self.input.as_mut().expect("MCP stdin 已关闭");
        writeln!(input, "{message}").expect("写入 MCP JSON-RPC 请求失败");
        input.flush().expect("刷新 MCP JSON-RPC 请求失败");

        loop {
            let mut line = String::new();
            let byteCount = self
                .output
                .as_mut()
                .expect("MCP stdout 已关闭")
                .read_line(&mut line)
                .expect("读取 MCP JSON-RPC 响应失败");
            assert!(byteCount > 0, "MCP 子进程在返回响应前结束");
            let response: Value =
                serde_json::from_str(&line).expect("MCP stdout 必须逐行输出 JSON-RPC");
            if response["id"].as_u64() == Some(requestId) {
                return response;
            }
        }
    }

    /// 发送不要求响应的 JSON-RPC 通知，初始化完成后才允许工具调用。
    fn notification(&mut self, method: &str, params: Value) {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let input = self.input.as_mut().expect("MCP stdin 已关闭");
        writeln!(input, "{message}").expect("写入 MCP JSON-RPC 通知失败");
        input.flush().expect("刷新 MCP JSON-RPC 通知失败");
    }

    /// 通过 tools/call 调用一个正式 MCP 工具并返回 JSON-RPC result，错误工具结果仍保留在 result 内。
    fn callTool(&mut self, toolName: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": toolName, "arguments": arguments }),
        );
        assert!(
            response.get("error").is_none(),
            "工具调用不应产生 JSON-RPC 错误：{response}"
        );
        response["result"].clone()
    }

    /// 关闭 stdio 写端并等待生产二进制正常退出，避免测试遗留后台 MCP 进程。
    fn finish(mut self) {
        drop(self.input.take());
        drop(self.output.take());
        let status = self.child.wait().expect("等待 MCP 子进程退出失败");
        assert!(status.success(), "MCP 子进程异常退出：{status}");
    }
}

/// 验证工具目录经生产 stdio 传输公开完整且已按启动语言本地化。
#[test]
fn toolCatalogUsesStdioProductionInterface() {
    let mut session = McpStdioSession::start("http://127.0.0.1:17890", "ja");
    let response = session.request("tools/list", json!({}));
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools/list 必须返回工具数组");
    let actualTools = tools
        .iter()
        .map(|tool| {
            let description = tool["description"]
                .as_str()
                .expect("工具必须具有已本地化描述");
            assert!(!description.starts_with("mcp."));
            tool["name"].as_str().expect("工具必须具有名称")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(actualTools, expectedTools.into_iter().collect());
    session.finish();
}

/// 汇集 M3 工具测试需要的独立配置和可比较结果，避免每个协议断言复制大型 JSON fixture。
struct M3FixtureValues {
    mapLocalConfiguration: Value,
    mapRemoteConfiguration: Value,
    throttlingConfiguration: Value,
    throttlingState: Value,
    breakpointDraft: Value,
    harArchive: Value,
    expectedHarBase64: String,
}

/// 验证 M3 映射、节流、断点和 HAR 工具均经真实 stdio 入口保持与控制面的请求契约一致。
#[test]
fn m3ToolsUseStableStdioControlContracts() {
    let values = createM3FixtureValues();
    let (controlBase, fixture) = startControlFixture(m3ExpectedRequests(&values));
    let mut session = McpStdioSession::start(&controlBase, "en");

    assert_eq!(
        structuredContent(
            session.callTool("capture_tool_map_local_get", json!({ "locale": "zh-CN" }))
        ),
        values.mapLocalConfiguration
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_tool_map_local_update",
            json!({ "locale": "ja-JP", "configuration": values.mapLocalConfiguration.clone() })
        ))["enabled"],
        true
    );
    assert_eq!(
        structuredContent(
            session.callTool("capture_tool_map_remote_get", json!({ "locale": "de-DE" }))
        ),
        values.mapRemoteConfiguration
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_tool_map_remote_update",
            json!({ "locale": "fr-FR", "configuration": values.mapRemoteConfiguration.clone() })
        ))["enabled"],
        true
    );
    assert_eq!(
        structuredContent(
            session.callTool("capture_tool_throttle_get", json!({ "locale": "ko-KR" }))
        )["presets"][0]["id"],
        "fixture"
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_tool_throttle_update",
            json!({ "locale": "pt-PT", "configuration": values.throttlingConfiguration.clone() })
        ))["custom"]["downloadBytesPerSecond"],
        4096
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_breakpoint_list_suspended",
            json!({ "locale": "ru-RU" })
        ))[0]["transactionId"],
        "transaction/alpha"
    );
    assert_eq!(
        structuredContent(session.callTool("capture_breakpoint_continue", json!({ "locale": "zh-TW", "transactionId": "transaction/alpha", "draft": values.breakpointDraft.clone() })))["completed"],
        true
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_breakpoint_abort",
            json!({ "locale": "en", "transactionId": "transaction/beta" })
        ))["completed"],
        true
    );
    let fullExport = structuredContent(session.callTool(
        "capture_export_har",
        json!({ "locale": "zh-CN", "includeBodies": false }),
    ));
    assert_eq!(fullExport["format"], "har");
    assert_eq!(fullExport["base64"], values.expectedHarBase64);
    assert_eq!(
        structuredContent(session.callTool("capture_export_har", json!({ "locale": "fr-FR", "includeBodies": true, "transactionIds": ["transaction/alpha", "transaction-beta"] })))["base64"],
        values.expectedHarBase64
    );
    session.finish();
    fixture.join().expect("M3 控制 fixture 线程失败");
}

/// 构造 M3 断言使用的规则、节流和导出样本；所有字段均沿用公开控制协议的 wire name。
fn createM3FixtureValues() -> M3FixtureValues {
    let mapLocalConfiguration = json!({
        "enabled": true,
        "rules": [{
            "id": "local-rule",
            "enabled": true,
            "location": { "protocol": "http", "host": "local.example", "port": "80", "path": "/asset.json", "query": null },
            "localPath": "fixtures/asset.json",
            "isDirectory": false,
            "statusCode": 200,
            "responseHeaders": [{ "name": "x-local", "value": "fixture" }],
            "contentTypeOverride": "application/json"
        }]
    });
    let mapRemoteConfiguration = json!({
        "enabled": true,
        "rules": [{
            "id": "remote-rule",
            "enabled": true,
            "from": { "protocol": "http", "host": "origin.example", "port": "80", "path": "/v1/*", "query": null },
            "to": { "protocol": "http", "host": "127.0.0.1", "port": "18080", "path": "/v2/*" }
        }]
    });
    let throttlingConfiguration = json!({
        "enabled": true,
        "activePresetId": null,
        "custom": { "downloadBytesPerSecond": 4096, "uploadBytesPerSecond": 2048, "latencyMilliseconds": 15, "latencyJitterMilliseconds": 0, "reliabilityPercent": 100, "mtu": 512 },
        "locations": [],
        "userPresets": []
    });
    let throttlingState = json!({
        "enabled": true,
        "activePresetId": null,
        "custom": throttlingConfiguration["custom"].clone(),
        "locations": [],
        "presets": [{ "id": "fixture", "name": "Fixture", "downloadBytesPerSecond": 4096, "uploadBytesPerSecond": 2048, "latencyMilliseconds": 15, "latencyJitterMilliseconds": 0, "reliabilityPercent": 100, "mtu": 512 }]
    });
    let breakpointDraft = json!({
        "method": "POST",
        "url": "http://origin.example/edited",
        "statusCode": null,
        "reason": null,
        "headers": [{ "name": "content-type", "value": "application/json" }],
        "bodyBase64": "eyJvayI6dHJ1ZX0="
    });
    let harArchive = json!({ "log": { "version": "1.2", "creator": { "name": "fixture", "version": "1.0" }, "entries": [] } });
    let expectedHarBase64 = base64Standard.encode(harArchive.to_string());
    M3FixtureValues {
        mapLocalConfiguration,
        mapRemoteConfiguration,
        throttlingConfiguration,
        throttlingState,
        breakpointDraft,
        harArchive,
        expectedHarBase64,
    }
}

/// 根据 M3 fixture 值生成严格有序的控制调用清单，读取快照与写入正文都必须保持原协议边界。
fn m3ExpectedRequests(values: &M3FixtureValues) -> Vec<ExpectedControlRequest> {
    vec![
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/snapshot",
            locale: "zh-Hans",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "tools": { "mapLocal": values.mapLocalConfiguration.clone() } }),
        },
        ExpectedControlRequest {
            method: "PUT",
            path: "/api/v1/tools/mapLocal",
            locale: "ja",
            body: RequestBodyExpectation::Json(values.mapLocalConfiguration.clone()),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: values.mapLocalConfiguration.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/snapshot",
            locale: "de",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "tools": { "mapRemote": values.mapRemoteConfiguration.clone() } }),
        },
        ExpectedControlRequest {
            method: "PUT",
            path: "/api/v1/tools/mapRemote",
            locale: "fr",
            body: RequestBodyExpectation::Json(values.mapRemoteConfiguration.clone()),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: values.mapRemoteConfiguration.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/snapshot",
            locale: "ko",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "tools": { "throttling": values.throttlingState.clone() } }),
        },
        ExpectedControlRequest {
            method: "PUT",
            path: "/api/v1/tools/throttling",
            locale: "pt-BR",
            body: RequestBodyExpectation::Json(values.throttlingConfiguration.clone()),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: values.throttlingState.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/breakpoints/suspended",
            locale: "ru",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!([{ "transactionId": "transaction/alpha", "phase": "request" }]),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/breakpoints/suspended/transaction%2Falpha/continue",
            locale: "zh-Hant",
            body: RequestBodyExpectation::Json(values.breakpointDraft.clone()),
            responseStatus: 204,
            responseContentType: "application/json",
            responseBody: Value::Null,
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/breakpoints/suspended/transaction%2Fbeta/abort",
            locale: "en",
            body: RequestBodyExpectation::Empty,
            responseStatus: 204,
            responseContentType: "application/json",
            responseBody: Value::Null,
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/recording/export",
            locale: "zh-Hans",
            body: RequestBodyExpectation::Json(json!({ "format": "har", "includeBodies": false })),
            responseStatus: 200,
            responseContentType: "application/har+json",
            responseBody: values.harArchive.clone(),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/recording/export",
            locale: "fr",
            body: RequestBodyExpectation::Json(
                json!({ "format": "har", "includeBodies": true, "transactionIds": ["transaction/alpha", "transaction-beta"] }),
            ),
            responseStatus: 200,
            responseContentType: "application/har+json",
            responseBody: values.harArchive.clone(),
        },
    ]
}

/// 验证事务集合令牌和事务标识在真实 stdio 调用中保持正确编码，不改变控制路由的单段边界。
#[test]
fn transactionToolsPreserveEncodedControlPaths() {
    let collectionToken = "recording-1:7&next=1";
    let (controlBase, fixture) = startControlFixture(vec![
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/transactions?offset=10&limit=25&collectionToken=recording-1%3A7%26next%3D1",
            locale: "de",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "offset": 10, "collectionToken": collectionToken, "nextOffset": 35, "items": [] }),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/transactions/id%2Fwith%3Fdelimiter",
            locale: "fr",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "transaction": { "transactionId": "id/with?delimiter" } }),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/transactions/transaction-1/response/body",
            locale: "pt-BR",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "meta": { "side": "response" }, "base64": "b2s=" }),
        },
    ]);
    let mut session = McpStdioSession::start(&controlBase, "en");

    let page = structuredContent(session.callTool(
        "capture_transaction_list",
        json!({ "locale": "de-DE", "offset": 10, "limit": 25, "collectionToken": collectionToken }),
    ));
    assert_eq!(page["collectionToken"], collectionToken);
    assert_eq!(page["nextOffset"], 35);
    assert_eq!(
        structuredContent(session.callTool(
            "capture_transaction_get",
            json!({ "locale": "fr-FR", "transactionId": "id/with?delimiter" })
        ))["transaction"]["transactionId"],
        "id/with?delimiter"
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_transaction_get_body",
            json!({ "locale": "pt-PT", "transactionId": "transaction-1", "side": "response" })
        ))["meta"]["side"],
        "response"
    );
    session.finish();
    fixture.join().expect("事务控制 fixture 线程失败");
}

/// 验证控制面业务拒绝在真实 MCP 结果内保持唯一结构化错误，不透传原始控制响应正文。
#[test]
fn controlRejectionRemainsStructuredOverStdio() {
    let (controlBase, fixture) = startControlFixture(vec![ExpectedControlRequest {
        method: "POST",
        path: "/api/v1/service/start",
        locale: "zh-Hans",
        body: RequestBodyExpectation::Empty,
        responseStatus: 409,
        responseContentType: "application/json",
        responseBody: json!({
            "code": "serviceStateConflict",
            "message": "SOCKS5 服务不在可启动状态",
            "messageKey": "error.serviceStateConflict",
            "params": { "serviceState": "starting" }
        }),
    }]);
    let mut session = McpStdioSession::start(&controlBase, "en");
    let result = session.callTool("capture_service_start", json!({ "locale": "zh-Hans" }));
    assert_eq!(result["isError"], true);
    let error = &result["structuredContent"];
    assert_eq!(error["code"], "serviceStateConflict");
    assert_eq!(error["messageKey"], "error.serviceStateConflict");
    assert_eq!(error["params"]["serviceState"], "starting");
    assert_eq!(error["controlStatus"], 409);
    assert!(error.get("controlResponse").is_none());
    assert!(error.get("body").is_none());
    session.finish();
    fixture.join().expect("拒绝控制 fixture 线程失败");
}

/// 汇集基线和 SSL 工具测试的公共配置，保持请求 fixture 与工具调用共享同一份协议样本。
struct BaselineFixtureValues {
    configuration: Value,
    sslConfiguration: Value,
}

/// 验证服务、配置、会话和 SSL 工具通过 stdio 保持既有控制路由与二进制证书导出契约。
#[test]
fn baselineAndSslToolsUseStableStdioControlContracts() {
    let values = createBaselineFixtureValues();
    let (controlBase, fixture) = startControlFixture(baselineExpectedRequests(&values));
    let mut session = McpStdioSession::start(&controlBase, "en");

    assert_eq!(
        structuredContent(session.callTool("capture_service_get_snapshot", json!({})))["serviceState"],
        "stopped"
    );
    assert_eq!(
        structuredContent(session.callTool("capture_service_start", json!({ "locale": "zh-CN" })))
            ["serviceState"],
        "running"
    );
    assert_eq!(
        structuredContent(session.callTool("capture_service_stop", json!({ "locale": "ja-JP" })))["serviceState"],
        "stopped"
    );
    assert_eq!(
        structuredContent(session.callTool("capture_config_get", json!({}))),
        values.configuration
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_config_update",
            json!({ "locale": "de-DE", "configuration": values.configuration.clone() })
        ))["serviceState"],
        "stopped"
    );
    assert_eq!(
        structuredContent(
            session.callTool("capture_sessions_clear_finished", json!({ "locale": "pt" }))
        )["sessions"],
        json!([])
    );
    assert_eq!(
        structuredContent(session.callTool("capture_ssl_get", json!({})))["enabled"],
        true
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_ssl_update",
            json!({ "locale": "zh-CN", "ssl": values.sslConfiguration.clone() })
        ))["maxCachedCertificates"],
        128
    );
    let export = structuredContent(session.callTool(
        "capture_ssl_export_root",
        json!({ "locale": "ja-JP", "format": "pem" }),
    ));
    assert_eq!(export["format"], "pem");
    assert_eq!(export["fileName"], "root.pem");
    assert!(export["byteLength"].as_u64().unwrap_or_default() > 0);
    assert!(!export["base64"].as_str().unwrap_or_default().is_empty());
    assert_eq!(
        structuredContent(
            session.callTool("capture_ssl_regenerate_root", json!({ "locale": "de-DE" }))
        )["enabled"],
        true
    );
    session.finish();
    fixture.join().expect("基线与 SSL fixture 线程失败");
}

/// 构造基线服务配置和 SSL 匹配配置，覆盖 MCP schema 的完整配置更新字段。
fn createBaselineFixtureValues() -> BaselineFixtureValues {
    BaselineFixtureValues {
        configuration: json!({
            "listenHost": "127.0.0.1",
            "listenPort": 1080,
            "authenticationMode": "none",
            "maxConnections": 1024,
            "connectTimeout": 10.0,
            "bindTimeout": 30.0,
            "idleTimeout": 300.0,
            "shutdownTimeout": 5.0,
            "readTimeout": 10.0,
            "relayBufferSize": 65536,
            "udpBindHost": "",
            "udpMaxPacketSize": 65507,
            "credentials": null,
            "httpProxy": {
                "enabled": false,
                "listenHost": "127.0.0.1",
                "listenPort": 8888,
                "maxConnections": 512,
                "maxHeaderBytes": 65536,
                "maxCaptureBodyBytes": 262144,
                "connectTimeoutMilliseconds": 10000,
                "requestTimeoutMilliseconds": 60000,
                "headerReadTimeoutMilliseconds": 15000,
                "shutdownTimeoutMilliseconds": 5000
            },
            "upstreamProxy": {
                "enabled": true,
                "protocol": "http",
                "host": "upstream.example.test",
                "port": 3128,
                "username": "fixture-user",
                "password": null
            },
            "processCapture": {
                "enabled": true,
                "processIds": [1200, 3400],
                "proxyPort": 1080
            }
        }),
        sslConfiguration: json!({
            "enabled": true,
            "includeLocations": [{
                "protocol": "https",
                "host": "*.example.com",
                "port": "",
                "path": "",
                "query": null
            }],
            "excludeLocations": [],
            "maxCachedCertificates": 128,
            "useClientSni": true
        }),
    }
}

/// 基于共享配置生成严格有序的基线和 SSL 控制请求，序列化后的空 query 必须遵守模型的省略规则。
fn baselineExpectedRequests(values: &BaselineFixtureValues) -> Vec<ExpectedControlRequest> {
    let sslState = json!({
        "enabled": true,
        "includeLocations": values.sslConfiguration["includeLocations"].clone(),
        "excludeLocations": [],
        "maxCachedCertificates": 128,
        "useClientSni": true,
        "ca": { "installed": true, "subject": "CN=Local Root", "fingerprintSha256": "AA:BB" },
        "cachedLeafCount": 0
    });
    let snapshot =
        json!({ "serviceState": "stopped", "configuration": values.configuration.clone() });
    vec![
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/snapshot",
            locale: "en",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: snapshot.clone(),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/service/start",
            locale: "zh-Hans",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "serviceState": "running" }),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/service/stop",
            locale: "ja",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "serviceState": "stopped" }),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/snapshot",
            locale: "en",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: snapshot,
        },
        ExpectedControlRequest {
            method: "PUT",
            path: "/api/v1/configuration",
            locale: "de",
            body: RequestBodyExpectation::Json(values.configuration.clone()),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "serviceState": "stopped" }),
        },
        ExpectedControlRequest {
            method: "DELETE",
            path: "/api/v1/sessions",
            locale: "pt-BR",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "sessions": [] }),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/ssl",
            locale: "en",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: sslState.clone(),
        },
        ExpectedControlRequest {
            method: "PUT",
            path: "/api/v1/ssl",
            locale: "zh-Hans",
            body: RequestBodyExpectation::Json(json!({
                "enabled": true,
                "includeLocations": [{ "protocol": "https", "host": "*.example.com", "port": "", "path": "" }],
                "excludeLocations": [],
                "maxCachedCertificates": 128,
                "useClientSni": true
            })),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: sslState.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/ssl/ca/export?format=pem",
            locale: "ja",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/x-pem-file",
            responseBody: json!("ROOT CERTIFICATE"),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/ssl/ca/generate",
            locale: "de",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: sslState,
        },
    ]
}

/// 验证录制更新和清空经 stdio 保持控制正文的省略字段语义，非法事务标识在离开 MCP 前被拒绝。
#[test]
fn recordingLifecycleAndInvalidIdentifierUseStableStdioContracts() {
    let (controlBase, fixture) = startControlFixture(vec![
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/recording",
            locale: "en",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "revision": 1, "recording": { "state": "recording", "transactionCount": 1 } }),
        },
        ExpectedControlRequest {
            method: "PUT",
            path: "/api/v1/recording",
            locale: "zh-Hans",
            body: RequestBodyExpectation::Json(json!({ "state": "paused" })),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "revision": 2, "recording": { "state": "paused", "transactionCount": 1 } }),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/recording/clear",
            locale: "ja",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "revision": 3, "recording": { "state": "paused", "transactionCount": 0 } }),
        },
    ]);
    let mut session = McpStdioSession::start(&controlBase, "en");

    assert_eq!(
        structuredContent(session.callTool("capture_recording_get", json!({})))["recording"]["state"],
        "recording"
    );
    assert_eq!(
        structuredContent(session.callTool(
            "capture_recording_update",
            json!({
                "locale": "zh-CN",
                "recording": { "state": "paused" }
            })
        ))["recording"]["state"],
        "paused"
    );
    assert_eq!(
        structuredContent(
            session.callTool("capture_recording_clear", json!({ "locale": "ja-JP" }))
        )["recording"]["transactionCount"],
        0
    );
    let invalidIdentifier = session.callTool(
        "capture_transaction_get",
        json!({ "locale": "zh-CN", "transactionId": ".." }),
    );
    assert_eq!(invalidIdentifier["isError"], true);
    assert_eq!(
        invalidIdentifier["structuredContent"]["messageKey"],
        "mcp.error.invalidTransactionId"
    );
    session.finish();
    fixture.join().expect("录制控制 fixture 线程失败");
}

/// 验证无法解析的控制响应经过真实 stdio 转换为本地化结构化错误，原始正文不会进入工具结果。
#[test]
fn invalidControlResponseDoesNotLeakBodyOverStdio() {
    let (controlBase, fixture) = startControlFixture(vec![ExpectedControlRequest {
        method: "POST",
        path: "/api/v1/service/start",
        locale: "en",
        body: RequestBodyExpectation::Empty,
        responseStatus: 502,
        responseContentType: "text/html",
        responseBody: json!("unexpected upstream body"),
    }]);
    let mut session = McpStdioSession::start(&controlBase, "en");
    let result = session.callTool("capture_service_start", json!({}));
    assert_eq!(result["isError"], true);
    let error = &result["structuredContent"];
    assert_eq!(error["messageKey"], "mcp.error.invalidControlResponse");
    assert_eq!(error["params"]["statusCode"], 502);
    assert!(error["params"].get("body").is_none());
    assert!(error["params"].get("controlResponse").is_none());
    session.finish();
    fixture.join().expect("无效响应 fixture 线程失败");
}

/// 验证 M6 协议工具经生产 stdio 转发到唯一控制契约；W3C 路径仅透传明确的单次上传确认字段。
#[test]
fn protocolToolsUseStdioControlContracts() {
    let protobufConfiguration = json!({ "enabled": false, "schemas": [], "routes": [] });
    let protobufUpdate = json!({ "enabled": true, "routes": [] });
    let validateConfiguration = json!({
        "enabled": true,
        "validators": [
            { "id": "htmlWellFormed", "enabled": true },
            { "id": "jsonSchema", "enabled": false },
            { "id": "w3cHtmlOnline", "enabled": false }
        ],
        "allowOnlineValidators": false,
        "w3cEndpoint": "https://validator.w3.org/nu/?out=json"
    });
    let validateReport = json!({
        "transactionId": "transaction-alpha",
        "validatorId": "htmlWellFormed",
        "issues": [],
        "validatedAtMilliseconds": 1720000000000_u64
    });
    let (controlBase, fixture) = startControlFixture(vec![
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/tools/protobuf",
            locale: "zh-Hans",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: protobufConfiguration.clone(),
        },
        ExpectedControlRequest {
            method: "PUT",
            path: "/api/v1/tools/protobuf",
            locale: "ja",
            body: RequestBodyExpectation::Json(protobufUpdate.clone()),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: protobufConfiguration.clone(),
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/tools/protobuf/schemas",
            locale: "de",
            body: RequestBodyExpectation::Json(json!({
                "name": "fixture",
                "defaultMessageType": "fixture.Envelope",
                "base64": "AA=="
            })),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: protobufConfiguration.clone(),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/transactions/transaction-alpha/decode/protobuf?side=response",
            locale: "fr",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: json!({ "messageType": null, "json": null, "decodeError": "protobufDisabled" }),
        },
        ExpectedControlRequest {
            method: "GET",
            path: "/api/v1/tools/validate",
            locale: "pt-BR",
            body: RequestBodyExpectation::Empty,
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: validateConfiguration.clone(),
        },
        ExpectedControlRequest {
            method: "PUT",
            path: "/api/v1/tools/validate",
            locale: "ru",
            body: RequestBodyExpectation::Json(validateConfiguration.clone()),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: validateConfiguration,
        },
        ExpectedControlRequest {
            method: "POST",
            path: "/api/v1/transactions/transaction-alpha/validate",
            locale: "zh-Hant",
            body: RequestBodyExpectation::Json(json!({
                "validatorId": "htmlWellFormed",
                "onlineUploadConfirmed": false
            })),
            responseStatus: 200,
            responseContentType: "application/json",
            responseBody: validateReport,
        },
    ]);
    let mut session = McpStdioSession::start(&controlBase, "en");
    assert_eq!(
        structuredContent(session.callTool("capture_protobuf_get", json!({ "locale": "zh-CN" }))),
        protobufConfiguration
    );
    let _ = structuredContent(session.callTool(
        "capture_protobuf_update",
        json!({ "locale": "ja-JP", "configuration": protobufUpdate }),
    ));
    let _ = structuredContent(session.callTool(
        "capture_protobuf_upload",
        json!({
            "locale": "de-DE",
            "name": "fixture",
            "defaultMessageType": "fixture.Envelope",
            "base64": "AA=="
        }),
    ));
    assert_eq!(
        structuredContent(session.callTool(
            "capture_protobuf_decode",
            json!({ "locale": "fr-FR", "transactionId": "transaction-alpha", "side": "response" }),
        ))["decodeError"],
        "protobufDisabled"
    );
    let _ =
        structuredContent(session.callTool("capture_validate_get", json!({ "locale": "pt-BR" })));
    let _ = structuredContent(session.callTool(
        "capture_validate_update",
        json!({
            "locale": "ru-RU",
            "configuration": {
                "enabled": true,
                "validators": [
                    { "id": "htmlWellFormed", "enabled": true },
                    { "id": "jsonSchema", "enabled": false },
                    { "id": "w3cHtmlOnline", "enabled": false }
                ],
                "allowOnlineValidators": false,
                "w3cEndpoint": "https://validator.w3.org/nu/?out=json"
            }
        }),
    ));
    assert_eq!(
        structuredContent(session.callTool(
            "capture_validate_response",
            json!({
                "locale": "zh-Hant",
                "transactionId": "transaction-alpha",
                "validatorId": "htmlWellFormed",
                "onlineUploadConfirmed": false
            }),
        ))["validatorId"],
        "htmlWellFormed"
    );
    session.finish();
    fixture.join().expect("M6 协议控制 fixture 线程失败");
}

/// 提取成功工具结果中的 structuredContent；MCP tool error 使用 isError 标识且不会进入本函数。
fn structuredContent(result: Value) -> Value {
    assert_eq!(result["isError"], false, "工具调用应成功：{result}");
    result["structuredContent"].clone()
}

/// 启动顺序严格的控制 HTTP fixture，确保生产 MCP 二进制对每条控制调用的线缆契约均可复现。
fn startControlFixture(expectedRequests: Vec<ExpectedControlRequest>) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 MCP 控制 fixture 失败");
    let address = listener
        .local_addr()
        .expect("读取 MCP 控制 fixture 地址失败");
    let task = std::thread::spawn(move || {
        for expected in expectedRequests {
            let (mut stream, _) = listener.accept().expect("接收 MCP 控制请求失败");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("设置 MCP fixture 读取超时失败");
            let request = readHttpRequest(&mut stream);
            let headerEnd = request.find("\r\n\r\n").expect("MCP 请求缺少头部结束符");
            let (head, bodyWithSeparator) = request.split_at(headerEnd);
            let mut requestLine = head
                .lines()
                .next()
                .expect("MCP 请求缺少请求行")
                .split_whitespace();
            assert_eq!(requestLine.next(), Some(expected.method));
            assert_eq!(requestLine.next(), Some(expected.path));
            let acceptLanguage = head
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("accept-language")
                            .then(|| value.trim())
                    })
                })
                .expect("MCP 控制请求缺少 Accept-Language");
            assert_eq!(acceptLanguage, expected.locale);
            let body = &bodyWithSeparator[4..];
            match expected.body {
                RequestBodyExpectation::Empty => {
                    assert!(body.is_empty(), "请求不应携带正文：{}", expected.path)
                }
                RequestBodyExpectation::Json(expectedBody) => {
                    let actualBody: Value =
                        serde_json::from_str(body).expect("解析 MCP 控制请求 JSON 正文失败");
                    assert_eq!(actualBody, expectedBody);
                }
            }
            writeFixtureResponse(
                &mut stream,
                expected.responseStatus,
                expected.responseContentType,
                &expected.responseBody,
            );
        }
    });
    (format!("http://{address}"), task)
}

/// 读取带 Content-Length 的单个 HTTP 请求；fixture 只覆盖当前 reqwest 控制调用的确定性传输形式。
fn readHttpRequest(stream: &mut TcpStream) -> String {
    let mut requestBytes = Vec::new();
    let mut buffer = [0_u8; 1_024];
    loop {
        let byteCount = stream.read(&mut buffer).expect("读取 MCP 控制请求失败");
        assert!(byteCount > 0, "MCP 控制请求提前结束");
        requestBytes.extend_from_slice(&buffer[..byteCount]);
        let Some(headerEnd) = requestBytes
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
        else {
            continue;
        };
        let headerLength = headerEnd + 4;
        let head = String::from_utf8_lossy(&requestBytes[..headerEnd]);
        let contentLength = head
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length").then(|| {
                        value
                            .trim()
                            .parse::<usize>()
                            .expect("解析 Content-Length 失败")
                    })
                })
            })
            .unwrap_or(0);
        if requestBytes.len() >= headerLength + contentLength {
            requestBytes.truncate(headerLength + contentLength);
            return String::from_utf8(requestBytes).expect("MCP 控制请求必须是 UTF-8");
        }
    }
}

/// 写入受控 JSON 或 HAR 响应；204 保持严格空正文，以验证断点完成操作的无正文契约。
fn writeFixtureResponse(stream: &mut TcpStream, statusCode: u16, contentType: &str, body: &Value) {
    if statusCode == 204 {
        write!(
            stream,
            "HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
        .expect("写入 MCP 204 fixture 响应失败");
        return;
    }
    let reason = if statusCode == 200 { "OK" } else { "Conflict" };
    let serializedBody = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {statusCode} {reason}\r\ncontent-type: {contentType}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{serializedBody}",
        serializedBody.len()
    )
    .expect("写入 MCP fixture 响应失败");
}
