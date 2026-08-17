//! 将 WinDivert 透明接管后的原始 TCP/TLS 字节流完整投影到录制事务。
//!
//! HTTP/HTTPS 已由协议处理器按消息录制；本模块只承接无法进行应用层解码的 Raw/RawTls。
//! 双向正文通过有界通道写入 Capture spool，网络侧在队列满时施加背压，既不无限占用内存也不丢字节。

use std::{collections::BTreeMap, io};

use bytes::Bytes;
use capture_core::{
    BeginTransaction, BodySpool, CaptureError, MessageSide, RecordingSession, StreamPacket,
    StreamPacketAction, StreamPacketModification, TransactionCompletion, TransactionError,
    TransactionProtocol, currentTimeMilliseconds,
};
use location_core::ResolvedLocation;
use plugin_host::{
    ConnectionMetadata, DataPlaneActionResult, PluginConnection, PluginHost, StreamDirection,
    TransportKind, deriveWireByteModifications,
};
use socks5_core::{SessionApplicationProtocol, interception::TcpTunnel};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const relayBufferBytes: usize = 64 * 1024;
const captureQueueChunks: usize = 16;
const binaryContentType: &str = "application/octet-stream";
const binaryEncoding: &str = "binary";

/// 共享透明原始流录制器；后台 spool 任务由同一 tracker 管理，服务停止可等待终态完成。
#[derive(Clone)]
pub struct TransparentRecording {
    recording: RecordingSession,
    tasks: TaskTracker,
}

impl TransparentRecording {
    /// 从 `recording` 创建与当前服务周期绑定的录制器；实例本身不启动任务、没有失败分支。
    pub fn new(recording: RecordingSession) -> Self {
        Self {
            recording,
            tasks: TaskTracker::new(),
        }
    }

    /// 中继一条 Raw/RawTls 透明连接并完整录制双向正文。
    ///
    /// 运行上下文：原目标连接已经建立，两个套接字仍包含全部原始字节；协议必须是 Tcp 或 Tls。
    /// 失败语义：网络或 spool 写入失败会关闭本连接并形成 failed 终态；服务取消形成 cancelled 终态。
    pub async fn relay(
        &self,
        tunnel: TcpTunnel,
        applicationProtocol: SessionApplicationProtocol,
    ) -> io::Result<()> {
        self.relayWithDataPlane(tunnel, applicationProtocol, PluginHost::disabled())
            .await
    }

    /// 通过宿主唯一最终写线入口中继透明 Raw/RawTls；WinDivert TCP 与显式 SOCKS5 因而共享插件和 WPE 规则。
    ///
    /// 运行上下文：生产透明监听必须调用本函数并传入服务代际的 `PluginHost`；测试或禁用宿主可传 `PluginHost::disabled()`。
    /// 失败语义：数据面返回 Close、网络错误或 spool 错误会结束当前连接；Drop/Hold 只跳过当前读取块，连接继续处理后续字节。
    pub async fn relayWithDataPlane(
        &self,
        tunnel: TcpTunnel,
        applicationProtocol: SessionApplicationProtocol,
        pluginHost: PluginHost,
    ) -> io::Result<()> {
        // 录制是透明转发的旁路观察者，初始化失败只能丢失本条事务的录制能力，不能截断真实连接。
        // 该边界保证磁盘、会话状态或索引故障不会被客户端误判为 TLS 坏包或服务端主动断开。
        let capture = match self.beginCapture(&tunnel, applicationProtocol).await {
            Ok(capture) => capture,
            Err(error) => {
                eprintln!(
                    "透明流录制初始化失败，连接继续转发：code={}, operation=transparentRecordingBegin",
                    error.code()
                );
                None
            }
        };
        let pluginConnection = pluginHost.openConnection(ConnectionMetadata {
            transport: TransportKind::Tcp,
            clientAddress: tunnel.clientAddress.to_string(),
            targetHost: tunnel.targetHost.clone(),
            targetPort: tunnel.targetPort,
        });
        relayTunnel(tunnel, capture, pluginHost, pluginConnection).await
    }

    /// 关闭任务登记并等待所有已接收字节完成落盘；调用前外层必须已停止接收新连接。
    /// 本方法不返回失败：单事务落盘错误由对应 `relay` 返回或记录稳定诊断，等待只负责回收任务所有权。
    pub async fn shutdown(&self) {
        self.tasks.close();
        self.tasks.wait().await;
    }

