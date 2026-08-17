use std::{
    convert::Infallible,
    error::Error,
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use bytes::Bytes;
use capture_core::{
    BeginTransaction, BodyWrite, CaptureError, RecordingSession, TransactionProtocol,
    currentTimeMilliseconds,
};
use http::{HeaderMap, HeaderValue, Method, Response, StatusCode, Version};
use http_body_util::BodyExt;
use hyper::{
    Request,
    body::{Frame, Incoming},
    service::service_fn,
};
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
};
use plugin_host::{
    ConnectionMetadata, DataPlaneActionResult, PluginConnection, PluginHost, StreamDirection,
    TransportKind,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
    time::timeout,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::{
    bodyStream::{
        BodyFrameSender, BoxBodyError, CapturedBody, ProxyBody, RequestBodyCompletion,
        SharedBodyCapture, bodyFrameChannel, bodyFromBytes, captureIncomingBody, drainIncomingBody,
        emptyBody, materializeIncomingBody,
    },
    captureBridge::CaptureTransaction,
    config::HttpProxyConfig,
    connector::ProxyConnector,
    error::RequestFailure,
    pipeline::{
        PipelineContext, PipelineRequestOutcome, RequestDraft, ResponseDraft, ToolPipeline,
    },
    ssl::SslMitmManager,
    target::{
        ConnectTarget, HttpTarget, canonicalAuthority, captureHeaders, contentEncoding,
        contentType, parseConnectTarget, parseHttpTarget, parseHttpsTarget, parsePipelineTarget,
        removeHopByHopHeaders, requestHeaderBytes, responseHeaderBytes,
    },
    taskTracker::ProxyTaskTracker,
    tools::{DnsSpoofingTool, ThrottleChunkAction, ThrottleDirection, ThrottlePacer, ThrottlePlan},
    upstreamClient::HttpsUpstreamClients,
};

const responseFrameCapacity: usize = 16;

type ThrottleCompletion = oneshot::Receiver<Result<(), RequestFailure>>;

pub(crate) type HttpUpstreamClient = Client<ProxyConnector, ProxyBody>;

/// 区分共享明文连接池与验证证书的 HTTPS 连接池，避免请求代码复制两套录制逻辑。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UpstreamTransport {
    Http,
    Https,
}

/// 汇总每个连接共享的不可变依赖，避免服务回调捕获生命周期不完整的局部引用。
#[derive(Clone)]
pub(crate) struct ProxyContext {
    pub config: HttpProxyConfig,
    pub capture: RecordingSession,
    pub httpClient: HttpUpstreamClient,
    pub httpsClients: HttpsUpstreamClients,
    pub ssl: SslMitmManager,
    pub pipeline: ToolPipeline,
    pub pluginHost: PluginHost,
    pub dnsSpoofing: Arc<DnsSpoofingTool>,
    pub outbound: transport_core::OutboundConnector,
    pub cancellation: CancellationToken,
    pub tasks: ProxyTaskTracker,
    /// 当前 HTTP 监听服务周期的套接字指标；请求派生任务只共享引用，不直接修改生命周期计数。
    pub metrics: crate::HttpRuntimeMetrics,
    /// 当前透明连接的本机进程名称；普通 HTTP/SOCKS 客户端保持为空。
    pub clientProcessName: Option<String>,
    /// 当前透明连接的本机进程编号，用于把录制事务关联回进程选择器。
    pub clientProcessId: Option<u32>,
}

/// 描述反向代理将本地 origin-form 请求固定映射到的上游；该结构不允许客户端提供协议或 authority。
#[derive(Clone)]
pub(crate) struct ReverseRequestTarget {
    pub scheme: &'static str,
    pub host: String,
    pub port: u16,
    pub preserveHostHeader: bool,
    pub stripPathPrefix: String,
}

/// 区分默认零拷贝请求流与正文工具要求的有界物化副本，防止普通代理路径承担改包内存成本。
enum RequestBodySource {
    Streaming(Incoming),
    Buffered(Bytes),
}

/// 保存请求正文在转发完成时的可用来源；流式路径必须等待 Hyper 消费结束，物化路径可立即提交一致副本。
enum RequestCaptureSource {
    Streaming {
        capture: SharedBodyCapture,
        completion: RequestBodyCompletion,
    },
    Buffered(Bytes),
}

/// 聚合响应泵完成事务所需的请求侧状态，避免参数列表随录制字段扩张。
struct RequestCaptureContext {
    source: RequestCaptureSource,
    requestContentType: String,
    requestEncoding: String,
    throttleCompletion: Option<ThrottleCompletion>,
}

impl RequestCaptureContext {
    /// 在线上请求体确定后生成 capture-core 写入描述；流式请求在上游提前响应时仍等待 CompletionBody 释放。
    async fn complete(self, cancellation: &CancellationToken) -> Result<BodyWrite, RequestFailure> {
        let Self {
            source,
            requestContentType,
            requestEncoding,
            throttleCompletion,
        } = self;
        if let Some(completion) = throttleCompletion {
            waitForThrottleCompletion(completion, cancellation).await?;
        }
        let captured = match source {
            RequestCaptureSource::Streaming {
                capture,
                completion,
            } => {
                completion.wait(cancellation).await?;
                capture.snapshot()
            }
            RequestCaptureSource::Buffered(bytes) => capturedBodyFromBytes(bytes),
        };
        Ok(bodyWrite(captured, requestContentType, requestEncoding))
    }
}

/// 聚合响应侧事务和请求镜像，保持异步正文泵只有一个明确的完成上下文。
struct ResponseCaptureContext {
    transaction: Option<CaptureTransaction>,
    request: RequestCaptureContext,
    pipeline: PipelineContext,
}

/// 路由普通 HTTP 与 CONNECT；请求级失败转换为稳定状态码而不终止客户端连接服务。
pub(crate) async fn forwardRequest(
    request: Request<Incoming>,
    clientAddress: SocketAddr,
    context: ProxyContext,
) -> Result<Response<ProxyBody>, Infallible> {
    let response = if request.method() == Method::CONNECT {
        forwardConnect(request, clientAddress, context).await
    } else {
        forwardHttp(request, clientAddress, context).await
    };
    Ok(response)
}

/// 流式转发明文 HTTP，请求和响应正文只保留配置允许的有界前缀。
pub(crate) async fn forwardHttp(
    request: Request<Incoming>,
    clientAddress: SocketAddr,
    context: ProxyContext,
) -> Response<ProxyBody> {
    let target = match parseHttpTarget(&request) {
        Ok(target) => target,
        Err(failure) => return failureResponse(failure),
    };
    forwardDecodedHttp(
        request,
        clientAddress,
        context,
        target,
        UpstreamTransport::Http,
        false,
    )
    .await
}

