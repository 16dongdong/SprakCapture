use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    net::SocketAddr,
};

use capture_core::{
    BeginTransaction, BodyWrite, CaptureError, MessageSide, RecordingSession,
    TransactionCompletion, TransactionError, TransactionProgressUpdate, TransactionProtocol,
};
use location_core::ResolvedLocation;
use socks5_core::{SessionApplicationProtocol, SessionSnapshot, SessionState, TrafficDirection};

/// 保存单个 SOCKS5 会话对应的录制事务状态；None 表示录制暂停或 Location 规则忽略。
struct ActiveProjection {
    transactionId: Option<String>,
    captureGeneration: u64,
    requestSentAtMilliseconds: u64,
    requestSentObserved: bool,
    connectionObserved: bool,
    responseObserved: bool,
    capturedRequestBytes: u64,
    capturedResponseBytes: u64,
    lastCaptureFlushAtMilliseconds: u64,
}

/// 活动原始流最多按此周期刷新正文与分包索引；首次出现任一方向数据时仍立即刷新，避免界面显示空树。
const activeCaptureFlushIntervalMilliseconds: u64 = 250;

/// 把传输层会话生命周期投影到统一事务模型；映射只在控制面存在，不反向污染 SOCKS5 核心。
pub struct SocksTransactionProjector {
    recording: RecordingSession,
    active: HashMap<String, ActiveProjection>,
    finalized: HashSet<String>,
    finalizedOrder: VecDeque<String>,
    historyLimit: usize,
    minimumCaptureGeneration: u64,
}

impl SocksTransactionProjector {
    /// 创建与单次 SOCKS5 服务周期绑定的投影器；终态去重集合与会话历史使用相同上限。
    pub fn new(recording: RecordingSession, historyLimit: usize) -> Self {
        Self {
            recording,
            active: HashMap::new(),
            finalized: HashSet::new(),
            finalizedOrder: VecDeque::new(),
            historyLimit: historyLimit.max(1),
            minimumCaptureGeneration: 0,
        }
    }

    /// 推进清空水位并遗忘所有旧代际活动映射；迟到事件随后只进入 finalized 去重集合。
    pub fn advanceCaptureGeneration(&mut self, minimumCaptureGeneration: u64) {
        if minimumCaptureGeneration <= self.minimumCaptureGeneration {
            return;
        }
        self.minimumCaptureGeneration = minimumCaptureGeneration;
        let staleSessionIds = self
            .active
            .iter()
            .filter(|(_, projection)| projection.captureGeneration < self.minimumCaptureGeneration)
            .map(|(sessionId, _)| sessionId.clone())
            .collect::<Vec<_>>();
        for sessionId in staleSessionIds {
            self.rememberFinalized(&sessionId);
        }
    }

    /// 消费一个会话状态；命令和目标可见时创建事务，终态时提交或失败，不为协商噪声伪造记录。
    pub async fn project(&mut self, session: &SessionSnapshot) -> Result<(), CaptureError> {
        if session.captureGeneration < self.minimumCaptureGeneration {
            self.rememberFinalized(&session.sessionId);
            return Ok(());
        }
        if self.finalized.contains(&session.sessionId) {
            return Ok(());
        }
        // CONNECT 在首段字节分类前不得生成原始流事务；HTTP/HTTPS 已由协议处理器按请求录制，
        // 这里只保留真正的 TCP 和 UDP，避免同一传输层连接出现两条不一致的记录。
        if !matches!(
            session.applicationProtocol,
            SessionApplicationProtocol::Tcp
                | SessionApplicationProtocol::Tls
                | SessionApplicationProtocol::Udp
        ) {
            if isTerminal(session.state) {
                self.rememberFinalized(&session.sessionId);
            }
            return Ok(());
        }
        if !self.active.contains_key(&session.sessionId) {
            let Some(input) = beginTransactionInput(session) else {
                if isTerminal(session.state) {
                    self.rememberFinalized(&session.sessionId);
                }
                return Ok(());
            };
            let transactionId = self.recording.beginTransaction(input).await?;
            self.active.insert(
                session.sessionId.clone(),
                ActiveProjection {
                    transactionId,
                    captureGeneration: session.captureGeneration,
                    requestSentAtMilliseconds: session.updatedAtMilliseconds,
                    requestSentObserved: false,
                    connectionObserved: false,
                    responseObserved: false,
                    capturedRequestBytes: 0,
                    capturedResponseBytes: 0,
                    lastCaptureFlushAtMilliseconds: 0,
                },
            );
        }
        self.synchronize(session).await
    }