    /// 强制断开连接后等待 channel 关闭触发取消终态；不再中止 spool，避免停止时丢失已观察正文。
    /// 本方法不返回失败：调用方负责先取消并丢弃套接字任务，后台写入失败通过稳定诊断保留根因。
    pub async fn abortAndWait(&self) {
        self.tasks.close();
        self.tasks.wait().await;
    }

    /// 为 `tunnel` 和已判定的 `applicationProtocol` 创建事务、双向 spool 与有界事件通道。
    /// 录制暂停时返回禁用句柄；事务或文件初始化失败时返回 `CaptureError`，数据面据此关闭该连接。
    async fn beginCapture(
        &self,
        tunnel: &TcpTunnel,
        applicationProtocol: SessionApplicationProtocol,
    ) -> Result<Option<CaptureSender>, CaptureError> {
        let location = transparentLocation(tunnel, applicationProtocol);
        let transactionId = self
            .recording
            .beginTransaction(BeginTransaction {
                protocol: TransactionProtocol::Tunnel,
                method: "CONNECT".to_owned(),
                location,
                clientAddress: tunnel.clientAddress.to_string(),
                clientProcessName: tunnel.clientProcessName.clone(),
                clientProcessId: tunnel.clientProcessId,
                contentType: binaryContentType.to_owned(),
                startAtMilliseconds: currentTimeMilliseconds(),
            })
            .await?;
        let Some(transactionId) = transactionId else {
            return Ok(None);
        };
        let requestSpool = match self
            .recording
            .createBodySpool(&transactionId, MessageSide::Request)
            .await
        {
            Ok(spool) => spool,
            Err(error) => {
                failTransaction(&self.recording, &transactionId, &error.to_string()).await;
                return Err(error);
            }
        };
        let responseSpool = match self
            .recording
            .createBodySpool(&transactionId, MessageSide::Response)
            .await
        {
            Ok(spool) => spool,
            Err(error) => {
                failTransaction(&self.recording, &transactionId, &error.to_string()).await;
                return Err(error);
            }
        };
        let (eventSender, eventReceiver) = mpsc::channel(captureQueueChunks);
        let (completionSender, completionReceiver) = oneshot::channel();
        let recording = self.recording.clone();
        self.tasks.spawn(async move {
            let result = runCaptureWriter(
                recording,
                transactionId,
                requestSpool,
                responseSpool,
                eventReceiver,
            )
            .await;
            if let Err(Err(error)) = completionSender.send(result) {
                // 强制停止会先丢弃套接字任务；成功终态已经落盘时接收端消失属于预期竞态，
                // 只有失败结果无人接收时才记录稳定错误码，避免真正的落盘根因被静默吞掉。
                eprintln!(
                    "透明流录制失败结果未被接收：code={}, operation=transparentRecordingCompletion",
                    error.code()
                );
            }
        });
        Ok(Some(CaptureSender {
            events: eventSender,
            completion: completionReceiver,
        }))
    }
}

/// 保存网络任务到后台 spool 的有界通道；completion 保证正常返回前事务已经形成终态。
struct CaptureSender {
    events: mpsc::Sender<CaptureEvent>,
    completion: oneshot::Receiver<Result<(), CaptureError>>,
}

/// 描述双向正文块和连接终态；正文块按方向分别追加，终态事件始终位于全部已发送块之后。
enum CaptureEvent {
    Request(CapturedChunk),
    Response(CapturedChunk),
    Terminal(RelayTerminal),
}

/// 保存一次成功读取的流片段及其观测时间；正文和片段索引共用同一字节对象，避免额外复制。
struct CapturedChunk {
    bytes: Bytes,
    capturedAtMilliseconds: u64,
    action: StreamPacketAction,
    modifications: Vec<StreamPacketModification>,
}

/// 区分自然双向结束、服务取消与网络失败，驱动统一事务状态机。
enum RelayTerminal {
    Complete,
    Cancelled,
    Failed(String),
}