/// 转发已解析的 HTTP 或 HTTPS 明文消息；两种传输共用正文背压、录制和失败语义。
pub(crate) async fn forwardDecodedHttp(
    request: Request<Incoming>,
    clientAddress: SocketAddr,
    context: ProxyContext,
    target: HttpTarget,
    transport: UpstreamTransport,
    preserveHostHeader: bool,
) -> Response<ProxyBody> {
    let (mut requestParts, incomingBody) = request.into_parts();
    let mut pipelineContext = PipelineContext::new(
        clientAddress.to_string(),
        target.location.clone(),
        RequestDraft::fromParts(&requestParts),
    );
    pipelineContext.bindClientProcess(context.clientProcessName.clone(), context.clientProcessId);
    // 普通代理的 origin-form 在这里已经解析为绝对 URI，后续 Map Remote 与 Rewrite 只面对一种目标表示。
    pipelineContext.request.uri = target.upstreamUri.clone();
    pipelineContext.flags.mitmDecrypted = transport == UpstreamTransport::Https;
    let captureInput = BeginTransaction {
        protocol: match transport {
            UpstreamTransport::Http => TransactionProtocol::Http,
            UpstreamTransport::Https => TransactionProtocol::Https,
        },
        method: pipelineContext.request.method.as_str().to_owned(),
        location: pipelineContext.originalLocation.clone(),
        clientAddress: clientAddress.to_string(),
        clientProcessName: context.clientProcessName.clone(),
        clientProcessId: context.clientProcessId,
        contentType: contentType(&pipelineContext.request.headers),
        startAtMilliseconds: currentTimeMilliseconds(),
    };
    let mut capture = beginCapture(&context.capture, captureInput).await;
    if let Some(transaction) = capture.as_ref() {
        pipelineContext.bindTransaction(
            transaction.transactionId().to_owned(),
            transaction.recordingSessionId().to_owned(),
        );
    }
    let mut requestBodySource = if context.pipeline.requiresRequestBody() {
        let body = match materializeIncomingBody(
            incomingBody,
            context.config.maxCaptureBodyBytes,
            &context.cancellation,
        )
        .await
        {
            Ok(body) => body,
            Err(failure) => {
                failCapture(capture, failure).await;
                return failureResponse(failure);
            }
        };
        pipelineContext.request.body = Some(body.clone());
        RequestBodySource::Buffered(body)
    } else {
        RequestBodySource::Streaming(incomingBody)
    };
    let requestOutcome = match context.pipeline.runRequest(&mut pipelineContext).await {
        Ok(outcome) => outcome,
        Err(error) => {
            logPipelineError(&error);
            failCapture(capture, RequestFailure::UpstreamProtocol).await;
            return failureResponse(RequestFailure::UpstreamProtocol);
        }
    };
    if context.pipeline.requiresRequestBody() {
        let Some(body) = pipelineContext.request.body.clone() else {
            failCapture(capture, RequestFailure::UpstreamProtocol).await;
            return failureResponse(RequestFailure::UpstreamProtocol);
        };
        synchronizeBodyLength(&mut pipelineContext.request.headers, body.len());
        requestBodySource = RequestBodySource::Buffered(body);
    }
    let finalRequestMethod = pipelineContext.request.method.as_str().to_owned();
    let finalRequestLocation = pipelineContext.location.clone();
    if let Some(transaction) = capture.take() {
        capture = retainCapture(transaction, |transaction| async move {
            transaction
                .storeFinalRequestIdentity(finalRequestMethod, finalRequestLocation)
                .await
        })
        .await;
    }
    capture = retainPipelineState(capture, &pipelineContext).await;
    let requestContentType = contentType(&pipelineContext.request.headers);
    let requestEncoding = contentEncoding(&pipelineContext.request.headers);
    let requestHeaders = captureHeaders(&pipelineContext.request.headers);
    let requestHeaderBytes = requestHeaderBytes(
        &pipelineContext.request.method,
        &pipelineContext.request.uri,
        pipelineContext.request.version,
        &pipelineContext.request.headers,
    );
    if let Some(transaction) = capture.take() {
        capture = retainCapture(transaction, |transaction| async move {
            transaction
                .storeRequestHeaders(requestHeaders, requestHeaderBytes)
                .await
        })
        .await;
    }
    if requestOutcome != PipelineRequestOutcome::Forward {
        return forwardSyntheticResponse(
            requestBodySource,
            capture,
            pipelineContext,
            context,
            requestOutcome,
        )
        .await;
    }
    let target = match parsePipelineTarget(&pipelineContext.request) {
        Ok(target) => target,
        Err(failure) => {
            failCapture(capture, failure).await;
            return failureResponse(failure);
        }
    };
    if targetsProxyListener(&target.location.host, target.location.port, &context.config) {
        failCapture(capture, RequestFailure::LoopDetected).await;
        return failureResponse(RequestFailure::LoopDetected);
    }
    let transport = match upstreamTransportForTarget(&target) {
        Ok(transport) => transport,
        Err(failure) => {
            failCapture(capture, failure).await;
            return failureResponse(failure);
        }
    };
    let upstreamLocation = target.location.clone();
    requestParts.method = pipelineContext.request.method.clone();
    requestParts.uri = target.upstreamUri;
    requestParts.version = upstreamRequestVersion(pipelineContext.request.version);
    requestParts.headers = pipelineContext.request.headers.clone();
    removeHopByHopHeaders(&mut requestParts.headers);
    // absolute-form 的 Host 可能由客户端伪造且与目标 URI 不一致；上游只可信任已解析 authority。
    if !preserveHostHeader {
        requestParts
            .headers
            .insert(http::header::HOST, target.hostHeader);
    }
    let (requestBody, mut requestCaptureContext) =
        prepareRequestBody(requestBodySource, requestContentType, requestEncoding);
    let (requestBody, throttleCompletion) = match createThrottledBody(
        requestBody,
        pipelineContext.requestThrottlePlan.take(),
        ThrottleDirection::Upload,
        &context,
    ) {
        Ok(body) => body,
        Err(failure) => {
            failCapture(capture, failure).await;
            return failureResponse(failure);
        }
    };
    requestCaptureContext.throttleCompletion = throttleCompletion;
    let upstreamRequest = Request::from_parts(requestParts, requestBody);

    if let Some(transaction) = capture.take() {
        capture = retainCapture(transaction, |transaction| async move {
            transaction.markRequestSent().await
        })
        .await;
    }

    let upstreamResponse = tokio::select! {
        biased;
        () = context.cancellation.cancelled() => {
            failCapture(capture, RequestFailure::Cancelled).await;
            return failureResponse(RequestFailure::Cancelled);
        }
        result = timeout(context.config.requestTimeout(), async {
            match transport {
                UpstreamTransport::Http => context.httpClient.request(upstreamRequest).await,
                UpstreamTransport::Https => context.httpsClients.request(&upstreamLocation, upstreamRequest).await,
            }
        }) => result,
    };
    let upstreamResponse = match upstreamResponse {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            let failure = classifyUpstreamError(&error, transport);
            storeRequestBeforeFailure(
                capture.as_ref(),
                requestCaptureContext,
                &context.cancellation,
            )
            .await;
            failCapture(capture, failure).await;
            return failureResponse(failure);
        }
        Err(_) => {
            storeRequestBeforeFailure(
                capture.as_ref(),
                requestCaptureContext,
                &context.cancellation,
            )
            .await;
            failCapture(capture, RequestFailure::UpstreamTimeout).await;
            return failureResponse(RequestFailure::UpstreamTimeout);
        }
    };

    let captureContext = ResponseCaptureContext {
        transaction: capture,
        request: requestCaptureContext,
        pipeline: pipelineContext,
    };
    buildStreamingResponse(upstreamResponse, captureContext, context).await
}

/// 将下游消息版本转换为稳定的上游请求版本；HTTP/2 帧由下游连接状态机负责，不应强迫目标站也支持 h2。
///
/// 真实站点可能只对部分入口开放 HTTP/2。若把下游 `HTTP_2` 原样交给 Hyper 连接池，客户端会把
/// h2 作为硬约束，目标只协商 HTTP/1.1 时就错误返回 502。统一转换为语义兼容的 HTTP/1.1 后，
/// 下游仍收到 HTTP/2 响应帧，同时代理可以连接 HTTP/1.1 与 HTTP/2 站点，不需要缓存或重放正文。
fn upstreamRequestVersion(downstreamVersion: Version) -> Version {
    match downstreamVersion {
        Version::HTTP_2 | Version::HTTP_3 => Version::HTTP_11,
        version => version,
    }
}

