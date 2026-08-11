#![allow(non_snake_case)]

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use capture_core::{MessageSide, RecordingConfiguration, RecordingSession, TransactionStatus};
use http_proxy_core::{
    BlockCookiesConfiguration, BlockCookiesTool, BlockListConfiguration, BlockListTool, BlockMode,
    BreakpointRule, BreakpointTimeoutAction, BreakpointsConfiguration, BreakpointsTool,
    EditableHttpMessage, HttpProxyConfig, NoCachingConfiguration, NoCachingTool, PipelineContext,
    PipelineDirective, PipelineError, PipelineTool, RewriteConfiguration, RewriteRule,
    RewriteRuleType, RewriteSet, RewriteTool, SslMitmManager, ToolId, ToolPhase, ToolPipeline,
    ToolRegistration, startHttpProxy,
};
use location_core::LocationPattern;
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

/// 创建隔离录制容器，使流水线 HTTP 集成测试不会共享 spill 文件或事务状态。
async fn createCapture(directory: &tempfile::TempDir) -> RecordingSession {
    RecordingSession::new(RecordingConfiguration {
        spillDirectory: directory.path().join("capture"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("测试录制会话必须创建成功")
}

/// 创建只供本测试代理使用的 SSL 管理器；HTTP 工具测试不依赖系统证书或用户证书目录。
fn createSsl() -> SslMitmManager {
    let directory = tempdir().expect("测试证书目录必须创建成功");
    SslMitmManager::load(directory.path()).expect("测试 SSL 管理器必须初始化")
}

/// 返回端口零的短超时代理配置，避免并行集成测试争用监听端口并缩短错误路径等待。
fn createProxyConfig() -> HttpProxyConfig {
    HttpProxyConfig {
        listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
        listenPort: 0,
        connectTimeoutMilliseconds: 500,
        requestTimeoutMilliseconds: 500,
        shutdownTimeoutMilliseconds: 1_000,
        ..HttpProxyConfig::default()
    }
}

/// 读取测试客户端的一次完整 HTTP 响应；响应均携带 Content-Length，因此无需实现通用分块解析器。
async fn readResponse(stream: &mut TcpStream) -> Vec<u8> {
    let mut response = Vec::new();
    let mut buffer = [0_u8; 4 * 1024];
    loop {
        let bytes = stream.read(&mut buffer).await.expect("代理响应必须可读");
        if bytes == 0 {
            return response;
        }
        response.extend_from_slice(&buffer[..bytes]);
        let Some(headerEnd) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let contentLength = response[..headerEnd]
            .split(|byte| *byte == b'\n')
            .find_map(|line| {
                let text = std::str::from_utf8(line).ok()?;
                let (name, value) = text.trim().split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if response.len() >= headerEnd + 4 + contentLength {
            return response;
        }
    }
}

/// 读取测试上游收到的一条完整 HTTP 请求，依据 Content-Length 判断正文边界，避免 TCP 分段让正文断言产生偶发误判。
async fn readRequest(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4 * 1024];
    loop {
        let bytes = stream.read(&mut buffer).await.expect("上游请求必须可读");
        assert_ne!(bytes, 0, "请求正文未读完整时连接不应关闭");
        request.extend_from_slice(&buffer[..bytes]);
        let Some(headerEnd) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let contentLength = request[..headerEnd]
            .split(|byte| *byte == b'\n')
            .find_map(|line| {
                let text = std::str::from_utf8(line).ok()?;
                let (name, value) = text.trim().split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if request.len() >= headerEnd + 4 + contentLength {
            return request;
        }
    }
}

/// 专用于验证 forwarder 正文缓冲边界的最小工具：请求与响应均声明完整正文需求并替换正文与标记头。
struct FullBodyMutationTool;

#[async_trait]
impl PipelineTool for FullBodyMutationTool {
    /// 声明 Rewrite 固定槽位以及双向正文访问需求，使测试覆盖代理在钩子执行前物化正文、执行后同步长度的完整路径。
    fn registration(&self) -> ToolRegistration {
        ToolRegistration::new(
            ToolId::Rewrite,
            vec![ToolPhase::Request, ToolPhase::Response],
            true,
        )
        .withRequestBody()
        .withResponseBody()
    }

    /// 用确定性正文替换客户端提交内容，并添加上游可观察的请求头以验证正文工具发生在出站转发之前。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        context.request.body = Some(Bytes::from_static(b"request-rewritten"));
        context.request.headers.insert(
            "x-test-request-body",
            http::HeaderValue::from_static("rewritten"),
        );
        Ok(PipelineDirective::Applied)
    }

    /// 用确定性正文替换上游响应内容，并添加客户端可观察的响应头以验证正文工具发生在下游传输之前。
    async fn onResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let response = context
            .response
            .as_mut()
            .expect("响应钩子执行时必须存在响应草稿");
        response.body = Some(Bytes::from_static(b"response-rewritten"));
        response.headers.insert(
            "x-test-response-body",
            http::HeaderValue::from_static("rewritten"),
        );
        Ok(PipelineDirective::Applied)
    }
}

/// 记录 SSE 响应正文工具是否被调用；持续事件流没有完整正文边界，因此该工具必须被代理跳过。
struct SseBodyMutationTool {
    responseCalls: Arc<AtomicUsize>,
}

#[async_trait]
impl PipelineTool for SseBodyMutationTool {
    /// 声明只参与响应并要求完整正文，使测试能够证明 SSE 分支不会等待或调用正文工具。
    fn registration(&self) -> ToolRegistration {
        ToolRegistration::new(ToolId::Rewrite, vec![ToolPhase::Response], true).withResponseBody()
    }

    /// 记录错误调用并尝试替换正文；正确的 SSE 路径不会进入此函数。
    async fn onResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        self.responseCalls.fetch_add(1, Ordering::SeqCst);
        context
            .response
            .as_mut()
            .expect("响应钩子执行时必须存在响应草稿")
            .body = Some(Bytes::from_static(b"unexpected"));
        Ok(PipelineDirective::Applied)
    }
}

/// 等待异步录制提交到指定终态；正文泵在客户端响应返回后仍可能处于最后一次提交。
async fn waitForStatus(
    capture: &RecordingSession,
    status: TransactionStatus,
) -> capture_core::TransactionSummary {
    timeout(Duration::from_secs(1), async {
        loop {
            let transactions = capture.listMetadata().await.expect("事务列表必须读取成功");
            if let Some(transaction) = transactions
                .into_iter()
                .find(|transaction| transaction.status == status)
            {
                return transaction;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("事务必须在超时前完成")
}

/// 等待真实代理请求进入断点队列并返回控制面可继续编辑的快照，超时即表明流水线没有把事务正确交给断点工具。
async fn waitForSuspendedBreakpoint(
    tool: &BreakpointsTool,
) -> http_proxy_core::SuspendedBreakpoint {
    timeout(Duration::from_secs(1), async {
        loop {
            if let Some(snapshot) = tool.suspendedBreakpoints().into_iter().next() {
                return snapshot;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("断点必须在超时前挂起真实代理事务")
}

/// Block List 必须在 HTTP 出站前短路，并将响应、工具痕迹和 blocked 终态写入同一事务。
#[tokio::test]
async fn blockListShortCircuitsActualHttpProxyAndRecordsBlockedTransaction() {
    let captureDirectory = tempdir().expect("测试录制目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(
            BlockListTool::new(BlockListConfiguration {
                mode: BlockMode::BlockList,
                locations: vec![LocationPattern {
                    host: "blocked.example.test".to_owned(),
                    ..LocationPattern::default()
                }],
                statusCode: 451,
                responseBody: "blocked by test".to_owned(),
                closeConnection: true,
            })
            .expect("屏蔽规则必须有效"),
        ))
        .expect("屏蔽工具必须注册");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            b"GET http://blocked.example.test/resource HTTP/1.1\r\nHost: blocked.example.test\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("测试请求必须写入");

    let response = readResponse(&mut client).await;
    assert!(response.starts_with(b"HTTP/1.1 451"));
    assert!(response.ends_with(b"blocked by test"));
    let transaction = waitForStatus(&capture, TransactionStatus::Blocked).await;
    assert_eq!(transaction.statusCode, Some(451));
    assert_eq!(transaction.appliedTools, vec!["blockList"]);
    assert!(!transaction.flags.mappedLocal);
    proxy.stop().await.expect("代理必须停止成功");
}

/// 无缓存与 Cookie 工具必须在真实 HTTP 请求/响应链上改写两侧头字段，并把两个工具都保留在同一事务痕迹中。
#[tokio::test]
async fn headerToolsTransformActualHttpProxyRequestAndResponse() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("上游监听必须绑定成功");
    let upstreamAddress = upstreamListener.local_addr().expect("上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener.accept().await.expect("上游必须接受请求");
        let mut request = vec![0_u8; 4 * 1024];
        let bytes = stream.read(&mut request).await.expect("上游请求必须读取");
        let requestText = std::str::from_utf8(&request[..bytes]).expect("请求头必须是 UTF-8");
        assert!(!requestText.to_ascii_lowercase().contains("cookie:"));
        assert!(!requestText.to_ascii_lowercase().contains("if-none-match:"));
        assert!(
            requestText
                .to_ascii_lowercase()
                .contains("cache-control: no-cache")
        );
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nETag: \"v1\"\r\nSet-Cookie: session=1\r\nSet-Cookie: theme=dark\r\nConnection: close\r\n\r\nOK",
            )
            .await
            .expect("上游响应必须写入");
    });
    let captureDirectory = tempdir().expect("测试录制目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(
            NoCachingTool::new(NoCachingConfiguration {
                enabled: true,
                ..NoCachingConfiguration::default()
            })
            .expect("无缓存配置必须有效"),
        ))
        .expect("无缓存工具必须注册");
    pipeline
        .register(Arc::new(
            BlockCookiesTool::new(BlockCookiesConfiguration {
                enabled: true,
                ..BlockCookiesConfiguration::default()
            })
            .expect("Cookie 配置必须有效"),
        ))
        .expect("Cookie 工具必须注册");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            format!(
                "GET http://{upstreamAddress}/resource HTTP/1.1\r\nHost: {upstreamAddress}\r\nIf-None-Match: \"v1\"\r\nCookie: session=1\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("测试请求必须写入");

    let response = readResponse(&mut client).await;
    let responseText = std::str::from_utf8(&response).expect("代理响应必须是 UTF-8");
    let lowerResponse = responseText.to_ascii_lowercase();
    assert!(!lowerResponse.contains("etag:"));
    assert!(!lowerResponse.contains("set-cookie:"));
    assert!(lowerResponse.contains("cache-control: no-cache, no-store, must-revalidate"));
    assert!(response.ends_with(b"OK"));
    let transaction = waitForStatus(&capture, TransactionStatus::Complete).await;
    assert_eq!(transaction.appliedTools, vec!["noCaching", "blockCookies"]);
    upstreamTask.await.expect("上游任务必须成功完成");
    proxy.stop().await.expect("代理必须停止成功");
}

/// 正文工具必须在真实 HTTP 链路中替换请求和响应 body、同步 Content-Length，并把替换后的双向正文写入同一录制事务。
#[tokio::test]
async fn bodyToolsRewriteActualHttpProxyRequestResponseAndCapture() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("上游监听必须绑定成功");
    let upstreamAddress = upstreamListener.local_addr().expect("上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener.accept().await.expect("上游必须接受请求");
        let request = readRequest(&mut stream).await;
        let requestText = std::str::from_utf8(&request).expect("请求必须是 UTF-8");
        let lowerRequest = requestText.to_ascii_lowercase();
        assert!(lowerRequest.contains("content-length: 17"));
        assert!(lowerRequest.contains("x-test-request-body: rewritten"));
        assert!(request.ends_with(b"request-rewritten"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 15\r\nConnection: close\r\n\r\nserver-original",
            )
            .await
            .expect("上游响应必须写入");
    });
    let captureDirectory = tempdir().expect("测试录制目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(FullBodyMutationTool))
        .expect("正文测试工具必须注册");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            format!(
                "POST http://{upstreamAddress}/body HTTP/1.1\r\nHost: {upstreamAddress}\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\noriginal"
            )
            .as_bytes(),
        )
        .await
        .expect("测试请求必须写入");
    let response = readResponse(&mut client).await;
    let responseText = std::str::from_utf8(&response).expect("响应必须是 UTF-8");
    let lowerResponse = responseText.to_ascii_lowercase();
    assert!(lowerResponse.contains("content-length: 18"));
    assert!(lowerResponse.contains("x-test-response-body: rewritten"));
    assert!(response.ends_with(b"response-rewritten"));
    let transaction = waitForStatus(&capture, TransactionStatus::Complete).await;
    assert_eq!(transaction.appliedTools, vec!["rewrite"]);
    let requestBody = capture
        .getBody(&transaction.transactionId, MessageSide::Request)
        .await
        .expect("录制请求正文必须可读");
    let responseBody = capture
        .getBody(&transaction.transactionId, MessageSide::Response)
        .await
        .expect("录制响应正文必须可读");
    assert_eq!(requestBody.bytes, b"request-rewritten");
    assert_eq!(responseBody.bytes, b"response-rewritten");
    upstreamTask.await.expect("上游任务必须成功完成");
    proxy.stop().await.expect("代理必须停止成功");
}

/// URL Rewrite 命中后，真实上游与录制摘要必须共同使用修改后的路径，原始路径只保留在流水线匹配上下文中。
#[tokio::test]
async fn urlRewriteRecordsFinalRequestTarget() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("上游监听必须绑定成功");
    let upstreamAddress = upstreamListener.local_addr().expect("上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener.accept().await.expect("上游必须接受请求");
        let request = readRequest(&mut stream).await;
        assert!(request.starts_with(b"GET /modified?source=rule HTTP/1.1\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK",
            )
            .await
            .expect("上游响应必须写入");
    });
    let captureDirectory = tempdir().expect("测试录制目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let rewriteTool = RewriteTool::new(RewriteConfiguration {
        enabled: true,
        sets: vec![RewriteSet {
            id: "final-target".to_owned(),
            name: "最终目标".to_owned(),
            enabled: true,
            locations: Vec::new(),
            rules: vec![RewriteRule {
                id: "path".to_owned(),
                enabled: true,
                r#type: RewriteRuleType::UrlPath,
                matchRegex: "^/original$".to_owned(),
                replace: "/modified".to_owned(),
                headerName: None,
                matchValueRegex: None,
                headerAction: None,
                caseSensitive: true,
                matchAllOccurrences: false,
            }],
        }],
    })
    .expect("Rewrite 规则必须有效");
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(rewriteTool))
        .expect("Rewrite 工具必须注册");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            format!(
                "GET http://{upstreamAddress}/original?source=rule HTTP/1.1\r\nHost: {upstreamAddress}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("测试请求必须写入");
    let response = readResponse(&mut client).await;
    assert!(response.ends_with(b"OK"));

    let transaction = waitForStatus(&capture, TransactionStatus::Complete).await;
    assert_eq!(transaction.path, "/modified");
    assert_eq!(transaction.query, "source=rule");
    assert_eq!(
        transaction.urlDisplay,
        format!("http://{upstreamAddress}/modified?source=rule")
    );
    assert!(transaction.flags.rewritten);
    upstreamTask.await.expect("上游任务必须成功完成");
    proxy.stop().await.expect("代理必须停止成功");
}

/// RewriteTool 的响应正文规则必须在真实上游响应上生效，向客户端和录制层交付替换后的正文以及重新计算的 Content-Length。
#[tokio::test]
async fn rewriteToolChangesActualHttpProxyResponseBody() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("上游监听必须绑定成功");
    let upstreamAddress = upstreamListener.local_addr().expect("上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener.accept().await.expect("上游必须接受请求");
        let _request = readRequest(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\nold body",
            )
            .await
            .expect("上游响应必须写入");
    });
    let captureDirectory = tempdir().expect("测试录制目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let rewriteTool = RewriteTool::new(RewriteConfiguration {
        enabled: true,
        sets: vec![RewriteSet {
            id: "response-set".to_owned(),
            name: "响应正文替换".to_owned(),
            enabled: true,
            locations: Vec::new(),
            rules: vec![RewriteRule {
                id: "response-body".to_owned(),
                enabled: true,
                r#type: RewriteRuleType::ResponseBody,
                matchRegex: "old".to_owned(),
                replace: "rewritten".to_owned(),
                headerName: None,
                matchValueRegex: None,
                headerAction: None,
                caseSensitive: true,
                matchAllOccurrences: true,
            }],
        }],
    })
    .expect("重写配置必须有效");
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(rewriteTool))
        .expect("重写工具必须注册");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            format!(
                "GET http://{upstreamAddress}/rewrite HTTP/1.1\r\nHost: {upstreamAddress}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("请求必须写入代理");
    let response = readResponse(&mut client).await;
    let responseText = std::str::from_utf8(&response).expect("响应必须是 UTF-8");
    assert!(
        responseText
            .to_ascii_lowercase()
            .contains("content-length: 14")
    );
    assert!(response.ends_with(b"rewritten body"));
    let transaction = waitForStatus(&capture, TransactionStatus::Complete).await;
    assert!(transaction.flags.rewritten);
    assert_eq!(transaction.appliedTools, vec!["rewrite"]);
    let responseBody = capture
        .getBody(&transaction.transactionId, MessageSide::Response)
        .await
        .expect("重写后响应正文必须可读");
    assert_eq!(responseBody.bytes, b"rewritten body");
    upstreamTask.await.expect("上游任务必须成功完成");
    proxy.stop().await.expect("代理必须停止成功");
}

/// 请求断点必须暂停真实代理连接，控制面继续时将编辑后的 URL、正文和长度语义交给上游并写入断点命中事务。
#[tokio::test]
async fn requestBreakpointContinuesActualHttpProxyWithEditedBody() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("上游监听必须绑定成功");
    let upstreamAddress = upstreamListener.local_addr().expect("上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener
            .accept()
            .await
            .expect("编辑后请求必须到达上游");
        let request = readRequest(&mut stream).await;
        let requestText = std::str::from_utf8(&request).expect("请求必须是 UTF-8");
        assert!(requestText.starts_with("POST /edited HTTP/1.1\r\n"));
        assert!(
            requestText
                .to_ascii_lowercase()
                .contains("content-length: 11")
        );
        assert!(request.ends_with(b"edited-body"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await
            .expect("上游响应必须写入");
    });
    let captureDirectory = tempdir().expect("测试录制目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let breakpointTool = BreakpointsTool::new(BreakpointsConfiguration {
        enabled: true,
        rules: vec![BreakpointRule {
            id: "request-edit".to_owned(),
            enabled: true,
            location: LocationPattern::default(),
            onRequest: true,
            onResponse: false,
        }],
        suspendTimeoutSeconds: 1,
        maxSuspended: 4,
        onTimeout: BreakpointTimeoutAction::Abort,
    })
    .expect("断点配置必须有效");
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(breakpointTool.clone()))
        .expect("断点工具必须注册");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            b"POST http://initial.example.test/original HTTP/1.1\r\nHost: initial.example.test\r\nContent-Type: text/plain\r\nContent-Length: 8\r\nConnection: close\r\n\r\noriginal",
        )
        .await
        .expect("请求必须写入代理");
    let snapshot = waitForSuspendedBreakpoint(&breakpointTool).await;
    breakpointTool
        .continueBreakpoint(
            &snapshot.transactionId,
            EditableHttpMessage {
                method: Some("POST".to_owned()),
                url: Some(format!("http://{upstreamAddress}/edited")),
                statusCode: None,
                reason: None,
                headers: Vec::new(),
                bodyBase64: STANDARD.encode(b"edited-body"),
            },
        )
        .expect("有效编辑草稿必须继续请求");
    let response = readResponse(&mut client).await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(b"OK"));
    let transaction = waitForStatus(&capture, TransactionStatus::Complete).await;
    assert!(transaction.flags.breakpointHit);
    assert_eq!(transaction.appliedTools, vec!["breakpoints"]);
    let requestBody = capture
        .getBody(&transaction.transactionId, MessageSide::Request)
        .await
        .expect("编辑后请求正文必须可读");
    assert_eq!(requestBody.bytes, b"edited-body");
    upstreamTask.await.expect("上游任务必须成功完成");
    proxy.stop().await.expect("代理必须停止成功");
}