/// 根据 `tunnel` 原目标与 `applicationProtocol` 返回稳定显示位置；纯转换不执行 I/O、没有失败分支。
///
/// 运行上下文：已确认 ClientHello 的 TCP 流在产品协议模型中归为 HTTPS，即使未启用本地解密也不显示底层 TLS 容器名。
/// 参数：`tunnel` 提供权威目标，`applicationProtocol` 是首包分类结果；未知二进制流保持 TCP。
/// 失败语义：输入均已由连接分类器校验，本函数只构造 Location，不产生解析失败。
fn transparentLocation(
    tunnel: &TcpTunnel,
    applicationProtocol: SessionApplicationProtocol,
) -> ResolvedLocation {
    let protocol = match applicationProtocol {
        SessionApplicationProtocol::Tls | SessionApplicationProtocol::Https => "https",
        _ => "tcp",
    };
    let displayHost = if tunnel.targetHost.contains(':') {
        format!("[{}]", tunnel.targetHost)
    } else {
        tunnel.targetHost.clone()
    };
    ResolvedLocation {
        protocol: protocol.to_owned(),
        host: tunnel.targetHost.clone(),
        port: tunnel.targetPort,
        path: String::new(),
        query: String::new(),
        display: format!("{protocol}://{displayHost}:{}", tunnel.targetPort),
    }
}

/// 同时驱动 `tunnel` 上下行并把已成功转发的字节交给可选 `capture`；任一网络错误结束另一方向。
/// 网络或取消失败返回连接级 I/O 错误；录制故障只形成诊断，不能改变真实 TCP 字节流和连接寿命。
async fn relayTunnel(
    tunnel: TcpTunnel,
    capture: Option<CaptureSender>,
    pluginHost: PluginHost,
    pluginConnection: PluginConnection,
) -> io::Result<()> {
    let TcpTunnel {
        clientStream,
        remoteStream,
        cancellation,
        ..
    } = tunnel;
    let eventSender = capture.as_ref().map(|capture| capture.events.clone());
    let (clientRead, clientWrite) = clientStream.into_split();
    let (remoteRead, remoteWrite) = remoteStream.into_split();
    let upload = relayDirection(
        clientRead,
        remoteWrite,
        RelayDirectionContext {
            capture: eventSender.clone(),
            side: MessageSide::Request,
            cancellation: cancellation.clone(),
            pluginHost: pluginHost.clone(),
            pluginConnection: pluginConnection.clone(),
            direction: StreamDirection::ClientToServer,
        },
    );
    let download = relayDirection(
        remoteRead,
        clientWrite,
        RelayDirectionContext {
            capture: eventSender,
            side: MessageSide::Response,
            cancellation: cancellation.clone(),
            pluginHost: pluginHost.clone(),
            pluginConnection: pluginConnection.clone(),
            direction: StreamDirection::ServerToClient,
        },
    );
    let relayResult = tokio::try_join!(upload, download);
    let terminal = match &relayResult {
        Ok(_) => RelayTerminal::Complete,
        Err(_) if cancellation.is_cancelled() => RelayTerminal::Cancelled,
        Err(error) => RelayTerminal::Failed(error.to_string()),
    };

    if let Some(capture) = capture {
        let terminalAccepted = capture
            .events
            .send(CaptureEvent::Terminal(terminal))
            .await
            .is_ok();
        drop(capture.events);
        match capture.completion.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!(
                "透明流录制终态失败，连接结果保持网络真实状态：code={}, operation=transparentRecordingFinalize",
                error.code()
            ),
            Err(_) if terminalAccepted => eprintln!(
                "透明流录制终态任务异常结束，连接结果保持网络真实状态：operation=transparentRecordingFinalize"
            ),
            Err(_) => {}
        }
    }
    pluginHost.closeDataPlaneConnection(pluginConnection).await;
    relayResult.map(|_| ())
}

/// 汇聚透明流单方向中继所需的录制、取消和统一封包数据面上下文，避免参数在双向调用处发生错位。
struct RelayDirectionContext {
    capture: Option<mpsc::Sender<CaptureEvent>>,
    side: MessageSide,
    cancellation: CancellationToken,
    pluginHost: PluginHost,
    pluginConnection: PluginConnection,
    direction: StreamDirection,
}