/// 将反向代理入口的请求改写为固定上游后复用标准 HTTP 事务、工具和录制流水线。
pub(crate) async fn forwardReverseRequest(
    request: Request<Incoming>,
    clientAddress: SocketAddr,
    context: ProxyContext,
    target: ReverseRequestTarget,
) -> Response<ProxyBody> {
    if request.method() == Method::CONNECT {
        return failureResponse(RequestFailure::InvalidRequest);
    }
    let (mut parts, body) = request.into_parts();
    let pathAndQuery = match reversePathAndQuery(&parts.uri, &target.stripPathPrefix) {
        Some(value) => value,
        None => return failureResponse(RequestFailure::InvalidRequest),
    };
    let defaultPort = match target.scheme {
        "http" => 80,
        "https" => 443,
        _ => return failureResponse(RequestFailure::UnsupportedScheme),
    };
    // 反向入口和普通代理共用 authority 规范化规则，确保 HTTP/2 `:authority` 与后续 Host
    // 在默认端口上保持完全相同的文本，严格上游不会再把代理生成的请求判为冲突。
    let authority = match canonicalAuthority(&target.host, target.port, defaultPort) {
        Ok(value) => value,
        Err(failure) => return failureResponse(failure),
    };
    let uri = match http::Uri::builder()
        .scheme(target.scheme)
        .authority(authority)
        .path_and_query(pathAndQuery)
        .build()
    {
        Ok(value) => value,
        Err(_) => return failureResponse(RequestFailure::InvalidRequest),
    };
    parts.uri = uri;
    let requestDraft = RequestDraft::fromParts(&parts);
    let request = Request::from_parts(parts, body);
    let parsedTarget = match parsePipelineTarget(&requestDraft) {
        Ok(value) => value,
        Err(failure) => return failureResponse(failure),
    };
    let transport = match target.scheme {
        "http" => UpstreamTransport::Http,
        "https" => UpstreamTransport::Https,
        _ => unreachable!("上游协议已在 URI 构造前完成校验"),
    };
    forwardDecodedHttp(
        request,
        clientAddress,
        context,
        parsedTarget,
        transport,
        target.preserveHostHeader,
    )
    .await
}

/// 从本地入口保留绝对 path/query，并在配置要求时仅剥离完整前缀边界。
fn reversePathAndQuery(uri: &http::Uri, stripPathPrefix: &str) -> Option<String> {
    if uri.scheme().is_some() || uri.authority().is_some() {
        return None;
    }
    let path = uri.path();
    let strippedPath = if stripPathPrefix.is_empty() {
        path
    } else if path == stripPathPrefix {
        "/"
    } else {
        let prefix = format!("{stripPathPrefix}/");
        path.strip_prefix(&prefix)?
    };
    let normalizedPath = if strippedPath.starts_with('/') {
        strippedPath.to_owned()
    } else {
        format!("/{strippedPath}")
    };
    Some(uri.query().map_or(normalizedPath.clone(), |query| {
        format!("{normalizedPath}?{query}")
    }))
}

/// 建立 CONNECT 上游后才返回 200，并在独立受跟踪任务中执行双向拷贝。
///
/// 运行上下文：请求 authority 已严格解析；自环、取消和上游连接失败都在响应前发生。
/// 失败语义：只要目标可确定，就先把稳定错误码写入事务再返回代理错误响应，避免失败只在客户端可见。
async fn forwardConnect(
    mut request: Request<Incoming>,
    clientAddress: SocketAddr,
    context: ProxyContext,
) -> Response<ProxyBody> {
    let target = match parseConnectTarget(&request) {
        Ok(target) => target,
        Err(failure) => return failureResponse(failure),
    };
    if targetsProxyListener(&target.host, target.port, &context.config) {
        recordConnectFailure(
            &context,
            &target,
            clientAddress,
            RequestFailure::LoopDetected,
        )
        .await;
        return failureResponse(RequestFailure::LoopDetected);
    }
    if context.ssl.shouldIntercept(&target.location) {
        return forwardInterceptedConnect(request, clientAddress, context, target).await;
    }
    let captureInput = BeginTransaction {
        protocol: TransactionProtocol::Tunnel,
        method: Method::CONNECT.as_str().to_owned(),
        location: target.location,
        clientAddress: clientAddress.to_string(),
        clientProcessName: context.clientProcessName.clone(),
        clientProcessId: context.clientProcessId,
        contentType: String::new(),
        startAtMilliseconds: currentTimeMilliseconds(),
    };
    let mut capture = beginCapture(&context.capture, captureInput).await;
    if let Some(transaction) = capture.take() {
        let headers = captureHeaders(request.headers());
        let headerBytes = requestHeaderBytes(
            request.method(),
            request.uri(),
            request.version(),
            request.headers(),
        );
        capture = retainCapture(transaction, |transaction| async move {
            transaction.storeRequestHeaders(headers, headerBytes).await
        })
        .await;
    }

    let upstream = tokio::select! {
        biased;
        () = context.cancellation.cancelled() => {
            failCapture(capture, RequestFailure::Cancelled).await;
            return failureResponse(RequestFailure::Cancelled);
        }
        result = connectTcpTarget(&context, &target.host, target.port) => result
    };
    let upstream = match upstream {
        Ok(stream) => stream,
        Err(transport_core::OutboundConnectError::Timeout) => {
            failCapture(capture, RequestFailure::UpstreamTimeout).await;
            return failureResponse(RequestFailure::UpstreamTimeout);
        }
        Err(_) => {
            failCapture(capture, RequestFailure::UpstreamUnavailable).await;
            return failureResponse(RequestFailure::UpstreamUnavailable);
        }
    };

    if let Some(transaction) = capture.take() {
        let tunnelResponseHeaderBytes =
            responseHeaderBytes(StatusCode::OK, Version::HTTP_11, &HeaderMap::new());
        capture = retainCapture(transaction, |transaction| async move {
            transaction.markTunnelConnected().await?;
            transaction
                .storeResponseHeaders(Vec::new(), tunnelResponseHeaderBytes, 200)
                .await
        })
        .await;
    }
    let upgrade = hyper::upgrade::on(&mut request);
    let tunnelCancellation = context.cancellation.clone();
    let pluginHost = context.pluginHost.clone();
    let pluginConnection = pluginHost.openConnection(ConnectionMetadata {
        transport: TransportKind::Tcp,
        clientAddress: clientAddress.to_string(),
        targetHost: target.host.clone(),
        targetPort: target.port,
    });
    context.tasks.spawn(async move {
        let upgraded = tokio::select! {
            biased;
            () = tunnelCancellation.cancelled() => {
                failCapture(capture, RequestFailure::Cancelled).await;
                pluginHost.closeDataPlaneConnection(pluginConnection).await;
                return;
            }
            result = upgrade => result,
        };
        let upgraded = match upgraded {
            Ok(upgraded) => upgraded,
            Err(_) => {
                failCapture(capture, RequestFailure::UpgradeFailed).await;
                pluginHost.closeDataPlaneConnection(pluginConnection).await;
                return;
            }
        };
        let downstream = TokioIo::new(upgraded);
        let copyResult = tokio::select! {
            biased;
            () = tunnelCancellation.cancelled() => {
                failCapture(capture, RequestFailure::Cancelled).await;
                pluginHost.closeDataPlaneConnection(pluginConnection).await;
                return;
            }
            result = relayTunnel(
                downstream,
                upstream,
                pluginHost.clone(),
                pluginConnection.clone(),
            ) => result,
        };
        pluginHost.closeDataPlaneConnection(pluginConnection).await;
        match copyResult {
            Ok((clientToRemoteBytes, remoteToClientBytes)) => {
                if let Some(transaction) = capture
                    && let Err(error) = transaction
                        .completeTunnel(clientToRemoteBytes, remoteToClientBytes)
                        .await
                {
                    logCaptureError(&error);
                    failCapture(Some(transaction), RequestFailure::CaptureFailed).await;
                }
            }
            Err(_) => failCapture(capture, RequestFailure::ClientDisconnected).await,
        }
    });

    let mut response = Response::new(emptyBody());
    *response.status_mut() = StatusCode::OK;
    response
}

/// 判断上游目标是否重新指向当前代理监听器；该检查在请求工具改写完成后执行，阻止显式 HTTP、CONNECT 或错误代理链把流量递归送回自身。
/// 端口不同必定不是当前监听器；端口相同时仅把本机可绑定 IP、通配地址和 localhost 视为本机，远端同端口不受影响。
fn targetsProxyListener(host: &str, port: u16, config: &HttpProxyConfig) -> bool {
    let listenAddress = config.listenAddress();
    if port != listenAddress.port() {
        return false;
    }
    let normalizedHost = host.trim_matches(['[', ']']);
    if normalizedHost.eq_ignore_ascii_case("localhost") {
        return listenAddress.ip().is_unspecified() || listenAddress.ip().is_loopback();
    }
    let Ok(targetIp) = normalizedHost.parse::<IpAddr>() else {
        return false;
    };
    if !listenAddress.ip().is_unspecified() {
        return targetIp == listenAddress.ip();
    }
    targetIp.is_unspecified() || localIpCanBeBound(targetIp)
}

