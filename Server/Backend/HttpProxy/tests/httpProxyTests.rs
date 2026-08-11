#![allow(non_snake_case)]

use std::{
    convert::Infallible,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use capture_core::{
    MessageSide, RecordingConfiguration, RecordingSession, TransactionProtocol, TransactionStatus,
};
use http::{Request, Response, Version};
use http_body_util::{BodyExt, Full};
use http_proxy_core::{
    HttpProxyConfig, HttpProxyError, SocksHttpTarget, SocksHttpTunnelHandler, SslMitmConfiguration,
    SslMitmManager, ToolPipeline, startHttpProxy,
};
use hyper::{body::Incoming, client::conn::http2, service::service_fn};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto,
};
use location_core::LocationPattern;
use plugin_host::PluginHost;
use rcgen::generate_simple_self_signed;
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;
use transport_core::{UpstreamProxyConfiguration, UpstreamProxyProtocol};

/// 为每个代理测试生成隔离根证书，防止并行测试共享用户目录或叶证书缓存。
fn testSsl() -> SslMitmManager {
    let directory = tempdir().expect("临时证书目录必须可创建");
    SslMitmManager::load(directory.path()).expect("测试 SSL 管理器必须可初始化")
}

/// 创建信任指定 HTTPS mock 根的 SSL 管理器，并显式启用 localhost 解密规则。
fn testInterceptingSsl(upstreamRoot: CertificateDer<'static>) -> SslMitmManager {
    let directory = tempdir().expect("临时证书目录必须可创建");
    let manager = SslMitmManager::loadWithUpstreamRoots(directory.path(), vec![upstreamRoot])
        .expect("测试 SSL 管理器必须可初始化");
    manager
        .updateConfiguration(SslMitmConfiguration {
            enabled: true,
            includeLocations: vec![LocationPattern {
                protocol: "https".to_owned(),
                host: "localhost".to_owned(),
                port: String::new(),
                path: String::new(),
                query: None,
            }],
            excludeLocations: Vec::new(),
            maxCachedCertificates: 16,
            useClientSni: true,
        })
        .expect("localhost 解密规则必须有效");
    manager
}

struct ToyHttpServer {
    address: SocketAddr,
    acceptedConnections: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl ToyHttpServer {
    /// 启动固定响应的本机 HTTP/1.1 服务；delay 用于构造可重复的上游超时。
    async fn start(delay: Duration) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("测试 HTTP 服务必须成功绑定");
        let address = listener.local_addr().expect("测试监听地址必须可读");
        let acceptedConnections = Arc::new(AtomicUsize::new(0));
        let counter = acceptedConnections.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(serveToyHttpConnection(stream, delay));
            }
        });
        Self {
            address,
            acceptedConnections,
            task,
        }
    }
}

impl Drop for ToyHttpServer {
    /// 测试结束时终止监听任务；已建立连接随运行时测试作用域一并释放。
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct ToyHttpsServer {
    address: SocketAddr,
    rootCertificate: CertificateDer<'static>,
    task: JoinHandle<()>,
}

impl ToyHttpsServer {
    /// 启动使用隔离自签名根的严格 HTTPS mock；根证书仅注入当前代理测试的上游信任库。
    ///
    /// 服务端同时公布 h2 与 HTTP/1.1，但只接受 HTTP/1.1 消息，用于复现会因代理擅自升级
    /// 已重建请求而返回 400 的真实网关。multipart 分支还会核对声明长度与实际正文完全一致。
    async fn start() -> Self {
        let certified = generate_simple_self_signed(vec!["localhost".to_owned()])
            .expect("测试 HTTPS 证书必须生成");
        let rootCertificate = certified.cert.der().clone();
        let privateKey = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        ));
        let mut serverConfiguration = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![rootCertificate.clone()], privateKey)
            .expect("测试 HTTPS 配置必须有效");
        serverConfiguration.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(serverConfiguration));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("测试 HTTPS 服务必须绑定");
        let address = listener.local_addr().expect("测试 HTTPS 地址必须可读");
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(tlsStream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let service = service_fn(|request: Request<Incoming>| async move {
                        let requestVersion = request.version();
                        let multipartRequest = request
                            .headers()
                            .get(http::header::CONTENT_TYPE)
                            .and_then(|value| value.to_str().ok())
                            .is_some_and(|value| value.starts_with("multipart/form-data"));
                        let declaredBodyBytes = request
                            .headers()
                            .get(http::header::CONTENT_LENGTH)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.parse::<usize>().ok());
                        let body = request
                            .into_body()
                            .collect()
                            .await
                            .expect("严格 HTTPS mock 必须完整读取请求正文")
                            .to_bytes();
                        let validMultipartLength =
                            !multipartRequest || declaredBodyBytes == Some(body.len());
                        let status = if requestVersion == Version::HTTP_11 && validMultipartLength {
                            200
                        } else {
                            400
                        };
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(status)
                                .header("content-type", "application/json")
                                .body(Full::new(Bytes::from_static(br#"{"secure":true}"#)))
                                .expect("测试 HTTPS 响应必须构建"),
                        )
                    });
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tlsStream), service)
                        .await;
                });
            }
        });
        Self {
            address,
            rootCertificate,
            task,
        }
    }
}