/// 响应断点必须在上游正文物化后暂停，继续时替换状态和正文并向客户端、录制层交付同一份编辑结果。
#[tokio::test]
async fn responseBreakpointContinuesActualHttpProxyWithEditedResponse() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("上游监听必须绑定成功");
    let upstreamAddress = upstreamListener.local_addr().expect("上游地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener.accept().await.expect("上游必须接受请求");
        let _request = readRequest(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\nConnection: close\r\n\r\nserver-answer",
            )
            .await
            .expect("上游响应必须写入");
    });
    let captureDirectory = tempdir().expect("测试录制目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let breakpointTool = BreakpointsTool::new(BreakpointsConfiguration {
        enabled: true,
        rules: vec![BreakpointRule {
            id: "response-edit".to_owned(),
            enabled: true,
            location: LocationPattern::default(),
            onRequest: false,
            onResponse: true,
        }],
        suspendTimeoutSeconds: 1,
        maxSuspended: 4,
        onTimeout: BreakpointTimeoutAction::Abort,
    })
    .expect("断点配置必须有效");
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(breakpointTool.clone()))
        .expect("断点工具必须注册");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            format!(
                "GET http://{upstreamAddress}/response HTTP/1.1\r\nHost: {upstreamAddress}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("请求必须写入代理");
    let snapshot = waitForSuspendedBreakpoint(&breakpointTool).await;
    breakpointTool
        .continueBreakpoint(
            &snapshot.transactionId,
            EditableHttpMessage {
                method: None,
                url: None,
                statusCode: Some(201),
                reason: None,
                headers: Vec::new(),
                bodyBase64: STANDARD.encode(b"edited-response"),
            },
        )
        .expect("有效响应草稿必须继续响应");
    let response = readResponse(&mut client).await;
    let responseText = std::str::from_utf8(&response).expect("响应必须是 UTF-8");
    assert!(responseText.starts_with("HTTP/1.1 201"));
    assert!(
        responseText
            .to_ascii_lowercase()
            .contains("content-length: 15")
    );
    assert!(response.ends_with(b"edited-response"));
    let transaction = waitForStatus(&capture, TransactionStatus::Complete).await;
    assert!(transaction.flags.breakpointHit);
    assert_eq!(transaction.appliedTools, vec!["breakpoints"]);
    let responseBody = capture
        .getBody(&transaction.transactionId, MessageSide::Response)
        .await
        .expect("编辑后响应正文必须可读");
    assert_eq!(responseBody.bytes, b"edited-response");
    upstreamTask.await.expect("上游任务必须成功完成");
    proxy.stop().await.expect("代理必须停止成功");
}