/// 通过绑定临时零端口确认字面 IP 是否属于本机；只创建未监听套接字，不发起网络访问，失败即按远端地址处理。
fn localIpCanBeBound(targetIp: IpAddr) -> bool {
    let socket = match targetIp {
        IpAddr::V4(_) => tokio::net::TcpSocket::new_v4(),
        IpAddr::V6(_) => tokio::net::TcpSocket::new_v6(),
    };
    socket
        .and_then(|socket| socket.bind(SocketAddr::new(targetIp, 0)))
        .is_ok()
}

/// 在 CONNECT 隧道双向读取处同步分发 Native Hook；未匹配插件时仍是两个读写泵，不跨进程复制数据。
async fn relayTunnel<D, U>(
    downstream: D,
    upstream: U,
    pluginHost: PluginHost,
    pluginConnection: PluginConnection,
) -> io::Result<(u64, u64)>
where
    D: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (downstreamReader, downstreamWriter) = tokio::io::split(downstream);
    let (upstreamReader, upstreamWriter) = tokio::io::split(upstream);
    let clientToServer = pumpTunnel(
        downstreamReader,
        upstreamWriter,
        pluginHost.clone(),
        pluginConnection.clone(),
        StreamDirection::ClientToServer,
    );
    let serverToClient = pumpTunnel(
        upstreamReader,
        downstreamWriter,
        pluginHost,
        pluginConnection,
        StreamDirection::ServerToClient,
    );
    tokio::try_join!(clientToServer, serverToClient)
}

/// 转发 CONNECT 单方向字节并在写入前应用插件决定；Hold、Drop 不写入对端，Close 终止隧道。
async fn pumpTunnel<R, W>(
    mut reader: R,
    mut writer: W,
    pluginHost: PluginHost,
    pluginConnection: PluginConnection,
    direction: StreamDirection,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0_u8; 65_536];
    let mut total = 0_u64;
    loop {
        let length = reader.read(&mut buffer).await?;
        if length == 0 {
            writer.shutdown().await?;
            return Ok(total);
        }
        match pluginHost
            .processDataPlaneBytes(&pluginConnection, direction, buffer[..length].to_vec())
            .await
        {
            DataPlaneActionResult::Forward { bytes } => {
                writer.write_all(&bytes).await?;
                writer.flush().await?;
                total = total.saturating_add(bytes.len() as u64);
            }
            DataPlaneActionResult::Hold | DataPlaneActionResult::Drop => continue,
            DataPlaneActionResult::Close => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "插件请求关闭 CONNECT 隧道",
                ));
            }
        }
    }
}

/// 将命中规则的 CONNECT 升级为下游 TLS 服务，并把解密后的每条 HTTP/1.1 消息送入统一转发链。
///
/// 运行上下文：目标已解析并命中 SSL 规则；上游探针、Hyper 升级与下游握手依次建立边界。
/// 失败语义：每个握手前失败都记录结构化 CONNECT 终态，异步升级后的失败使用同一入口，禁止静默隐藏。
async fn forwardInterceptedConnect(
    mut request: Request<Incoming>,
    clientAddress: SocketAddr,
    context: ProxyContext,
    target: ConnectTarget,
) -> Response<ProxyBody> {
    let upstreamProbe = tokio::select! {
        biased;
        () = context.cancellation.cancelled() => {
            recordConnectFailure(
                &context,
                &target,
                clientAddress,
                RequestFailure::Cancelled,
            ).await;
            return failureResponse(RequestFailure::Cancelled);
        }
        result = connectTcpTarget(&context, &target.host, target.port) => result,
    };
    let upstreamProbe = match upstreamProbe {
        Ok(stream) => stream,
        Err(transport_core::OutboundConnectError::Timeout) => {
            recordConnectFailure(
                &context,
                &target,
                clientAddress,
                RequestFailure::UpstreamTimeout,
            )
            .await;
            return failureResponse(RequestFailure::UpstreamTimeout);
        }
        Err(_) => {
            recordConnectFailure(
                &context,
                &target,
                clientAddress,
                RequestFailure::UpstreamUnavailable,
            )
            .await;
            return failureResponse(RequestFailure::UpstreamUnavailable);
        }
    };
    let tlsConfiguration = match context.ssl.downstreamServerConfiguration(&target.host) {
        Ok(configuration) => configuration,
        Err(_) => {
            recordConnectFailure(
                &context,
                &target,
                clientAddress,
                RequestFailure::DownstreamTlsHandshake,
            )
            .await;
            return failureResponse(RequestFailure::DownstreamTlsHandshake);
        }
    };
    let upgrade = hyper::upgrade::on(&mut request);
    let taskContext = context.clone();
    context.tasks.spawn(async move {
        let upgraded = tokio::select! {
            biased;
            () = taskContext.cancellation.cancelled() => return,
            result = upgrade => result,
        };
        let upgraded = match upgraded {
            Ok(upgraded) => upgraded,
            Err(_) => {
                recordConnectFailure(
                    &taskContext,
                    &target,
                    clientAddress,
                    RequestFailure::UpgradeFailed,
                )
                .await;
                return;
            }
        };
        let acceptor = TlsAcceptor::from(tlsConfiguration);
        let downstreamTls = tokio::select! {
            biased;
            () = taskContext.cancellation.cancelled() => return,
            result = timeout(
                taskContext.config.connectTimeout(),
                acceptor.accept(TokioIo::new(upgraded)),
            ) => result,
        };
        let downstreamTls = match downstreamTls {
            Ok(Ok(stream)) => {
                taskContext.ssl.recordHandshakeSuccess();
                stream
            }
            Ok(Err(_)) | Err(_) => {
                taskContext.ssl.recordHandshakeFailure();
                recordConnectFailure(
                    &taskContext,
                    &target,
                    clientAddress,
                    RequestFailure::DownstreamTlsHandshake,
                )
                .await;
                return;
            }
        };
        // CONNECT 返回 200 前保留一次真实 TCP 建连结果；下游握手完成后由 HTTPS 连接池建立可复用 TLS 上游。
        drop(upstreamProbe);
        let serviceContext = taskContext.clone();
        let connectHost = target.host.clone();
        let connectPort = target.port;
        let service = service_fn(move |decryptedRequest: Request<Incoming>| {
            let context = serviceContext.clone();
            let host = connectHost.clone();
            async move {
                let response = match parseHttpsTarget(&decryptedRequest, &host, connectPort) {
                    Ok(httpTarget) => {
                        forwardDecodedHttp(
                            decryptedRequest,
                            clientAddress,
                            context,
                            httpTarget,
                            UpstreamTransport::Https,
                            false,
                        )
                        .await
                    }
                    Err(failure) => failureResponse(failure),
                };
                Ok::<_, Infallible>(response)
            }
        });
        let mut builder = auto::Builder::new(TokioExecutor::new());
        builder
            .http1()
            .keep_alive(true)
            .max_buf_size(taskContext.config.maxHeaderBytes)
            .timer(TokioTimer::new())
            .header_read_timeout(taskContext.config.headerReadTimeout());
        let connection = builder.serve_connection(TokioIo::new(downstreamTls), service);
        tokio::pin!(connection);
        tokio::select! {
            result = &mut connection => {
                if result.is_err() {
                    tracing::debug!(
                        errorCode = "sslMitmClientProtocolClosed",
                        messageKey = "error.httpProxy.clientDisconnected"
                    );
                }
            }
            () = taskContext.cancellation.cancelled() => {
                connection.as_mut().graceful_shutdown();
                let _ = timeout(
                    taskContext.config.connectionDrainTimeout(),
                    &mut connection,
                )
                .await;
            }
        }
    });
    let mut response = Response::new(emptyBody());
    *response.status_mut() = StatusCode::OK;
    response
}