impl Drop for ToyHttpsServer {
    /// 测试结束时终止 HTTPS 监听，已建立 TLS 任务由 Tokio 测试运行时统一释放。
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 在同一上游连接上循环处理请求，验证代理连接池和客户端 keep-alive 行为。
async fn serveToyHttpConnection(mut stream: TcpStream, delay: Duration) {
    let mut bufferedBytes = Vec::new();
    loop {
        let request = match readHttpMessage(&mut stream, &mut bufferedBytes).await {
            Ok(Some(request)) => request,
            Ok(None) | Err(_) => return,
        };
        if request.is_empty() {
            return;
        }
        sleep(delay).await;
        let responseBody = b"toy-response";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            responseBody.len()
        );
        if stream.write_all(response.as_bytes()).await.is_err()
            || stream.write_all(responseBody).await.is_err()
        {
            return;
        }
    }
}

/// 启动把收到的 Host 作为响应正文返回的一次性上游，用于验证 authority 重写。
async fn startHostEchoServer() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("Host 回显服务必须绑定");
    let address = listener.local_addr().expect("Host 回显地址必须可读");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("必须接受代理连接");
        let mut bufferedBytes = Vec::new();
        let request = readHttpMessage(&mut stream, &mut bufferedBytes)
            .await
            .expect("上游请求必须有效")
            .expect("上游请求不得提前结束");
        let host = headerValue(&request, "host").expect("上游请求必须携带 Host");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{host}",
            host.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("Host 回显响应必须写入");
    });
    (address, task)
}

/// 启动一次性大响应上游，并在响应头写出后通知测试开始停止代理。
async fn startLargeResponseServer() -> (
    SocketAddr,
    tokio::sync::oneshot::Receiver<()>,
    JoinHandle<()>,
) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("大响应服务必须绑定");
    let address = listener.local_addr().expect("大响应地址必须可读");
    let (startedSender, startedReceiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut bufferedBytes = Vec::new();
        if readHttpMessage(&mut stream, &mut bufferedBytes)
            .await
            .is_err()
        {
            return;
        }
        let responseBytes = 32 * 1024 * 1024;
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {responseBytes}\r\nConnection: keep-alive\r\n\r\n"
        );
        if stream.write_all(header.as_bytes()).await.is_err() {
            return;
        }
        if startedSender.send(()).is_err() {
            return;
        }
        let chunk = [b'x'; 64 * 1024];
        let mut writtenBytes = 0;
        while writtenBytes < responseBytes {
            if stream.write_all(&chunk).await.is_err() {
                return;
            }
            writtenBytes += chunk.len();
        }
    });
    (address, startedReceiver, task)
}

/// 启动读取少量请求正文后立即响应的上游，复现未消费完整上传的 early response。
async fn startEarlyResponseServer() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("early response 服务必须绑定");
    let address = listener.local_addr().expect("early response 地址必须可读");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("必须接受代理连接");
        let mut bufferedBytes = Vec::new();
        let headerEnd = loop {
            if let Some(position) = findHeaderEnd(&bufferedBytes) {
                break position + 4;
            }
            let mut chunk = [0_u8; 4096];
            let readBytes = stream.read(&mut chunk).await.expect("请求头必须可读");
            if readBytes == 0 {
                return;
            }
            bufferedBytes.extend_from_slice(&chunk[..readBytes]);
        };
        let mut receivedBodyBytes = bufferedBytes.len().saturating_sub(headerEnd);
        let mut bodyChunk = [0_u8; 7];
        while receivedBodyBytes < bodyChunk.len() {
            let readBytes = stream
                .read(&mut bodyChunk[receivedBodyBytes..])
                .await
                .expect("early response 请求正文必须可读");
            if readBytes == 0 {
                return;
            }
            receivedBodyBytes += readBytes;
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .expect("early response 必须写入");
    });
    (address, task)
}

/// 读取一条含 Content-Length 的 HTTP 消息，并保留可能属于下一条消息的多读字节。
async fn readHttpMessage<Stream>(
    stream: &mut Stream,
    bufferedBytes: &mut Vec<u8>,
) -> io::Result<Option<Vec<u8>>>
where
    Stream: AsyncRead + Unpin,
{
    let headerEnd = loop {
        if let Some(position) = findHeaderEnd(bufferedBytes) {
            break position;
        }
        let mut chunk = [0_u8; 4096];
        let readBytes = stream.read(&mut chunk).await?;
        if readBytes == 0 {
            return Ok(None);
        }
        bufferedBytes.extend_from_slice(&chunk[..readBytes]);
    };
    let bodyBytes = contentLength(&bufferedBytes[..headerEnd])?;
    let messageBytes = headerEnd + 4 + bodyBytes;
    while bufferedBytes.len() < messageBytes {
        let mut chunk = [0_u8; 4096];
        let readBytes = stream.read(&mut chunk).await?;
        if readBytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "测试 HTTP 消息正文提前结束",
            ));
        }
        bufferedBytes.extend_from_slice(&chunk[..readBytes]);
    }
    Ok(Some(bufferedBytes.drain(..messageBytes).collect()))
}

/// 查找 HTTP 头结束边界；未收齐时返回 None。
fn findHeaderEnd(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

/// 从测试消息头解析 Content-Length；缺失时按零正文处理。
fn contentLength(headerBytes: &[u8]) -> io::Result<usize> {
    let headerText = std::str::from_utf8(headerBytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    for line in headerText.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
    Ok(0)
}

/// 从完整测试消息中提取指定头字段，保持大小写不敏感的 HTTP 语义。
fn headerValue(message: &[u8], expectedName: &str) -> Option<String> {
    let headerEnd = findHeaderEnd(message)?;
    let headerText = std::str::from_utf8(&message[..headerEnd]).ok()?;
    headerText.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(expectedName)
            .then(|| value.trim().to_owned())
    })
}