/// SSE 必须在上游连接仍保持打开时把首事件交付客户端，并跳过依赖完整正文的响应工具；
/// 事件帧仍经过有界响应通道和录制副本，流结束后事务才进入完成状态。
#[tokio::test]
async fn sseForwardsFirstEventBeforeUpstreamCompletesAndSkipsBodyTools() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("SSE 上游监听必须绑定成功");
    let upstreamAddress = upstreamListener.local_addr().expect("SSE 上游地址必须可读");
    let (firstEventSender, firstEventReceiver) = tokio::sync::oneshot::channel();
    let (finishSender, finishReceiver) = tokio::sync::oneshot::channel();
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener
            .accept()
            .await
            .expect("SSE 上游必须接受请求");
        let _request = readRequest(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: Text/Event-Stream; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("SSE 响应头必须写入");
        let firstEvent = b"data: first\n\n";
        stream
            .write_all(format!("{:X}\r\n", firstEvent.len()).as_bytes())
            .await
            .expect("SSE 首事件长度必须写入");
        stream
            .write_all(firstEvent)
            .await
            .expect("SSE 首事件必须写入");
        stream.write_all(b"\r\n").await.expect("SSE 首事件必须结束");
        firstEventSender
            .send(())
            .expect("测试协调器必须接收首事件写入通知");
        finishReceiver
            .await
            .expect("测试协调器必须允许 SSE 上游结束");
        let secondEvent = b"data: second\n\n";
        stream
            .write_all(format!("{:X}\r\n", secondEvent.len()).as_bytes())
            .await
            .expect("SSE 第二事件长度必须写入");
        stream
            .write_all(secondEvent)
            .await
            .expect("SSE 第二事件必须写入");
        stream
            .write_all(b"\r\n0\r\n\r\n")
            .await
            .expect("SSE 分块响应必须结束");
    });
    let captureDirectory = tempdir().expect("测试录制目录必须创建成功");
    let capture = createCapture(&captureDirectory).await;
    let responseCalls = Arc::new(AtomicUsize::new(0));
    let pipeline = ToolPipeline::new();
    pipeline
        .register(Arc::new(SseBodyMutationTool {
            responseCalls: responseCalls.clone(),
        }))
        .expect("SSE 正文工具必须注册");
    let proxy = startHttpProxy(
        createProxyConfig(),
        capture.clone(),
        createSsl(),
        pipeline,
        CancellationToken::new(),
    )
    .await
    .expect("代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("SSE 测试客户端必须连接代理");
    client
        .write_all(
            format!(
                "GET http://{upstreamAddress}/events HTTP/1.1\r\nHost: {upstreamAddress}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("SSE 请求必须写入代理");
    firstEventReceiver.await.expect("SSE 上游必须写出首事件");
    let firstResponse = timeout(Duration::from_secs(1), async {
        let mut received = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let bytes = client.read(&mut buffer).await.expect("SSE 首事件必须可读");
            assert_ne!(bytes, 0, "SSE 上游未结束时客户端连接不得提前关闭");
            received.extend_from_slice(&buffer[..bytes]);
            if received
                .windows(b"data: first\n\n".len())
                .any(|window| window == b"data: first\n\n")
            {
                return received;
            }
        }
    })
    .await
    .expect("SSE 首事件必须在上游结束前交付");
    assert!(
        firstResponse
            .windows(b"data: first\n\n".len())
            .any(|window| window == b"data: first\n\n")
    );
    assert_eq!(responseCalls.load(Ordering::SeqCst), 0);
    finishSender.send(()).expect("SSE 上游必须收到结束许可");
    let mut remainingResponse = Vec::new();
    client
        .read_to_end(&mut remainingResponse)
        .await
        .expect("SSE 剩余响应必须读取完成");
    assert!(
        remainingResponse
            .windows(b"data: second\n\n".len())
            .any(|window| window == b"data: second\n\n")
    );
    let transaction = waitForStatus(&capture, TransactionStatus::Complete).await;
    assert!(transaction.appliedTools.is_empty());
    let responseBody = capture
        .getBody(&transaction.transactionId, MessageSide::Response)
        .await
        .expect("SSE 录制正文必须可读");
    assert_eq!(responseBody.bytes, b"data: first\n\ndata: second\n\n");
    upstreamTask.await.expect("SSE 上游任务必须完成");
    proxy.stop().await.expect("代理必须停止成功");
}