/// 为 CONNECT 裸隧道和 TLS 探针应用代理进程内 DNS 映射，同时保持 HTTP Host 与 TLS SNI 使用原域名。
///
/// 运行上下文：Hyper 连接池之外的直接 TCP 建连必须显式调用该函数，否则同一规则会只对普通请求生效。
/// 失败语义：直连规则命中时连接指定 IP；二级代理保留原域名；连接器的超时与协议错误原样交给上层映射。
async fn connectTcpTarget(
    context: &ProxyContext,
    host: &str,
    port: u16,
) -> Result<TcpStream, transport_core::OutboundConnectError> {
    // 二级代理必须接收原始域名才能在代理侧解析；仅直连路径应用进程内 DNS 映射。
    let connectHost = if context.outbound.usesUpstreamProxy() {
        host.to_owned()
    } else {
        context
            .dnsSpoofing
            .resolveIp(host)
            .map_or_else(|| host.to_owned(), |address| address.to_string())
    };
    context.outbound.connect(&connectHost, port).await
}

/// 为 CONNECT 在请求事务形成前的失败建立可见终态；成功路径仍只展示解密后的 HTTPS 请求或真实隧道。
///
/// 运行上下文：目标 authority 已完成严格解析，但失败可能发生在循环检查、上游探针、升级或下游 TLS 握手阶段。
/// 失败语义：录制暂停、并发清空或录制写入错误不得改变发给代理客户端的原始失败响应。
async fn recordConnectFailure(
    context: &ProxyContext,
    target: &ConnectTarget,
    clientAddress: SocketAddr,
    failure: RequestFailure,
) {
    let input = BeginTransaction {
        protocol: TransactionProtocol::Tunnel,
        method: Method::CONNECT.as_str().to_owned(),
        location: target.location.clone(),
        clientAddress: clientAddress.to_string(),
        clientProcessName: context.clientProcessName.clone(),
        clientProcessId: context.clientProcessId,
        contentType: String::new(),
        startAtMilliseconds: currentTimeMilliseconds(),
    };
    let capture = beginCapture(&context.capture, input).await;
    failCapture(capture, failure).await;
}

/// 将合成响应送入与上游响应相同的响应工具和录制链；短路路径必须排空请求正文以保持客户端 keep-alive 边界。
/// 将请求正文来源转换为上游可消费的 body，并同时建立录制所需的完成上下文；普通路径保持流式，正文工具路径复用已物化字节。
fn prepareRequestBody(
    source: RequestBodySource,
    requestContentType: String,
    requestEncoding: String,
) -> (ProxyBody, RequestCaptureContext) {
    let (body, source) = match source {
        RequestBodySource::Streaming(incoming) => {
            let (body, capture, completion) = captureIncomingBody(incoming);
            (
                body,
                RequestCaptureSource::Streaming {
                    capture,
                    completion,
                },
            )
        }
        RequestBodySource::Buffered(bytes) => (
            bodyFromBytes(bytes.clone()),
            RequestCaptureSource::Buffered(bytes),
        ),
    };
    (
        body,
        RequestCaptureContext {
            source,
            requestContentType,
            requestEncoding,
            throttleCompletion: None,
        },
    )
}

/// 为命中节流规则的正文创建独立的异步帧生产者；上传和下载共享相同的背压通道，但各自持有独立令牌桶。
fn createThrottledBody(
    body: ProxyBody,
    plan: Option<ThrottlePlan>,
    direction: ThrottleDirection,
    context: &ProxyContext,
) -> Result<(ProxyBody, Option<ThrottleCompletion>), RequestFailure> {
    let Some(plan) = plan else {
        return Ok((body, None));
    };
    let pacer = plan
        .createPacer(direction)
        .map_err(|_| RequestFailure::UpstreamProtocol)?;
    let (sender, throttledBody) = bodyFrameChannel(responseFrameCapacity);
    let (completionSender, completionReceiver) = oneshot::channel();
    let cancellation = context.cancellation.clone();
    context.tasks.spawn(async move {
        let result = pumpThrottledBody(body, sender, pacer, &cancellation).await;
        let _ = completionSender.send(result);
    });
    Ok((throttledBody, Some(completionReceiver)))
}

/// 从命中的计划创建单方向调度器；计划在请求或响应钩子中已完成配置与 Location 校验，此处只建立本次传输的独立状态。
fn createThrottlePacer(
    plan: Option<ThrottlePlan>,
    direction: ThrottleDirection,
) -> Result<Option<ThrottlePacer>, RequestFailure> {
    plan.map(|plan| {
        plan.createPacer(direction)
            .map_err(|_| RequestFailure::UpstreamProtocol)
    })
    .transpose()
}

/// 消费一个正文流并按 MTU、令牌桶、首包延迟与可靠性决策重组数据帧；任一丢包都以 body 错误终止该 HTTP 方向，绝不交付残缺正文。
async fn pumpThrottledBody(
    mut body: ProxyBody,
    sender: BodyFrameSender,
    mut pacer: ThrottlePacer,
    cancellation: &CancellationToken,
) -> Result<(), RequestFailure> {
    loop {
        let frame = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(RequestFailure::Cancelled),
            frame = body.frame() => frame,
        };
        match frame {
            Some(Ok(frame)) => {
                if let Some(bytes) = frame.data_ref() {
                    forwardThrottledBytes(&sender, bytes.clone(), &mut pacer, cancellation).await?;
                } else {
                    sendBodyFrame(&sender, Ok(frame), cancellation).await?;
                }
            }
            Some(Err(error)) => {
                return match sendBodyFrame(&sender, Err(error), cancellation).await {
                    Ok(()) => Err(RequestFailure::UpstreamProtocol),
                    Err(failure) => Err(failure),
                };
            }
            None => return Ok(()),
        }
    }
}

/// 将单个数据帧拆分为不超过 MTU 的子帧并串行等待带宽配额；可用性决策为丢弃时向消费端发送明确 body 错误。
async fn forwardThrottledBytes(
    sender: &BodyFrameSender,
    bytes: Bytes,
    pacer: &mut ThrottlePacer,
    cancellation: &CancellationToken,
) -> Result<(), RequestFailure> {
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk = pacer
            .nextChunk(bytes.len() - offset)
            .await
            .map_err(|_| RequestFailure::UpstreamProtocol)?
            .ok_or(RequestFailure::UpstreamProtocol)?;
        let nextOffset = offset
            .checked_add(chunk.byteCount)
            .ok_or(RequestFailure::UpstreamProtocol)?;
        if chunk.byteCount == 0 || nextOffset > bytes.len() {
            return Err(RequestFailure::UpstreamProtocol);
        }
        match chunk.action {
            ThrottleChunkAction::Forward => {
                sendBodyFrame(
                    sender,
                    Ok(Frame::data(bytes.slice(offset..nextOffset))),
                    cancellation,
                )
                .await?;
            }
            ThrottleChunkAction::Drop => {
                let error = Box::new(io::Error::other("throttlingDroppedFrame"));
                return match sendBodyFrame(sender, Err(error), cancellation).await {
                    Ok(()) => Err(RequestFailure::UpstreamProtocol),
                    Err(failure) => Err(failure),
                };
            }
        }
        offset = nextOffset;
    }
    Ok(())
}

/// 等待上传节流生产者结束，确保即使上游提前返回响应，丢包或取消也不会被录制层误记为成功事务。
async fn waitForThrottleCompletion(
    completion: ThrottleCompletion,
    cancellation: &CancellationToken,
) -> Result<(), RequestFailure> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(RequestFailure::Cancelled),
        result = completion => result.unwrap_or(Err(RequestFailure::UpstreamProtocol)),
    }
}

/// 排空短路请求的原始正文或复用已物化正文，确保 HTTP keep-alive 边界和录制正文始终对应同一份请求数据。
async fn captureShortCircuitRequestBody(
    source: RequestBodySource,
    cancellation: &CancellationToken,
) -> Result<CapturedBody, RequestFailure> {
    match source {
        RequestBodySource::Streaming(incoming) => drainIncomingBody(incoming, cancellation).await,
        RequestBodySource::Buffered(bytes) => Ok(capturedBodyFromBytes(bytes)),
    }
}