/// 创建独立录制会话；临时目录由调用测试持有，避免正文跨测试污染。
async fn createCapture(directory: &tempfile::TempDir) -> RecordingSession {
    RecordingSession::new(RecordingConfiguration {
        spillDirectory: directory.path().to_path_buf(),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("测试录制会话必须创建成功")
}

/// 等待异步正文泵或隧道任务提交终态；截止时间内未完成会使测试明确失败。
async fn waitForTransactionStatus(
    capture: &RecordingSession,
    expectedStatus: TransactionStatus,
) -> capture_core::TransactionSummary {
    timeout(Duration::from_secs(1), async {
        loop {
            let transactions = capture.listMetadata().await.expect("事务列表必须可读");
            if let Some(transaction) = transactions
                .into_iter()
                .find(|transaction| transaction.status == expectedStatus)
            {
                return transaction;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("事务必须在截止时间内进入预期终态")
}

/// 返回端口零的代理配置，确保并行测试不会争用固定端口。
fn testConfig() -> HttpProxyConfig {
    HttpProxyConfig {
        listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
        listenPort: 0,
        connectTimeoutMilliseconds: 500,
        requestTimeoutMilliseconds: 500,
        shutdownTimeoutMilliseconds: 1_000,
        ..HttpProxyConfig::default()
    }
}

/// 可配置监听地址必须与 SOCKS5 一致允许通配地址，实际暴露范围由用户配置决定。
#[test]
fn acceptsExplicitUnspecifiedListenHost() {
    let mut config = testConfig();
    config.listenHost = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    config.validate().expect("显式通配监听地址必须有效");
}

/// 连接数与单连接正文镜像上限的危险组合必须在启动前被总预算拒绝。
#[test]
fn rejectsExcessiveTotalCaptureBufferBudget() {
    let mut config = testConfig();
    config.maxConnections = 512;
    config.maxCaptureBodyBytes = 64 * 1024 * 1024;
    let error = config.validate().expect_err("超额正文镜像预算必须失败");
    assert!(matches!(error, HttpProxyError::CaptureBudgetExceeded));
    assert_eq!(error.code(), "httpProxyCaptureBudgetExceeded");
    assert_eq!(error.messageKey(), "error.httpProxy.captureBudgetExceeded");
}

/// 慢请求连接数与头上限的危险组合必须在 Hyper 分配缓冲前被总预算拒绝。
#[test]
fn rejectsExcessiveTotalHeaderBufferBudget() {
    let mut config = testConfig();
    config.maxConnections = 16_384;
    config.maxHeaderBytes = 1024 * 1024;
    let error = config.validate().expect_err("超额请求头缓冲预算必须失败");
    assert!(matches!(error, HttpProxyError::HeaderBudgetExceeded));
    assert_eq!(error.code(), "httpProxyHeaderBudgetExceeded");
    assert_eq!(error.messageKey(), "error.httpProxy.headerBudgetExceeded");
}

/// 通过真实 reqwest 代理链验证明文 HTTP、头体捕获及列表不携带正文。
#[tokio::test]
async fn forwardsHttpAndCapturesBoundedTransaction() {
    let upstream = ToyHttpServer::start(Duration::ZERO).await;
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::all(format!("http://{}", proxy.boundAddress()))
                .expect("测试代理地址必须有效"),
        )
        .build()
        .expect("测试客户端必须构建成功");

    let response = client
        .post(format!("http://{}/echo?name=test", upstream.address))
        .header("content-type", "text/plain")
        .body("请求正文")
        .send()
        .await
        .expect("经代理请求必须成功");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.text().await.expect("响应正文必须可读"),
        "toy-response"
    );

    let transactions = capture.listMetadata().await.expect("事务列表必须可读");
    assert_eq!(transactions.len(), 1);
    let transaction = &transactions[0];
    assert_eq!(transaction.protocol, TransactionProtocol::Http);
    assert_eq!(transaction.status, TransactionStatus::Complete);
    assert_eq!(transaction.statusCode, Some(200));
    assert_eq!(transaction.method, "POST");
    assert_eq!(transaction.path, "/echo");
    assert_eq!(transaction.query, "name=test");
    assert_eq!(transaction.sizes.requestBodyBytes, "请求正文".len() as u64);
    assert_eq!(transaction.sizes.responseBodyBytes, 12);
    let requestBody = capture
        .getBody(&transaction.transactionId, MessageSide::Request)
        .await
        .expect("请求正文必须按需读取");
    assert_eq!(requestBody.bytes, "请求正文".as_bytes());
    let listJson = serde_json::to_string(&transactions).expect("事务列表必须可序列化");
    assert!(!listJson.contains("请求正文"));
    assert!(!listJson.contains("toy-response"));

    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 向代理发送以其自身监听地址为目标的绝对 URI；服务必须立即返回 508，而不是重新连接自身并无限生成事务。
#[tokio::test]
async fn rejectsHttpRequestThatTargetsProxyListener() {
    let directory = tempfile::tempdir().expect("临时录制目录必须创建成功");
    let capture = createCapture(&directory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let proxyAddress = proxy.boundAddress();
    let mut client = TcpStream::connect(proxyAddress)
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            format!(
                "GET http://{proxyAddress}/ HTTP/1.1\r\nHost: {proxyAddress}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("自环测试请求必须写入");
    let response = readHttpMessage(&mut client, &mut Vec::new())
        .await
        .expect("代理必须返回完整响应")
        .expect("代理不得提前关闭响应");
    assert!(response.starts_with(b"HTTP/1.1 508 Loop Detected"));
    assert_eq!(
        headerValue(&response, "x-proxy-error-code").as_deref(),
        Some("httpProxyLoopDetected")
    );
    let transaction = waitForTransactionStatus(&capture, TransactionStatus::Failed).await;
    assert_eq!(transaction.host, proxyAddress.ip().to_string());
    assert_eq!(transaction.port, proxyAddress.port());
    assert_eq!(
        transaction.error.expect("自环事务必须记录失败").code,
        "httpProxyLoopDetected"
    );
    assert_eq!(
        capture
            .listMetadata()
            .await
            .expect("事务列表必须可读")
            .len(),
        1
    );
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 通过真实 reqwest CONNECT、双向 TLS 和 HTTPS mock 验证明文捕获及自定义 CA 信任链。
#[tokio::test]
async fn interceptsIncludedHttpsAndCapturesDecryptedBody() {
    let upstream = ToyHttpsServer::start().await;
    let directory = tempfile::tempdir().expect("临时录制目录必须创建");
    let capture = createCapture(&directory).await;
    let ssl = testInterceptingSsl(upstream.rootCertificate.clone());
    let clientRoot = reqwest::Certificate::from_pem(&ssl.exportRootPem())
        .expect("代理根证书必须可供 reqwest 信任");
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        ssl.clone(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTPS 代理必须启动");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::all(format!("http://{}", proxy.boundAddress()))
                .expect("测试代理地址必须有效"),
        )
        .tls_certs_only([clientRoot])
        .build()
        .expect("HTTPS 测试客户端必须构建");

    let response = client
        .get(format!(
            "https://localhost:{}/secure",
            upstream.address.port()
        ))
        .send()
        .await
        .expect("经 MITM 代理的 HTTPS 请求必须成功");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.text().await.expect("HTTPS 正文必须可读"),
        r#"{"secure":true}"#
    );

    let transaction = waitForTransactionStatus(&capture, TransactionStatus::Complete).await;
    assert_eq!(transaction.protocol, TransactionProtocol::Https);
    assert_eq!(transaction.host, "localhost");
    assert_eq!(transaction.path, "/secure");
    assert!(transaction.flags.mitmDecrypted);
    let responseBody = capture
        .getBody(&transaction.transactionId, MessageSide::Response)
        .await
        .expect("解密后的 HTTPS 正文必须按需读取");
    assert_eq!(responseBody.bytes, br#"{"secure":true}"#);
    let sslState = ssl.publicState();
    assert_eq!(sslState.handshakeSuccessTotal, 1);
    assert_eq!(sslState.handshakeFailureTotal, 0);
    assert_eq!(sslState.cachedLeafCount, 1);
    proxy.stop().await.expect("HTTPS 代理必须有序停止");
}

/// 真实 HTTP/2 客户端会把 `:scheme` 与 `:authority` 还原为绝对 URI；代理必须校验 CONNECT 绑定后继续转发。
#[tokio::test]
async fn interceptedHttpsAcceptsHttp2AuthorityBoundToConnectTarget() {
    let upstream = ToyHttpsServer::start().await;
    let directory = tempfile::tempdir().expect("临时录制目录必须创建");
    let capture = createCapture(&directory).await;
    let ssl = testInterceptingSsl(upstream.rootCertificate.clone());
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        ssl.clone(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTPS 代理必须启动");

    let mut tunnel = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("HTTP/2 测试客户端必须连接代理");
    let authority = format!("localhost:{}", upstream.address.port());
    tunnel
        .write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await
        .expect("CONNECT 请求必须写入");
    let connectResponse = readHttpMessage(&mut tunnel, &mut Vec::new())
        .await
        .expect("CONNECT 响应必须可读取")
        .expect("CONNECT 响应不得提前结束");
    assert!(connectResponse.starts_with(b"HTTP/1.1 200"));

    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ssl.exportRootDer()))
        .expect("代理根证书必须加入客户端信任库");
    let mut clientConfiguration = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    clientConfiguration.alpn_protocols = vec![b"h2".to_vec()];
    let tls = TlsConnector::from(Arc::new(clientConfiguration))
        .connect(
            ServerName::try_from("localhost".to_owned()).expect("测试服务器名必须有效"),
            tunnel,
        )
        .await
        .expect("HTTP/2 下游 TLS 握手必须成功");
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));

    let (mut sender, connection) = http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
        .await
        .expect("HTTP/2 客户端状态机必须建立");
    let connectionTask = tokio::spawn(connection);
    let multipartBody = Bytes::from_static(b"--fixture-boundary--\r\n");
    let request = Request::builder()
        .version(Version::HTTP_2)
        .method("POST")
        .uri(format!("https://{authority}/secure"))
        .header(
            http::header::CONTENT_TYPE,
            "multipart/form-data; boundary=fixture-boundary",
        )
        .header(http::header::CONTENT_LENGTH, multipartBody.len())
        .body(Full::new(multipartBody))
        .expect("HTTP/2 请求必须构建");
    let response = sender
        .send_request(request)
        .await
        .expect("经解密代理转发的 HTTP/2 请求必须成功");
    let responseStatus = response.status();
    let responseBody = response
        .into_body()
        .collect()
        .await
        .expect("HTTP/2 响应正文必须可读取")
        .to_bytes();
    assert_eq!(
        responseStatus,
        200,
        "HTTP/2 转发失败响应：{}",
        String::from_utf8_lossy(&responseBody)
    );
    assert_eq!(responseBody.as_ref(), br#"{"secure":true}"#);

    let transaction = waitForTransactionStatus(&capture, TransactionStatus::Complete).await;
    assert_eq!(transaction.protocol, TransactionProtocol::Https);
    assert_eq!(transaction.path, "/secure");
    assert_eq!(transaction.statusCode, Some(200));
    assert!(transaction.flags.mitmDecrypted);
    drop(sender);
    connectionTask.abort();
    proxy.stop().await.expect("HTTPS 代理必须有序停止");
}

/// 工具物化预算不得裁剪录制副本；请求和响应正文必须与线上字节完全一致。
#[tokio::test]
async fn recordsCompleteBodiesIndependentOfPipelineMaterializationLimit() {
    let upstream = ToyHttpServer::start(Duration::ZERO).await;
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let mut config = testConfig();
    config.maxCaptureBodyBytes = 4;
    let proxy = startHttpProxy(
        config,
        capture.clone(),
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://{}", proxy.boundAddress()))
                .expect("测试代理地址必须有效"),
        )
        .build()
        .expect("测试客户端必须构建成功");

    let response = client
        .post(format!("http://{}/bounded", upstream.address))
        .body("abcdefgh")
        .send()
        .await
        .expect("经代理请求必须成功");
    assert_eq!(
        response.text().await.expect("完整响应必须可读"),
        "toy-response"
    );
    let transaction = waitForTransactionStatus(&capture, TransactionStatus::Complete).await;
    let requestBody = capture
        .getBody(&transaction.transactionId, MessageSide::Request)
        .await
        .expect("请求录制副本必须可读");
    let responseBody = capture
        .getBody(&transaction.transactionId, MessageSide::Response)
        .await
        .expect("响应录制副本必须可读");
    assert_eq!(requestBody.bytes, b"abcdefgh");
    assert_eq!(requestBody.meta.originalBytes, 8);
    assert!(!requestBody.meta.truncated);
    assert_eq!(responseBody.bytes, b"toy-response");
    assert_eq!(responseBody.meta.originalBytes, 12);
    assert!(!responseBody.meta.truncated);
    assert!(!transaction.flags.bodyTruncated);
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// absolute-form 中不可信的客户端 Host 必须被已解析目标 authority 覆盖。
#[tokio::test]
async fn rewritesMismatchedHostFromAbsoluteTarget() {
    let (upstreamAddress, upstreamTask) = startHostEchoServer().await;
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture,
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    let request =
        format!("GET http://{upstreamAddress}/host HTTP/1.1\r\nHost: attacker.invalid\r\n\r\n");
    client
        .write_all(request.as_bytes())
        .await
        .expect("测试请求必须写入");
    let mut bufferedBytes = Vec::new();
    let response = readHttpMessage(&mut client, &mut bufferedBytes)
        .await
        .expect("代理响应必须有效")
        .expect("代理响应不得提前结束");
    assert!(response.ends_with(upstreamAddress.to_string().as_bytes()));
    upstreamTask.await.expect("Host 回显任务必须正常结束");
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 上游提前响应时请求 body 完成信号必须线性化最终镜像，不能留下 pending 或错误字节数。
#[tokio::test]
async fn capturesForwardedPrefixAfterEarlyResponse() {
    let (upstreamAddress, upstreamTask) = startEarlyResponseServer().await;
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    let request = format!(
        "POST http://{upstreamAddress}/early HTTP/1.1\r\nHost: {upstreamAddress}\r\nContent-Length: 1024\r\n\r\npartial"
    );
    client
        .write_all(request.as_bytes())
        .await
        .expect("部分上传必须写入");
    let mut bufferedResponse = Vec::new();
    let response = readHttpMessage(&mut client, &mut bufferedResponse)
        .await
        .expect("early response 必须有效")
        .expect("early response 不得提前断开");
    assert!(response.starts_with(b"HTTP/1.1 200 OK"));
    upstreamTask.await.expect("early response 上游必须正常结束");
    let transaction = waitForTransactionStatus(&capture, TransactionStatus::Complete).await;
    assert_eq!(transaction.sizes.requestBodyBytes, 7);
    let requestBody = capture
        .getBody(&transaction.transactionId, MessageSide::Request)
        .await
        .expect("early response 请求镜像必须可读");
    assert_eq!(requestBody.bytes, b"partial");
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 在同一个客户端 TCP 连接连续发送两条请求，验证 HTTP/1.1 keep-alive。
#[tokio::test]
async fn keepsClientAndUpstreamConnectionsAlive() {
    let upstream = ToyHttpServer::start(Duration::ZERO).await;
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture,
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    let mut bufferedBytes = Vec::new();

    for path in ["/first", "/second"] {
        let request = format!(
            "GET http://{}{path} HTTP/1.1\r\nHost: {}\r\n\r\n",
            upstream.address, upstream.address
        );
        client
            .write_all(request.as_bytes())
            .await
            .expect("测试请求必须写入");
        let response = readHttpMessage(&mut client, &mut bufferedBytes)
            .await
            .expect("代理响应必须有效")
            .expect("代理不得提前关闭 keep-alive");
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));
        assert!(response.ends_with(b"toy-response"));
    }
    assert_eq!(upstream.acceptedConnections.load(Ordering::SeqCst), 1);
    drop(client);
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 通过 CONNECT 建立裸隧道并验证双向字节转发。
#[tokio::test]
async fn forwardsConnectTunnelAndCapturesMetadata() {
    let echoListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("回显服务必须绑定");
    let echoAddress = echoListener.local_addr().expect("回显地址必须可读");
    let echoTask = tokio::spawn(async move {
        let (mut stream, _) = echoListener.accept().await.expect("必须接受隧道连接");
        let mut bytes = [0_u8; 64];
        loop {
            let readBytes = stream.read(&mut bytes).await.expect("回显读取必须成功");
            if readBytes == 0 {
                return;
            }
            stream
                .write_all(&bytes[..readBytes])
                .await
                .expect("回显写入必须成功");
        }
    });
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    let request = format!("CONNECT {echoAddress} HTTP/1.1\r\nHost: {echoAddress}\r\n\r\n");
    client
        .write_all(request.as_bytes())
        .await
        .expect("CONNECT 请求必须写入");
    let mut bufferedBytes = Vec::new();
    let response = readHttpMessage(&mut client, &mut bufferedBytes)
        .await
        .expect("CONNECT 响应必须有效")
        .expect("CONNECT 响应不得提前结束");
    assert!(response.starts_with(b"HTTP/1.1 200 OK"));
    client
        .write_all(b"tunnel-bytes")
        .await
        .expect("隧道必须可写");
    let mut echoed = [0_u8; 12];
    client
        .read_exact(&mut echoed)
        .await
        .expect("隧道回显必须可读");
    assert_eq!(&echoed, b"tunnel-bytes");
    client.shutdown().await.expect("客户端写半关闭必须成功");
    let mut trailingByte = [0_u8; 1];
    let trailingBytes = timeout(Duration::from_secs(1), client.read(&mut trailingByte))
        .await
        .expect("远端半关闭必须传播")
        .expect("隧道关闭读取必须成功");
    assert_eq!(trailingBytes, 0);
    echoTask.await.expect("回显任务必须正常退出");

    let transaction = waitForTransactionStatus(&capture, TransactionStatus::Complete).await;
    assert_eq!(transaction.protocol, TransactionProtocol::Tunnel);
    assert_eq!(transaction.sizes.requestBodyBytes, 12);
    assert_eq!(transaction.sizes.responseBodyBytes, 12);
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 验证 HTTP CONNECT 裸隧道复用统一出站连接器，而不是绕过已启用的二级代理直接解析目标。
#[tokio::test]
async fn forwardsConnectTunnelThroughConfiguredUpstreamProxy() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("二级代理夹具必须绑定");
    let upstreamAddress = upstreamListener.local_addr().expect("二级代理地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener.accept().await.expect("必须收到出站连接");
        let mut requestBytes = Vec::new();
        loop {
            let mut chunk = [0_u8; 256];
            let readBytes = stream.read(&mut chunk).await.expect("CONNECT 头必须可读");
            assert_ne!(readBytes, 0, "CONNECT 头不得提前结束");
            requestBytes.extend_from_slice(&chunk[..readBytes]);
            if findHeaderEnd(&requestBytes).is_some() {
                break;
            }
        }
        assert!(
            requestBytes.starts_with(b"CONNECT unresolved.invalid:443 HTTP/1.1\r\n"),
            "二级代理必须收到未经本机 DNS 改写的权威目标"
        );
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .expect("二级代理成功响应必须可写");
        let mut payload = [0_u8; 14];
        stream
            .read_exact(&mut payload)
            .await
            .expect("隧道载荷必须到达二级代理");
        stream
            .write_all(&payload)
            .await
            .expect("二级代理回显必须可写");
    });
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let mut config = testConfig();
    config.upstreamProxy = UpstreamProxyConfiguration {
        enabled: true,
        protocol: UpstreamProxyProtocol::Http,
        host: upstreamAddress.ip().to_string(),
        port: upstreamAddress.port(),
        username: String::new(),
        password: String::new(),
    };
    let proxy = startHttpProxy(
        config,
        capture,
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(
            b"CONNECT unresolved.invalid:443 HTTP/1.1\r\nHost: unresolved.invalid:443\r\n\r\n",
        )
        .await
        .expect("CONNECT 请求必须写入");
    let mut responseBytes = Vec::new();
    let response = readHttpMessage(&mut client, &mut responseBytes)
        .await
        .expect("CONNECT 响应必须有效")
        .expect("CONNECT 响应不得提前结束");
    assert!(response.starts_with(b"HTTP/1.1 200 OK"));
    client
        .write_all(b"upstream-check")
        .await
        .expect("隧道载荷必须可写");
    let mut echoed = [0_u8; 14];
    client
        .read_exact(&mut echoed)
        .await
        .expect("二级代理回显必须可读");
    assert_eq!(&echoed, b"upstream-check");
    drop(client);
    upstreamTask.await.expect("二级代理夹具必须正常退出");
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 验证透明 HTTP 接管在二级代理开启时仍以 WinDivert 原始 IP 建连，同时保留逻辑 Host。
#[tokio::test]
async fn transparentHttpKeepsOriginalIpThroughUpstreamProxy() {
    let upstreamListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("透明二级代理夹具必须绑定");
    let upstreamAddress = upstreamListener.local_addr().expect("二级代理地址必须可读");
    let upstreamTask = tokio::spawn(async move {
        let (mut stream, _) = upstreamListener
            .accept()
            .await
            .expect("必须收到透明上游连接");
        let mut bytes = Vec::new();
        let connect = readHttpMessage(&mut stream, &mut bytes)
            .await
            .expect("二级代理 CONNECT 必须有效")
            .expect("二级代理 CONNECT 不得提前结束");
        assert!(connect.starts_with(b"CONNECT 203.0.113.9:8080 HTTP/1.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .expect("二级代理 CONNECT 响应必须可写");
        bytes.clear();
        let request = readHttpMessage(&mut stream, &mut bytes)
            .await
            .expect("透明 HTTP 请求必须有效")
            .expect("透明 HTTP 请求不得提前结束");
        let expectedHost = b"logical.example:8080";
        assert!(
            request
                .windows(expectedHost.len())
                .any(|line| line == expectedHost)
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("透明 HTTP 响应必须可写");
    });
    let directory = tempfile::tempdir().expect("透明录制目录必须创建");
    let capture = createCapture(&directory).await;
    let captureForAssertion = capture.clone();
    let mut config = testConfig();
    config.upstreamProxy = UpstreamProxyConfiguration {
        enabled: true,
        protocol: UpstreamProxyProtocol::Http,
        host: upstreamAddress.ip().to_string(),
        port: upstreamAddress.port(),
        username: String::new(),
        password: String::new(),
    };
    let handler = SocksHttpTunnelHandler::new(
        config,
        capture,
        testSsl(),
        ToolPipeline::new(),
        PluginHost::disabled(),
    )
    .expect("透明 HTTP 处理器必须创建");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("透明下游夹具必须绑定");
    let mut client = TcpStream::connect(listener.local_addr().expect("下游地址必须可读"))
        .await
        .expect("透明下游客户端必须连接");
    let (server, clientAddress) = listener.accept().await.expect("透明下游必须接收");
    let handlerTask = tokio::spawn(async move {
        handler
            .servePlainHttp(
                server,
                clientAddress,
                SocksHttpTarget {
                    host: "logical.example".to_owned(),
                    port: 8080,
                    fixedAddress: Some("203.0.113.9".parse().unwrap()),
                    clientProcessName: Some("client.exe".to_owned()),
                    clientProcessId: Some(42),
                },
                CancellationToken::new(),
            )
            .await
            .expect("透明 HTTP 处理必须成功");
    });
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: logical.example:8080\r\nConnection: close\r\n\r\n")
        .await
        .expect("透明 HTTP 请求必须写入");
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("透明 HTTP 响应必须读取");
    assert!(response.ends_with(b"\r\n\r\nok"));
    handlerTask.await.expect("透明处理任务必须退出");
    upstreamTask.await.expect("透明二级代理夹具必须退出");
    let transactions = captureForAssertion
        .listMetadata()
        .await
        .expect("透明事务必须可读取");
    assert_eq!(
        transactions[0].clientProcessName.as_deref(),
        Some("client.exe")
    );
    assert_eq!(transactions[0].clientProcessId, Some(42));
}

/// 同一监听地址的第二次启动必须返回结构化绑定失败。
#[tokio::test]
async fn reportsBindConflict() {
    let firstDirectory = tempfile::tempdir().expect("临时目录必须创建成功");
    let firstCapture = createCapture(&firstDirectory).await;
    let firstProxy = startHttpProxy(
        testConfig(),
        firstCapture,
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("首个代理必须启动成功");
    let secondDirectory = tempfile::tempdir().expect("临时目录必须创建成功");
    let secondCapture = createCapture(&secondDirectory).await;
    let mut secondConfig = testConfig();
    secondConfig.listenPort = firstProxy.boundAddress().port();
    let error = match startHttpProxy(
        secondConfig,
        secondCapture,
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    {
        Ok(secondProxy) => {
            secondProxy.stop().await.expect("意外启动的代理必须停止");
            panic!("重复监听必须失败");
        }
        Err(error) => error,
    };
    assert!(matches!(error, HttpProxyError::BindFailed { .. }));
    firstProxy.stop().await.expect("首个代理必须有序停止");
}

/// stop 成功返回后立即重新绑定同一端口，证明监听资源已释放。
#[tokio::test]
async fn stopReleasesListeningPort() {
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture,
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let address = proxy.boundAddress();
    proxy.stop().await.expect("HTTP 代理必须有序停止");
    let rebound = TcpListener::bind(address)
        .await
        .expect("停止后端口必须立即可重新绑定");
    drop(rebound);
}

/// 外部守护取消源触发后必须关闭监听；随后 stop 只负责等待同一有序关闭流程。
#[tokio::test]
async fn externalCancellationReleasesListeningPort() {
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let externalShutdown = CancellationToken::new();
    let proxy = startHttpProxy(
        testConfig(),
        capture,
        testSsl(),
        ToolPipeline::new(),
        externalShutdown.clone(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let address = proxy.boundAddress();
    externalShutdown.cancel();
    let rebound = timeout(Duration::from_secs(1), async {
        loop {
            match TcpListener::bind(address).await {
                Ok(listener) => return listener,
                Err(_) => sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .expect("外部取消后端口必须释放");
    drop(rebound);
    proxy.stop().await.expect("取消后的代理任务必须正常收束");
}

/// 客户端不消费大响应时，取消必须打断已满 frame 通道并及时释放监听端口。
#[tokio::test]
async fn stopCancelsBackpressuredResponsePump() {
    let (upstreamAddress, responseStarted, upstreamTask) = startLargeResponseServer().await;
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let mut config = testConfig();
    config.shutdownTimeoutMilliseconds = 300;
    let proxy = startHttpProxy(
        config,
        capture.clone(),
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let address = proxy.boundAddress();
    let mut client = TcpStream::connect(address)
        .await
        .expect("测试客户端必须连接代理");
    let request =
        format!("GET http://{upstreamAddress}/large HTTP/1.1\r\nHost: {upstreamAddress}\r\n\r\n");
    client
        .write_all(request.as_bytes())
        .await
        .expect("大响应测试请求必须写入");
    responseStarted.await.expect("上游必须开始发送大响应");
    sleep(Duration::from_millis(100)).await;
    timeout(Duration::from_secs(1), proxy.stop())
        .await
        .expect("背压响应停止不得超出截止时间")
        .expect("背压响应必须有序停止");
    let transaction = waitForTransactionStatus(&capture, TransactionStatus::Cancelled).await;
    assert_eq!(
        transaction.error.expect("取消事务必须携带原因").code,
        "httpProxyCancelled"
    );
    let rebound = TcpListener::bind(address)
        .await
        .expect("背压停止后监听端口必须释放");
    drop(rebound);
    drop(client);
    upstreamTask.await.expect("大响应上游任务必须正常收束");
}

/// 未完成的请求头必须在配置截止时间后关闭，证明 TokioTimer 实际接入 Hyper。
#[tokio::test]
async fn closesConnectionAfterHeaderReadTimeout() {
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let mut config = testConfig();
    config.headerReadTimeoutMilliseconds = 50;
    let proxy = startHttpProxy(
        config,
        capture,
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let mut client = TcpStream::connect(proxy.boundAddress())
        .await
        .expect("测试客户端必须连接代理");
    client
        .write_all(b"GET / HTTP/1.1\r\nHost:")
        .await
        .expect("不完整请求头必须写入");
    let mut responseByte = [0_u8; 1];
    let readResult = timeout(Duration::from_millis(500), client.read(&mut responseByte))
        .await
        .expect("请求头超时后连接必须关闭");
    match readResult {
        Ok(0) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
            ) => {}
        other => panic!("请求头超时必须以 EOF 或连接重置结束，实际结果：{other:?}"),
    }
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 上游接受连接后在响应头前断开必须返回 502，并形成结构化失败事务。
#[tokio::test]
async fn mapsBrokenUpstreamToBadGateway() {
    let brokenListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("异常上游端口必须绑定");
    let brokenAddress = brokenListener.local_addr().expect("异常上游地址必须可读");
    let brokenTask = tokio::spawn(async move {
        let (stream, _) = brokenListener.accept().await.expect("必须接受代理连接");
        drop(stream);
    });
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let proxy = startHttpProxy(
        testConfig(),
        capture.clone(),
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://{}", proxy.boundAddress()))
                .expect("测试代理地址必须有效"),
        )
        .build()
        .expect("测试客户端必须构建");
    let response = client
        .get(format!("http://{brokenAddress}/"))
        .send()
        .await
        .expect("代理必须生成网关错误响应");
    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.headers()["x-proxy-error-code"],
        "httpProxyUpstreamUnavailable"
    );
    let transactions = capture.listMetadata().await.expect("事务列表必须可读");
    assert_eq!(transactions[0].status, TransactionStatus::Failed);
    assert_eq!(
        transactions[0].error.as_ref().expect("必须记录失败").code,
        "httpProxyUpstreamUnavailable"
    );
    brokenTask.await.expect("异常上游任务必须正常结束");
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}

/// 上游响应头超过请求超时必须返回 504，而不是无限占用连接槽。
#[tokio::test]
async fn mapsUpstreamTimeoutToGatewayTimeout() {
    let upstream = ToyHttpServer::start(Duration::from_secs(1)).await;
    let directory = tempfile::tempdir().expect("临时目录必须创建成功");
    let capture = createCapture(&directory).await;
    let mut config = testConfig();
    config.requestTimeoutMilliseconds = 50;
    let proxy = startHttpProxy(
        config,
        capture,
        testSsl(),
        ToolPipeline::new(),
        CancellationToken::new(),
    )
    .await
    .expect("HTTP 代理必须启动成功");
    let client = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://{}", proxy.boundAddress()))
                .expect("测试代理地址必须有效"),
        )
        .build()
        .expect("测试客户端必须构建");
    let response = timeout(
        Duration::from_secs(2),
        client
            .get(format!("http://{}/slow", upstream.address))
            .send(),
    )
    .await
    .expect("代理必须在测试截止前响应")
    .expect("代理必须生成超时响应");
    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        response.headers()["x-proxy-error-code"],
        "httpProxyUpstreamTimeout"
    );
    proxy.stop().await.expect("HTTP 代理必须有序停止");
}