    /// 合并会话的绝对字节计数和阶段时间；Capture 清空或淘汰后停止追写，避免同一会话被重新创建。
    async fn synchronize(&mut self, session: &SessionSnapshot) -> Result<(), CaptureError> {
        let Some(projection) = self.active.get(&session.sessionId) else {
            return Ok(());
        };
        let transactionId = projection.transactionId.clone();
        let transportReady = matches!(session.state, SessionState::Relaying | SessionState::Closed)
            || session.bytesUp > 0
            || session.bytesDown > 0;
        let responseReady = session.bytesDown > 0;
        let progress = TransactionProgressUpdate {
            requestBodyBytes: Some(session.bytesUp),
            responseBodyBytes: Some(session.bytesDown),
            requestSentAtMilliseconds: (!projection.requestSentObserved)
                .then_some(projection.requestSentAtMilliseconds),
            connectEndAtMilliseconds: (transportReady && !projection.connectionObserved)
                .then_some(session.updatedAtMilliseconds),
            responseStartAtMilliseconds: (responseReady && !projection.responseObserved)
                .then_some(session.updatedAtMilliseconds),
            ..TransactionProgressUpdate::default()
        };

        if let Some(transactionId) = transactionId.as_deref()
            && let Err(error) = self.recording.updateProgress(transactionId, progress).await
        {
            if isDiscardedTransaction(&error) {
                self.rememberFinalized(&session.sessionId);
                return Ok(());
            }
            return Err(error);
        }
        if let Some(projection) = self.active.get_mut(&session.sessionId) {
            projection.requestSentObserved = true;
            projection.connectionObserved |= transportReady;
            projection.responseObserved |= responseReady;
        }

        let captureBaseline = self.active.get(&session.sessionId).map(|projection| {
            (
                projection.capturedRequestBytes,
                projection.capturedResponseBytes,
            )
        });
        let shouldFlushCapture = self
            .active
            .get(&session.sessionId)
            .is_some_and(|projection| shouldFlushCapturedBodies(projection, session));
        if shouldFlushCapture
            && let Some(transactionId) = transactionId.as_deref()
            && let Some(captureBaseline) = captureBaseline
        {
            // 原始流正文以前只在连接关闭时入库，长连接虽然已有字节，详情接口却持续返回零个分包。
            // 首包立即写入，后续更新限频到 250ms；这样界面可实时展开，同时不会让高频 UDP/TCP 每包争用录制锁。
            self.storeCapturedBodies(transactionId, session, captureBaseline)
                .await?;
            if let Some(projection) = self.active.get_mut(&session.sessionId) {
                projection.capturedRequestBytes = session.bytesUp;
                projection.capturedResponseBytes = session.bytesDown;
                projection.lastCaptureFlushAtMilliseconds = session.updatedAtMilliseconds;
            }
        }

        if !isTerminal(session.state) {
            return Ok(());
        }
        let terminalResult = match (transactionId.as_deref(), session.state) {
            (Some(transactionId), SessionState::Closed) => {
                self.recording
                    .commit(
                        transactionId,
                        TransactionCompletion {
                            // SOCKS5 REP=0 表示命令成功；协议列已经区分它与 HTTP 状态码。
                            statusCode: 0,
                            endAtMilliseconds: terminalTimestamp(session),
                            contentType: String::new(),
                        },
                    )
                    .await
            }
            (Some(transactionId), SessionState::Failed) => {
                self.recording
                    .fail(
                        transactionId,
                        TransactionError {
                            code: "socksSessionFailed".to_owned(),
                            messageKey: "error.socks.sessionFailed".to_owned(),
                            params: BTreeMap::from([(
                                "command".to_owned(),
                                session.command.clone(),
                            )]),
                        },
                        terminalTimestamp(session),
                    )
                    .await
            }
            (None, SessionState::Closed | SessionState::Failed) => Ok(()),
            _ => Ok(()),
        };
        if let Err(error) = terminalResult
            && !isDiscardedTransaction(&error)
        {
            return Err(error);
        }
        self.rememberFinalized(&session.sessionId);
        Ok(())
    }

    /// 把 SOCKS5 已成功转发且发生增长的完整原始流写入统一正文存储。
    /// 参数：capturedByteBaseline 是上次已提交的请求、响应线上字节数，用于跳过未变化方向。
    /// 失败语义：正文或片段索引任一步入库失败均原样返回，调用方不得推进刷新水位。
    async fn storeCapturedBodies(
        &self,
        transactionId: &str,
        session: &SessionSnapshot,
        capturedByteBaseline: (u64, u64),
    ) -> Result<(), CaptureError> {
        for (side, direction, capturedBytes, originalBytes, previousOriginalBytes) in [
            (
                MessageSide::Request,
                TrafficDirection::Up,
                &session.capturedBytesUp,
                session.bytesUp,
                capturedByteBaseline.0,
            ),
            (
                MessageSide::Response,
                TrafficDirection::Down,
                &session.capturedBytesDown,
                session.bytesDown,
                capturedByteBaseline.1,
            ),
        ] {
            // 只重写实际增长的方向；双向高频流不能因为单侧收到新包而复制另一侧稳定正文。
            if originalBytes == 0 || originalBytes == previousOriginalBytes {
                continue;
            }
            self.recording
                .storeBody(
                    transactionId,
                    side,
                    BodyWrite {
                        bytes: capturedBytes.toVec(),
                        originalBytes,
                        // SOCKS5 只保证字节流透明转发；未经过协议解码的载荷必须按二进制展示。
                        contentType: "application/octet-stream".to_owned(),
                        encoding: "binary".to_owned(),
                    },
                )
                .await?;
            // 每个片段只保存聚合正文中的偏移和长度；这里在终态统一移交，避免转发热路径触碰异步录制锁。
            self.recording
                .storeStreamPackets(
                    transactionId,
                    side,
                    session
                        .capturedPackets
                        .forDirection(direction)
                        .into_iter()
                        .map(|packet| capture_core::StreamPacket {
                            sequence: packet.sequence,
                            capturedAtMilliseconds: packet.capturedAtMilliseconds,
                            storedOffsetBytes: packet.storedOffsetBytes,
                            storedBytes: packet.storedBytes,
                            originalBytes: packet.originalBytes,
                            truncated: packet.originalBytes > packet.storedBytes as u64,
                        })
                        .collect(),
                )
                .await?;
        }
        Ok(())
    }