/// 将完整字节正文转换为 capture-core 的正文描述；该路径只用于已通过工具物化上限校验的缓冲数据。
fn capturedBodyFromBytes(bytes: Bytes) -> CapturedBody {
    CapturedBody {
        originalBytes: bytes.len() as u64,
        bytes: bytes.to_vec(),
    }
}

/// 正文被工具替换后同步 HTTP 长度语义，移除分块传输避免与新的 Content-Length 冲突。
fn synchronizeBodyLength(headers: &mut HeaderMap, bodyLength: usize) {
    headers.remove(http::header::CONTENT_LENGTH);
    headers.remove(http::header::TRANSFER_ENCODING);
    let length = HeaderValue::try_from(bodyLength.to_string())
        .expect("usize 十进制长度必须能够写入 HTTP Content-Length");
    headers.insert(http::header::CONTENT_LENGTH, length);
}

async fn forwardSyntheticResponse(
    request: RequestBodySource,
    mut capture: Option<CaptureTransaction>,
    mut pipeline: PipelineContext,
    context: ProxyContext,
    outcome: PipelineRequestOutcome,
) -> Response<ProxyBody> {
    let requestContentType = contentType(&pipeline.request.headers);
    let requestEncoding = contentEncoding(&pipeline.request.headers);
    let requestBody = match captureShortCircuitRequestBody(request, &context.cancellation).await {
        Ok(body) => body,
        Err(failure) => {
            failCapture(capture, failure).await;
            return failureResponse(failure);
        }
    };
    if let Some(transaction) = capture.take() {
        capture = retainCapture(transaction, |transaction| async move {
            transaction
                .storeRequestBody(bodyWrite(requestBody, requestContentType, requestEncoding))
                .await
        })
        .await;
    }
    if let Err(error) = context.pipeline.runResponse(&mut pipeline).await {
        logPipelineError(&error);
        failCapture(capture, RequestFailure::UpstreamProtocol).await;
        return failureResponse(RequestFailure::UpstreamProtocol);
    }
    capture = retainPipelineState(capture, &pipeline).await;
    let responseThrottlePlan = pipeline.responseThrottlePlan.take();
    let Some(mut response) = pipeline.response.take() else {
        failCapture(capture, RequestFailure::UpstreamProtocol).await;
        return failureResponse(RequestFailure::UpstreamProtocol);
    };
    let responseBody = response.body.clone().unwrap_or_default();
    synchronizeBodyLength(&mut response.headers, responseBody.len());
    let responseContentType = contentType(&response.headers);
    let responseEncoding = contentEncoding(&response.headers);
    let responseHeaders = captureHeaders(&response.headers);
    let responseHeaderBytes =
        responseHeaderBytes(response.status, response.version, &response.headers);
    let statusCode = response.status;
    if let Some(transaction) = capture.take() {
        capture = retainCapture(transaction, |transaction| async move {
            transaction
                .storeResponseHeaders(responseHeaders, responseHeaderBytes, statusCode.as_u16())
                .await
        })
        .await;
    }
    let (outboundBody, throttleCompletion) = match createThrottledBody(
        bodyFromBytes(responseBody.clone()),
        responseThrottlePlan,
        ThrottleDirection::Download,
        &context,
    ) {
        Ok(body) => body,
        Err(failure) => {
            failCapture(capture, failure).await;
            return failureResponse(failure);
        }
    };
    let outbound = responseFromDraft(response, outboundBody);
    if let Some(throttleCompletion) = throttleCompletion {
        let Some(transaction) = capture else {
            return outbound;
        };
        let cancellation = context.cancellation.clone();
        context.tasks.spawn(async move {
            match waitForThrottleCompletion(throttleCompletion, &cancellation).await {
                Ok(()) => {
                    completeSyntheticCapture(
                        transaction,
                        outcome,
                        responseBody,
                        responseContentType,
                        responseEncoding,
                        statusCode,
                    )
                    .await;
                }
                Err(failure) => failCapture(Some(transaction), failure).await,
            }
        });
        return outbound;
    }
    if let Some(transaction) = capture {
        completeSyntheticCapture(
            transaction,
            outcome,
            responseBody,
            responseContentType,
            responseEncoding,
            statusCode,
        )
        .await;
    }
    outbound
}

/// 将已完成响应钩子的草稿转为客户端响应；常规上游响应不走此函数，以免破坏流式背压。
fn responseFromDraft(response: ResponseDraft, body: ProxyBody) -> Response<ProxyBody> {
    let mut outbound = Response::new(body);
    *outbound.status_mut() = response.status;
    *outbound.version_mut() = response.version;
    *outbound.headers_mut() = response.headers;
    outbound
}

/// 完成 Map Local 或阻断生成的终态录制；该函数在无节流时立即执行，有节流时只在下行正文完整交付后执行。
async fn completeSyntheticCapture(
    transaction: CaptureTransaction,
    outcome: PipelineRequestOutcome,
    responseBody: Bytes,
    responseContentType: String,
    responseEncoding: String,
    statusCode: StatusCode,
) {
    let body = bodyWrite(
        capturedBodyFromBytes(responseBody),
        responseContentType,
        responseEncoding,
    );
    let completion = match outcome {
        PipelineRequestOutcome::Blocked => {
            transaction.completeBlocked(body, statusCode.as_u16()).await
        }
        PipelineRequestOutcome::Synthetic => {
            transaction.completeHttp(body, statusCode.as_u16()).await
        }
        PipelineRequestOutcome::Forward => unreachable!("合成响应必须来自短路请求结果"),
    };
    if let Err(error) = completion {
        logCaptureError(&error);
        failCapture(Some(transaction), RequestFailure::CaptureFailed).await;
    }
}

/// 根据改写后的绝对 URI 选择上游连接池；未支持协议在出站前转换为稳定客户端错误。
fn upstreamTransportForTarget(target: &HttpTarget) -> Result<UpstreamTransport, RequestFailure> {
    match target.upstreamUri.scheme_str() {
        Some("http") => Ok(UpstreamTransport::Http),
        Some("https") => Ok(UpstreamTransport::Https),
        _ => Err(RequestFailure::UnsupportedScheme),
    }
}