/// 从 `reader` 向 `writer` 单向复制，并按上下文记录每次已经完整写入对端的最终字节块。
/// `context` 同时固定业务方向与 WPE 连接身份；取消或网络读写失败返回连接级 I/O 错误，录制通道失败只停用本方向录制。
async fn relayDirection<R, W>(
    mut reader: R,
    mut writer: W,
    mut context: RelayDirectionContext,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0_u8; relayBufferBytes];
    let mut transferredBytes = 0_u64;
    loop {
        let byteCount = tokio::select! {
            _ = context.cancellation.cancelled() => {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "代理服务已停止"));
            }
            result = reader.read(&mut buffer) => result?,
        };
        if byteCount == 0 {
            writer.shutdown().await?;
            return Ok(transferredBytes);
        }
        let originalBytes = &buffer[..byteCount];
        let actionResult = context
            .pluginHost
            .processDataPlaneBytes(
                &context.pluginConnection,
                context.direction,
                buffer[..byteCount].to_vec(),
            )
            .await;
        let (bytes, action) = match actionResult {
            DataPlaneActionResult::Forward { bytes } => {
                let action = if bytes == originalBytes {
                    StreamPacketAction::Forward
                } else {
                    StreamPacketAction::Replace
                };
                (bytes, action)
            }
            DataPlaneActionResult::Hold => continue,
            DataPlaneActionResult::Drop => {
                recordInterceptedChunk(&mut context, originalBytes, StreamPacketAction::Drop).await;
                continue;
            }
            DataPlaneActionResult::Close => {
                recordInterceptedChunk(&mut context, originalBytes, StreamPacketAction::Close)
                    .await;
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "封包数据面关闭了透明连接",
                ));
            }
        };
        // 必须先完成最终字节写入再建立片段索引；否则插件或 WPE 修改后的线上正文会与录制内容分叉。
        writer.write_all(&bytes).await?;
        transferredBytes = transferredBytes.saturating_add(bytes.len() as u64);
        if let Some(captureSender) = context.capture.as_ref() {
            let modifications = deriveWireByteModifications(originalBytes, &bytes)
                .into_iter()
                .map(|change| StreamPacketModification {
                    offsetBytes: change.offsetBytes,
                    originalBytes: change.originalBytes,
                    modifiedBytes: change.modifiedBytes,
                })
                .collect();
            let chunk = CapturedChunk {
                bytes: Bytes::from(bytes),
                capturedAtMilliseconds: currentTimeMilliseconds(),
                action,
                modifications,
            };
            let event = match context.side {
                MessageSide::Request => CaptureEvent::Request(chunk),
                MessageSide::Response => CaptureEvent::Response(chunk),
            };
            if captureSender.send(event).await.is_err() {
                // 后台 writer 已经记录明确失败终态；释放 sender 后本方向继续承担纯字节中继。
                context.capture = None;
            }
        }
    }
}

/// 记录未写向对端但已被规则消费的原始块；失败只关闭本方向录制，不改变丢弃或断连动作。
async fn recordInterceptedChunk(
    context: &mut RelayDirectionContext,
    originalBytes: &[u8],
    action: StreamPacketAction,
) {
    let Some(captureSender) = context.capture.as_ref() else {
        return;
    };
    let chunk = CapturedChunk {
        bytes: Bytes::copy_from_slice(originalBytes),
        capturedAtMilliseconds: currentTimeMilliseconds(),
        action,
        modifications: Vec::new(),
    };
    let event = match context.side {
        MessageSide::Request => CaptureEvent::Request(chunk),
        MessageSide::Response => CaptureEvent::Response(chunk),
    };
    if captureSender.send(event).await.is_err() {
        context.capture = None;
    }
}