    /// 记录已结束会话并按数据面历史上限淘汰去重键；长时间运行不会随总连接数无界增长。
    fn rememberFinalized(&mut self, sessionId: &str) {
        self.active.remove(sessionId);
        if !self.finalized.insert(sessionId.to_owned()) {
            return;
        }
        self.finalizedOrder.push_back(sessionId.to_owned());
        while self.finalizedOrder.len() > self.historyLimit {
            if let Some(expiredSessionId) = self.finalizedOrder.pop_front() {
                self.finalized.remove(&expiredSessionId);
            }
        }
    }
}

/// 判断活动流是否需要把共享镜像刷新到录制存储；每个方向的首包、限频周期和终态增量均不可遗漏。
fn shouldFlushCapturedBodies(projection: &ActiveProjection, session: &SessionSnapshot) -> bool {
    let requestAdvanced = session.bytesUp > projection.capturedRequestBytes;
    let responseAdvanced = session.bytesDown > projection.capturedResponseBytes;
    if !requestAdvanced && !responseAdvanced {
        return false;
    }
    let firstRequest = requestAdvanced && projection.capturedRequestBytes == 0;
    let firstResponse = responseAdvanced && projection.capturedResponseBytes == 0;
    firstRequest
        || firstResponse
        || isTerminal(session.state)
        || session
            .updatedAtMilliseconds
            .saturating_sub(projection.lastCaptureFlushAtMilliseconds)
            >= activeCaptureFlushIntervalMilliseconds
}

/// 将 SOCKS5 稳定端点文本拆成 host/port；IPv4、IPv6 和域名共用同一严格入口。
fn parseTargetAddress(targetAddress: &str) -> Option<(String, u16)> {
    if let Ok(socketAddress) = targetAddress.parse::<SocketAddr>() {
        return Some((socketAddress.ip().to_string(), socketAddress.port()));
    }
    let (host, portText) = targetAddress.rsplit_once(':')?;
    let port = portText.parse::<u16>().ok()?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_owned(), port))
}

/// 在命令和目标均已解析后构造统一事务输入；协商失败没有可用 Location，因此保持只在会话诊断中。
fn beginTransactionInput(session: &SessionSnapshot) -> Option<BeginTransaction> {
    let method = match session.command.as_str() {
        "connect" => "CONNECT",
        "bind" => "BIND",
        "udpAssociate" => "UDP ASSOCIATE",
        _ => return None,
    };
    let (host, port) = parseTargetAddress(&session.targetAddress)?;
    let displayScheme = match session.applicationProtocol {
        SessionApplicationProtocol::Tls => "https",
        SessionApplicationProtocol::Udp => "udp",
        SessionApplicationProtocol::Tcp => "tcp",
        _ => return None,
    };
    let displayHost = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    Some(BeginTransaction {
        protocol: TransactionProtocol::Socks,
        method: method.to_owned(),
        location: ResolvedLocation {
            protocol: "socks".to_owned(),
            host,
            port,
            // SOCKS5 只解析网络端点，没有应用层 URL path；空值防止界面伪造“根路径”。
            path: String::new(),
            query: String::new(),
            // SOCKS5 只是入口协议；事务标题展示首段字节确认后的实际载荷类型，避免把 HTTPS 误写成 socks5://。
            display: format!("{displayScheme}://{displayHost}:{port}"),
        },
        clientAddress: session.clientAddress.clone(),
        clientProcessName: None,
        clientProcessId: None,
        contentType: String::new(),
        startAtMilliseconds: session.createdAtMilliseconds,
    })
}

/// 判断会话是否已经离开所有可继续更新的网络状态。
const fn isTerminal(state: SessionState) -> bool {
    matches!(state, SessionState::Closed | SessionState::Failed)
}

/// Capture clear、FIFO 或重复终态都意味着当前投影已经结束，不应影响 SOCKS5 数据转发。
const fn isDiscardedTransaction(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::TransactionNotFound | CaptureError::TransactionFinished
    )
}

/// 选择数据面发布的关闭时间；异常旧快照缺少 closedAt 时使用最后更新时间保持非零终态。
fn terminalTimestamp(session: &SessionSnapshot) -> u64 {
    if session.closedAtMilliseconds == 0 {
        session.updatedAtMilliseconds
    } else {
        session.closedAtMilliseconds
    }
}