/// 根据响应媒体类型选择正文路径；普通响应服从工具的完整正文需求，SSE 始终走背压流式转发。
async fn buildStreamingResponse(
    upstreamResponse: Response<Incoming>,
    mut captureContext: ResponseCaptureContext,
    context: ProxyContext,
) -> Response<ProxyBody> {
    let (mut responseParts, incomingBody) = upstreamResponse.into_parts();
    let streamsEvents = isEventStream(&responseParts.headers);
    if !streamsEvents && context.pipeline.requiresResponseBody() {
        return buildBufferedResponse(responseParts, incomingBody, captureContext, context).await;
    }
    captureContext.pipeline.response = Some(ResponseDraft {
        status: responseParts.status,
        version: responseParts.version,
        headers: responseParts.headers.clone(),
        body: None,
    });
    // SSE 没有可等待的正文终点；只运行响应头工具，正文改写与响应断点明确跳过且不记为已应用。
    let pipelineResult = if streamsEvents {
        context
            .pipeline
            .runStreamingResponse(&mut captureContext.pipeline)
            .await
    } else {
        context
            .pipeline
            .runResponse(&mut captureContext.pipeline)
            .await
    };
    if let Err(error) = pipelineResult {
        logPipelineError(&error);
        failCapture(captureContext.transaction, RequestFailure::UpstreamProtocol).await;
        return failureResponse(RequestFailure::UpstreamProtocol);
    }
    captureContext.transaction =
        retainPipelineState(captureContext.transaction, &captureContext.pipeline).await;
    let Some(response) = captureContext.pipeline.response.take() else {
        failCapture(captureContext.transaction, RequestFailure::UpstreamProtocol).await;
        return failureResponse(RequestFailure::UpstreamProtocol);
    };
    // 当前流式响应路径不允许工具在未声明正文物化时替换 body；正文改写会走后续的缓冲路径。
    if response.body.is_some() {
        failCapture(captureContext.transaction, RequestFailure::UpstreamProtocol).await;
        return failureResponse(RequestFailure::UpstreamProtocol);
    }
    let responsePacer = match createThrottlePacer(
        captureContext.pipeline.responseThrottlePlan.take(),
        ThrottleDirection::Download,
    ) {
        Ok(pacer) => pacer,
        Err(failure) => {
            failCapture(captureContext.transaction, failure).await;
            return failureResponse(failure);
        }
    };
    let statusCode = response.status;
    let responseContentType = contentType(&response.headers);
    let responseEncoding = contentEncoding(&response.headers);
    let declaredResponseBodyBytes = declaredResponseBodyLength(&response.headers);
    let responseHeaders = captureHeaders(&response.headers);
    let responseHeaderBytes =
        responseHeaderBytes(response.status, response.version, &response.headers);
    if let Some(transaction) = captureContext.transaction.take() {
        captureContext.transaction = retainCapture(transaction, |transaction| async move {
            transaction
                .storeResponseHeaders(responseHeaders, responseHeaderBytes, statusCode.as_u16())
                .await
        })
        .await;
    }
    responseParts.status = response.status;
    responseParts.version = response.version;
    responseParts.headers = response.headers;
    removeHopByHopHeaders(&mut responseParts.headers);
    let (frameSender, responseBody) = bodyFrameChannel(responseFrameCapacity);
    let responseCapture = SharedBodyCapture::new();
    let cancellation = context.cancellation.clone();
    context.tasks.spawn(async move {
        let mut incomingBody = incomingBody;
        let mut responsePacer = responsePacer;
        loop {
            let nextFrame = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    failCapture(captureContext.transaction, RequestFailure::Cancelled).await;
                    return;
                }
                frame = incomingBody.frame() => frame,
            };
            match nextFrame {
                Some(Ok(frame)) => {
                    let frameBytes = frame.data_ref().cloned();
                    if let Some(bytes) = frameBytes.as_ref()
                        && let Some(pacer) = responsePacer.as_mut()
                    {
                        if let Err(failure) =
                            forwardThrottledBytes(&frameSender, bytes.clone(), pacer, &cancellation)
                                .await
                        {
                            failCapture(captureContext.transaction, failure).await;
                            return;
                        }
                        // 只记录已经完整进入客户端响应通道的数据；发送失败的当前帧不能写入录制正文。
                        responseCapture.append(bytes);
                        continue;
                    }
                    if let Err(failure) =
                        sendBodyFrame(&frameSender, Ok(frame), &cancellation).await
                    {
                        // 客户端收到声明长度的全部数据后常会立即释放 body，随后到达的空帧或 trailer
                        // 会看到通道关闭。此时正文交付已完成，不应把成功响应误标为客户端提前断开。
                        if failure == RequestFailure::ClientDisconnected
                            && responseDeliveryComplete(declaredResponseBodyBytes, &responseCapture)
                        {
                            break;
                        }
                        failCapture(captureContext.transaction, failure).await;
                        return;
                    }
                    if let Some(bytes) = frameBytes {
                        responseCapture.append(&bytes);
                    }
                }
                Some(Err(error)) => {
                    let bodyError = Box::new(error);
                    let failure =
                        match sendBodyFrame(&frameSender, Err(bodyError), &cancellation).await {
                            Ok(()) => RequestFailure::UpstreamProtocol,
                            Err(failure) => failure,
                        };
                    if failure == RequestFailure::ClientDisconnected {
                        tracing::debug!(
                            errorCode = "httpProxyResponseBodyReceiverClosed",
                            messageKey = "error.httpProxy.clientDisconnected"
                        );
                    }
                    failCapture(captureContext.transaction, failure).await;
                    return;
                }
                None => break,
            }
        }
        // 先关闭响应通道让客户端观察到 EOF，再等待请求侧终结，避免 early response 全双工场景互等。
        drop(frameSender);
        let requestBody = match captureContext.request.complete(&cancellation).await {
            Ok(body) => body,
            Err(failure) => {
                failCapture(captureContext.transaction, failure).await;
                return;
            }
        };
        let Some(transaction) = captureContext.transaction else {
            return;
        };
        if let Err(error) = transaction.storeRequestBody(requestBody).await {
            logCaptureError(&error);
            failCapture(Some(transaction), RequestFailure::CaptureFailed).await;
            return;
        }
        let responseBody = bodyWrite(
            responseCapture.snapshot(),
            responseContentType,
            responseEncoding,
        );
        if let Err(error) = transaction
            .completeHttp(responseBody, statusCode.as_u16())
            .await
        {
            logCaptureError(&error);
            failCapture(Some(transaction), RequestFailure::CaptureFailed).await;
        }
    });
    Response::from_parts(responseParts, responseBody)
}