/// 顺序消费 `events`，同时写入双向 spool 和连续片段索引；channel 提前关闭时把事务标记为 cancelled。
/// spool、索引或终态提交失败返回 `CaptureError`，同时尝试把 pending 事务转为显式失败状态。
async fn runCaptureWriter(
    recording: RecordingSession,
    transactionId: String,
    mut requestSpool: BodySpool,
    mut responseSpool: BodySpool,
    mut events: mpsc::Receiver<CaptureEvent>,
) -> Result<(), CaptureError> {
    let mut terminal = None;
    let mut requestPackets = Vec::new();
    let mut responsePackets = Vec::new();
    while let Some(event) = events.recv().await {
        let result = match event {
            CaptureEvent::Request(chunk) => {
                appendCapturedChunk(&mut requestSpool, &mut requestPackets, chunk).await
            }
            CaptureEvent::Response(chunk) => {
                appendCapturedChunk(&mut responseSpool, &mut responsePackets, chunk).await
            }
            CaptureEvent::Terminal(connectionTerminal) => {
                terminal = Some(connectionTerminal);
                break;
            }
        };
        if let Err(error) = result {
            failTransaction(&recording, &transactionId, &error.to_string()).await;
            return Err(error);
        }
    }
    let terminal = terminal.unwrap_or(RelayTerminal::Cancelled);
    if let Err(error) = recording
        .storeBodySpools(
            &transactionId,
            requestSpool,
            responseSpool,
            binaryContentType,
            binaryEncoding,
        )
        .await
    {
        failTransaction(&recording, &transactionId, &error.to_string()).await;
        return Err(error);
    }
    if let Err(error) = recording
        .storeStreamPackets(&transactionId, MessageSide::Request, requestPackets)
        .await
    {
        failTransaction(&recording, &transactionId, &error.to_string()).await;
        return Err(error);
    }
    if let Err(error) = recording
        .storeStreamPackets(&transactionId, MessageSide::Response, responsePackets)
        .await
    {
        failTransaction(&recording, &transactionId, &error.to_string()).await;
        return Err(error);
    }
    let endAtMilliseconds = currentTimeMilliseconds();
    match terminal {
        RelayTerminal::Complete => {
            recording
                .commit(
                    &transactionId,
                    TransactionCompletion {
                        statusCode: 0,
                        endAtMilliseconds,
                        contentType: binaryContentType.to_owned(),
                    },
                )
                .await
        }
        RelayTerminal::Cancelled => {
            recording
                .cancel(
                    &transactionId,
                    TransactionError {
                        code: "transparentTunnelCancelled".to_owned(),
                        messageKey: "error.httpProxy.clientDisconnected".to_owned(),
                        params: BTreeMap::new(),
                    },
                    endAtMilliseconds,
                )
                .await
        }
        RelayTerminal::Failed(detail) => {
            recording
                .fail(
                    &transactionId,
                    TransactionError {
                        code: "transparentTunnelFailed".to_owned(),
                        messageKey: "error.httpProxy.clientDisconnected".to_owned(),
                        params: BTreeMap::from([("detail".to_owned(), detail)]),
                    },
                    endAtMilliseconds,
                )
                .await
        }
    }
}

/// 将 `chunk` 完整追加到同侧 spool，并生成引用该正文范围的连续片段索引。
/// 写入失败时不增加序号和偏移；成功索引的 `storedBytes` 与 `originalBytes` 始终一致，不产生截断标记。
async fn appendCapturedChunk(
    spool: &mut BodySpool,
    packets: &mut Vec<StreamPacket>,
    chunk: CapturedChunk,
) -> Result<(), CaptureError> {
    let storedOffsetBytes = spool.writtenBytes();
    spool.append(&chunk.bytes).await?;
    let storedBytes = chunk.bytes.len();
    packets.push(StreamPacket {
        sequence: packets.len() as u64 + 1,
        capturedAtMilliseconds: chunk.capturedAtMilliseconds,
        storedOffsetBytes,
        storedBytes,
        originalBytes: storedBytes as u64,
        truncated: false,
        action: chunk.action,
        modifications: chunk.modifications,
    });
    Ok(())
}

/// 把 `detail` 对应的 spool 或初始化故障写入 `transactionId`；预期终态竞态直接结束。
/// 该辅助函数没有返回值；非预期录制错误使用稳定错误码输出诊断，禁止静默吞掉根因。
async fn failTransaction(recording: &RecordingSession, transactionId: &str, detail: &str) {
    let result = recording
        .fail(
            transactionId,
            TransactionError {
                code: "transparentRecordingFailed".to_owned(),
                messageKey: "error.httpProxy.clientDisconnected".to_owned(),
                params: BTreeMap::from([("detail".to_owned(), detail.to_owned())]),
            },
            currentTimeMilliseconds(),
        )
        .await;
    if let Err(error) = result
        && !matches!(
            error,
            CaptureError::TransactionNotFound
                | CaptureError::TransactionFinished
                | CaptureError::SessionClosed
        )
    {
        eprintln!(
            "透明流事务失败终态写入异常：code={}, transactionId={transactionId}",
            error.code()
        );
    }
}