/// 识别允许参数和大小写差异的 SSE 媒体类型；无效或重复头字段按非 SSE 处理，
/// 避免把普通响应错误放入不可回退的流式工具路径。
fn isEventStream(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(http::header::CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    let Ok(value) = value.to_str() else {
        return false;
    };
    value
        .split(';')
        .next()
        .is_some_and(|mediaType| mediaType.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// 读取唯一且合法的 Content-Length；重复或畸形字段不能用于推断客户端已完整收到正文。
fn declaredResponseBodyLength(headers: &HeaderMap) -> Option<u64> {
    let mut values = headers.get_all(http::header::CONTENT_LENGTH).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()?.trim().parse::<u64>().ok()
}

/// 判断客户端响应通道关闭前是否已经成功接收声明长度的全部数据帧；未知长度始终保持断连失败语义。
fn responseDeliveryComplete(
    declaredBodyBytes: Option<u64>,
    responseCapture: &SharedBodyCapture,
) -> bool {
    declaredBodyBytes
        .is_some_and(|expectedBytes| responseCapture.snapshot().originalBytes == expectedBytes)
}

/// 在响应正文工具启用时以配置上限物化上游正文，完成改写后再提交给客户端和录制层；未启用时始终保留流式路径。
async fn buildBufferedResponse(
    mut responseParts: http::response::Parts,
    incomingBody: Incoming,
    mut captureContext: ResponseCaptureContext,
    context: ProxyContext,
) -> Response<ProxyBody> {
    let responseBody = match materializeIncomingBody(
        incomingBody,
        context.config.maxCaptureBodyBytes,
        &context.cancellation,
    )
    .await
    {
        Ok(body) => body,
        Err(failure) => {
            let failure = responseMaterializationFailure(failure);
            failCapture(captureContext.transaction, failure).await;
            return failureResponse(failure);
        }
    };
    captureContext.pipeline.response = Some(ResponseDraft {
        status: responseParts.status,
        version: responseParts.version,
        headers: responseParts.headers.clone(),
        body: Some(responseBody),
    });
    if let Err(error) = context
        .pipeline
        .runResponse(&mut captureContext.pipeline)
        .await
    {
        logPipelineError(&error);
        failCapture(captureContext.transaction, RequestFailure::UpstreamProtocol).await;
        return failureResponse(RequestFailure::UpstreamProtocol);
    }
    captureContext.transaction =
        retainPipelineState(captureContext.transaction, &captureContext.pipeline).await;
    let responseThrottlePlan = captureContext.pipeline.responseThrottlePlan.take();
    let Some(mut response) = captureContext.pipeline.response.take() else {
        failCapture(captureContext.transaction, RequestFailure::UpstreamProtocol).await;
        return failureResponse(RequestFailure::UpstreamProtocol);
    };
    let responseBody = response.body.take().unwrap_or_default();
    synchronizeBodyLength(&mut response.headers, responseBody.len());
    let statusCode = response.status;
    let responseContentType = contentType(&response.headers);
    let responseEncoding = contentEncoding(&response.headers);
    let responseHeaders = captureHeaders(&response.headers);
    let responseHeaderBytes =
        responseHeaderBytes(response.status, response.version, &response.headers);
    if let Some(transaction) = captureContext.transaction.take() {
        captureContext.transaction = retainCapture(transaction, |transaction| async move {
            transaction
                .storeResponseHeaders(responseHeaders, responseHeaderBytes, statusCode.as_u16())
                .await
        })
        .await;
    }
    responseParts.status = response.status;
    responseParts.version = response.version;
    responseParts.headers = response.headers;
    removeHopByHopHeaders(&mut responseParts.headers);
    let (outboundBody, throttleCompletion) = match createThrottledBody(
        bodyFromBytes(responseBody.clone()),
        responseThrottlePlan,
        ThrottleDirection::Download,
        &context,
    ) {
        Ok(body) => body,
        Err(failure) => {
            failCapture(captureContext.transaction, failure).await;
            return failureResponse(failure);
        }
    };
    let outbound = Response::from_parts(responseParts, outboundBody);
    let Some(transaction) = captureContext.transaction else {
        return outbound;
    };
    let cancellation = context.cancellation.clone();
    context.tasks.spawn(async move {
        if let Some(throttleCompletion) = throttleCompletion
            && let Err(failure) = waitForThrottleCompletion(throttleCompletion, &cancellation).await
        {
            failCapture(Some(transaction), failure).await;
            return;
        }
        let requestBody = match captureContext.request.complete(&cancellation).await {
            Ok(body) => body,
            Err(failure) => {
                failCapture(Some(transaction), failure).await;
                return;
            }
        };
        if let Err(error) = transaction.storeRequestBody(requestBody).await {
            logCaptureError(&error);
            failCapture(Some(transaction), RequestFailure::CaptureFailed).await;
            return;
        }
        let responseBody = bodyWrite(
            capturedBodyFromBytes(responseBody),
            responseContentType,
            responseEncoding,
        );
        if let Err(error) = transaction
            .completeHttp(responseBody, statusCode.as_u16())
            .await
        {
            logCaptureError(&error);
            failCapture(Some(transaction), RequestFailure::CaptureFailed).await;
        }
    });
    outbound
}

/// 将上游响应物化阶段的连接错误归类为上游协议失败；请求方向才使用客户端断连语义。
fn responseMaterializationFailure(failure: RequestFailure) -> RequestFailure {
    match failure {
        RequestFailure::ClientDisconnected | RequestFailure::PipelineBodyLimitExceeded => {
            RequestFailure::UpstreamProtocol
        }
        failure => failure,
    }
}

/// 向有界正文通道发送 frame；请求与响应方向共用该背压原语，取消信号始终优先于通道等待。
async fn sendBodyFrame(
    sender: &BodyFrameSender,
    frame: Result<Frame<Bytes>, BoxBodyError>,
    cancellation: &CancellationToken,
) -> Result<(), RequestFailure> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(RequestFailure::Cancelled),
        result = sender.send(frame) => {
            result.map_err(|_| RequestFailure::ClientDisconnected)
        }
    }
}

/// 创建录制事务；录制层故障只停用当前事务，不中断真实代理数据面。
async fn beginCapture(
    session: &RecordingSession,
    input: BeginTransaction,
) -> Option<CaptureTransaction> {
    match CaptureTransaction::begin(session, input).await {
        Ok(transaction) => transaction,
        Err(error) => {
            logCaptureError(&error);
            None
        }
    }
}

/// 执行一项录制步骤；失败时显式终结事务并返回 None，防止部分事务继续写入。
async fn retainCapture<Operation, OperationFuture>(
    transaction: CaptureTransaction,
    operation: Operation,
) -> Option<CaptureTransaction>
where
    Operation: FnOnce(CaptureTransaction) -> OperationFuture,
    OperationFuture: Future<Output = Result<(), CaptureError>>,
{
    let retained = transaction.clone();
    match operation(transaction).await {
        Ok(()) => Some(retained),
        Err(error) => {
            logCaptureError(&error);
            failCapture(Some(retained), RequestFailure::CaptureFailed).await;
            None
        }
    }
}

/// 将工具阶段累积的标志和痕迹同步到 pending 事务；录制关闭或淘汰时 None 保持数据面继续转发。
async fn retainPipelineState(
    capture: Option<CaptureTransaction>,
    pipeline: &PipelineContext,
) -> Option<CaptureTransaction> {
    let transaction = capture?;
    let flags = pipeline.flags.clone();
    let appliedTools = pipeline.appliedTools.clone();
    retainCapture(transaction, |transaction| async move {
        transaction.storePipelineState(flags, appliedTools).await
    })
    .await
}

/// 在上游建连失败时先保存已被 Hyper 消费的请求镜像，再进入失败终态。
async fn storeRequestBeforeFailure(
    capture: Option<&CaptureTransaction>,
    request: RequestCaptureContext,
    cancellation: &CancellationToken,
) {
    let Some(transaction) = capture else {
        return;
    };
    let Ok(body) = request.complete(cancellation).await else {
        return;
    };
    if let Err(error) = transaction.storeRequestBody(body).await {
        logCaptureError(&error);
    }
}

/// 将流式镜像转换为 capture-core 的完整正文写入描述。
fn bodyWrite(captured: CapturedBody, contentType: String, encoding: String) -> BodyWrite {
    BodyWrite {
        bytes: captured.bytes,
        originalBytes: captured.originalBytes,
        contentType,
        encoding,
    }
}

/// 显式写入失败终态；重复终态或 FIFO 淘汰由 CaptureTransaction 统一解释。
async fn failCapture(capture: Option<CaptureTransaction>, failure: RequestFailure) {
    let Some(transaction) = capture else {
        return;
    };
    if let Err(error) = transaction.fail(failure).await {
        logCaptureError(&error);
    }
}

/// 记录不含正文、路径或上游诊断文本的结构化录制故障。
fn logCaptureError(error: &CaptureError) {
    tracing::error!(
        errorCode = error.code(),
        messageKey = error.messageKey(),
        "captureOperationFailed"
    );
}

/// 记录工具执行失败的稳定标识；错误文本不包含规则、路径、正文或本机文件系统信息。
fn logPipelineError(error: &crate::pipeline::PipelineError) {
    tracing::error!(
        errorCode = "toolPipelineFailed",
        pipelineError = %error,
        "toolPipelineOperationFailed"
    );
}

/// 将连接超时、建连失败与响应协议错误区分为稳定网关错误，不暴露底层诊断文本。
fn classifyUpstreamError(
    error: &hyper_util::client::legacy::Error,
    transport: UpstreamTransport,
) -> RequestFailure {
    let mut currentSource = error.source();
    let mut timedOut = false;
    while let Some(source) = currentSource {
        // 上游在发送任何 HTTP 响应前关闭连接属于可达性失败，而不是收到了一份格式错误的响应。
        // Hyper 将该边界包装为 incomplete message；单独识别可保持 502 诊断与真实故障阶段一致。
        if source
            .downcast_ref::<hyper::Error>()
            .is_some_and(hyper::Error::is_incomplete_message)
        {
            return RequestFailure::UpstreamUnavailable;
        }
        if let Some(ioError) = source.downcast_ref::<io::Error>() {
            match ioError.kind() {
                io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::AddrNotAvailable
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::NetworkUnreachable => {
                    return RequestFailure::UpstreamUnavailable;
                }
                io::ErrorKind::TimedOut => timedOut = true,
                _ => {}
            }
        }
        currentSource = source.source();
    }
    if timedOut {
        return RequestFailure::UpstreamTimeout;
    }
    if error.is_connect() {
        match transport {
            UpstreamTransport::Http => RequestFailure::UpstreamUnavailable,
            UpstreamTransport::Https => RequestFailure::UpstreamTlsHandshake,
        }
    } else {
        RequestFailure::UpstreamProtocol
    }
}

/// 构建无正文错误响应；稳定错误码通过头字段交给客户端或控制面本地化。
pub(crate) fn failureResponse(failure: RequestFailure) -> Response<ProxyBody> {
    let mut response = Response::new(emptyBody());
    *response.status_mut() = failure.statusCode();
    response.headers_mut().insert(
        "x-proxy-error-code",
        HeaderValue::from_static(failure.code()),
    );
    response
}
