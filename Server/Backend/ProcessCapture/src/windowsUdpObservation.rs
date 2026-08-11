//! 承载 UDP 批收、五元组关联、分片重组、统一封包决策与唯一回注。
//!
//! 收包线程只搬运固定批次；唯一 resolver 按 QPC 顺序决定每个真实包的修改、丢弃或回注。

use super::*;

/// 运行 UDP 拦截与统一写线线程；只有 resolver 可以回注，任何已拦截包都必须恰好得到一次终态。
///
/// 运行上下文：SOCKET/FLOW 先维护选中 PID 与五元组，当前线程只为精确命中的双向数据报
/// 解析完整 UDP payload。驱动、处理器或录制通道故障时先原样排空尚未决策的包再结束线程，
/// 禁止因控制面故障把无关系统流量留在 WinDivert 队列中。
pub(super) fn spawnUdpObservationWorkers(
    divert: WinDivert<NetworkLayer>,
    context: UdpObservationContext,
) -> Result<Vec<JoinHandle<()>>, ProcessCaptureError> {
    let divert = Arc::new(divert);
    let context = Arc::new(context);
    let (sender, receiver) = sync_channel::<PendingUdpObservation>(maximumUdpObservationPackets);
    let emergency = Arc::new(Mutex::new(VecDeque::<PendingUdpObservation>::new()));
    // resolver 必须先创建；若后续 recv 线程创建失败，失败闭包会释放唯一 sender，resolver 排空并退出，
    // 当前函数同步 join 后再返回错误，避免启动回滚留下脱管线程。
    let resolverDivert = Arc::clone(&divert);
    let resolverContext = Arc::clone(&context);
    let resolverEmergency = Arc::clone(&emergency);
    let resolverWorker = thread::Builder::new()
        .name("process-capture-udp-resolver".to_owned())
        .spawn(move || runUdpResolver(resolverDivert, resolverContext, receiver, resolverEmergency))
        .map_err(|error| ProcessCaptureError::Worker {
            worker: "NETWORK UDP resolver 创建",
            detail: error.to_string(),
        })?;
    let receiveDivert = Arc::clone(&divert);
    let receiveContext = Arc::clone(&context);
    let receiveEmergency = Arc::clone(&emergency);
    let receiverWorker = match thread::Builder::new()
        .name("process-capture-udp-intercept".to_owned())
        .spawn(move || {
            // 单次 WinDivertRecvEx 最多取 255 包；媒体响应突发不会在逐包用户态调用之间耗尽驱动队列。
            let mut buffer = vec![
                0_u8;
                maximumPacketBytes * usize::from(udpReceiveBatchPackets)
            ];
            'receive: loop {
                if receiveContext.stopRequested.load(Ordering::Acquire) {
                    break;
                }
                let mut packets = match receiveDivert.recv_ex(&mut buffer, udpReceiveBatchPackets) {
                    Ok(packets) => packets.into_iter(),
                    Err(error) => {
                        if !receiveContext.stopRequested.load(Ordering::Acquire) {
                            recordWorkerError(
                                &receiveContext.lastError,
                                "NETWORK UDP 拦截接收",
                                error.to_string(),
                            );
                        }
                        break;
                    }
                };
                while let Some(packet) = packets.next() {
                    let observation = pendingUdpObservation(
                        packet,
                        receiveContext
                            .nextUdpCaptureSequence
                            .fetch_add(1, Ordering::Relaxed),
                    );
                    match sender.try_send(observation) {
                        Ok(()) => {}
                        Err(TrySendError::Full(observation)) => {
                            let mut emergency = receiveEmergency
                                .lock()
                                .expect("UDP pending emergency 锁中毒");
                            emergency.push_back(observation);
                            emergency.extend(packets.map(|packet| {
                                pendingUdpObservation(
                                    packet,
                                    receiveContext
                                        .nextUdpCaptureSequence
                                        .fetch_add(1, Ordering::Relaxed),
                                )
                            }));
                            recordWorkerError(
                                &receiveContext.lastError,
                                "NETWORK UDP 拦截队列",
                                format!(
                                    "UDP 关联队列达到固定上限 {maximumUdpObservationPackets}，当前批次已保留到 emergency"
                                ),
                            );
                            break 'receive;
                        }
                        Err(TrySendError::Disconnected(observation)) => {
                            let mut emergency = receiveEmergency
                                .lock()
                                .expect("UDP pending emergency 锁中毒");
                            emergency.push_back(observation);
                            emergency.extend(packets.map(|packet| {
                                pendingUdpObservation(
                                    packet,
                                    receiveContext
                                        .nextUdpCaptureSequence
                                        .fetch_add(1, Ordering::Relaxed),
                                )
                            }));
                            recordWorkerError(
                                &receiveContext.lastError,
                                "NETWORK UDP 拦截 resolver",
                                "resolver 通道提前关闭，当前驱动批次已完整保留".to_owned(),
                            );
                            break 'receive;
                        }
                    }
                }
            }
        }) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = resolverWorker.join();
            return Err(ProcessCaptureError::Worker {
                worker: "NETWORK UDP 拦截创建",
                detail: error.to_string(),
            });
        }
    };
    Ok(vec![receiverWorker, resolverWorker])
}

/// 串行完成 UDP 归属、统一规则和回注；异常退出前原样释放所有尚未获得规则终态的包。
fn runUdpResolver(
    divert: Arc<WinDivert<NetworkLayer>>,
    context: Arc<UdpObservationContext>,
    receiver: Receiver<PendingUdpObservation>,
    emergency: Arc<Mutex<VecDeque<PendingUdpObservation>>>,
) {
    let mut fragments = UdpFragmentAssembler::<PendingUdpObservation>::default();
    let mut ownerSnapshot = OwnerSnapshotCache::default();
    let mut unresolved = VecDeque::new();
    loop {
        let observation = match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(observation) => observation,
            Err(RecvTimeoutError::Timeout) => {
                if let Err(detail) = fragments.pollExpired() {
                    recordWorkerError(&context.lastError, "NETWORK UDP 分片超时", detail);
                    context.stopRequested.store(true, Ordering::Release);
                    unresolved.append(&mut fragments.drainPendingObservations());
                    break;
                }
                if context.stopRequested.load(Ordering::Acquire) {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match processUdpObservation(
            &observation,
            &divert,
            &context,
            &mut fragments,
            &mut ownerSnapshot,
        ) {
            Ok(()) => context
                .flowTable
                .advanceUdpObservationWatermark(observation.capturedAtCounter),
            Err(UdpObservationError::Packet(detail)) => {
                eprintln!("NETWORK UDP 原样放行不可解析包：{detail}");
                if let Err(sendError) = sendOriginal(&divert, &observation) {
                    recordWorkerError(&context.lastError, "NETWORK UDP 原包回注", sendError);
                    context.stopRequested.store(true, Ordering::Release);
                    break;
                }
                context
                    .flowTable
                    .advanceUdpObservationWatermark(observation.capturedAtCounter);
            }
            Err(UdpObservationError::Owner(detail))
            | Err(UdpObservationError::Processing(detail)) => {
                recordWorkerError(&context.lastError, "NETWORK UDP 统一数据面", detail);
                context.stopRequested.store(true, Ordering::Release);
                unresolved.push_back(observation);
                unresolved.append(&mut fragments.drainPendingObservations());
                break;
            }
            Err(UdpObservationError::Restore(detail, mut packets)) => {
                // 分片组已经离开重组器，错误必须携带整组尚未写线的原包；只保留最后一片会造成真实 UDP 报文残缺。
                recordWorkerError(&context.lastError, "NETWORK UDP 统一数据面", detail);
                context.stopRequested.store(true, Ordering::Release);
                unresolved.append(&mut packets);
                unresolved.append(&mut fragments.drainPendingObservations());
                break;
            }
            Err(UdpObservationError::Send(detail)) => {
                recordWorkerError(&context.lastError, "NETWORK UDP 回注", detail);
                context.stopRequested.store(true, Ordering::Release);
                unresolved.append(&mut fragments.drainPendingObservations());
                break;
            }
            Err(UdpObservationError::Recording(detail)) => {
                // 当前包已经成功写线，录制失败不得二次回注；其余未决策包仍需原样释放。
                recordWorkerError(&context.lastError, "NETWORK UDP 录制", detail);
                context.stopRequested.store(true, Ordering::Release);
                unresolved.append(&mut fragments.drainPendingObservations());
                break;
            }
        }
    }
    // fatal 会先置 stopRequested，收包线程在 shutdown_recv 唤醒后关闭 sender；等待断开才能证明
    // recv_ex 已经取得但尚未入队的窗口也被接管，禁止在生产者仍可能发布时提前析构 receiver。
    while let Ok(observation) = receiver.recv() {
        unresolved.push_back(observation);
    }
    unresolved.append(&mut emergency.lock().expect("UDP 批次 emergency 锁中毒"));
    unresolved.append(&mut fragments.drainPendingObservations());
    flushOriginalPackets(&divert, unresolved, &context.lastError);
}

/// 保存 WinDivert 拦截包及其不可变归属水位；完整地址元数据必须随正文进入唯一 resolver 回注。
#[derive(Clone)]
pub(super) struct PendingUdpObservation {
    captureSequence: u64,
    packet: WinDivertPacket<'static, NetworkLayer>,
    outbound: bool,
    interfaceIndex: u32,
    capturedAtMilliseconds: u64,
    capturedAt: Instant,
    capturedAtCounter: i64,
    associationDeadline: Instant,
}

/// 把 WinDivert 批收视图复制成独立副本；批次迭代器释放后正文和捕获水位仍完整有效。
fn pendingUdpObservation(
    packet: WinDivertPacket<'_, NetworkLayer>,
    captureSequence: u64,
) -> PendingUdpObservation {
    let capturedAt = Instant::now();
    let outbound = packet.address.outbound();
    let interfaceIndex = packet.address.interface_index();
    let capturedAtCounter = packet.address.event_timestamp();
    PendingUdpObservation {
        captureSequence,
        packet: packet.into_owned(),
        outbound,
        interfaceIndex,
        capturedAtMilliseconds: currentTimeMilliseconds(),
        capturedAt,
        capturedAtCounter,
        associationDeadline: capturedAt + Duration::from_millis(associationWaitMilliseconds),
    }
}

/// 区分包级异常、owner 瞬态与持久化契约破坏；只有最后一类会终止当前录制代际。
enum UdpObservationError {
    Packet(String),
    Owner(String),
    Processing(String),
    Restore(String, VecDeque<PendingUdpObservation>),
    Send(String),
    Recording(String),
}

/// 原样回注一个尚未经过统一规则的包；失败表示系统原流量无法恢复，必须终止当前捕获代际。
fn sendOriginal(
    divert: &WinDivert<NetworkLayer>,
    observation: &PendingUdpObservation,
) -> Result<(), String> {
    divert
        .send(&observation.packet)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// 按捕获顺序原样释放故障或停止窗口内的全部包；同一 captureSequence 只允许回注一次。
fn flushOriginalPackets(
    divert: &WinDivert<NetworkLayer>,
    pending: VecDeque<PendingUdpObservation>,
    lastError: &Mutex<Option<String>>,
) {
    let mut ordered = pending.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|observation| observation.captureSequence);
    ordered.dedup_by_key(|observation| observation.captureSequence);
    for observation in ordered {
        if let Err(detail) = sendOriginal(divert, &observation) {
            recordWorkerError(lastError, "NETWORK UDP 故障排空", detail);
            break;
        }
    }
}

/// 表示一个本地 UDP 端点在全系统 owner 快照中的无歧义程度；热路径只复制该常量大小状态。
#[derive(Clone, Copy, Default)]
struct EndpointOwners {
    processId: Option<u32>,
    ambiguous: bool,
}

impl EndpointOwners {
    /// 合并一个 owner；同一 PID 的精确地址和通配地址重复项仍视为唯一，SO_REUSE 才标记歧义。
    fn insert(&mut self, processId: u32) {
        match self.processId {
            None => self.processId = Some(processId),
            Some(existing) if existing == processId => {}
            Some(_) => self.ambiguous = true,
        }
    }

    /// 合并精确地址与通配地址索引；结果不分配集合，供每包 O(1) 查询。
    fn merge(self, other: Self) -> Self {
        let mut merged = self;
        if let Some(processId) = other.processId {
            merged.insert(processId);
        }
        merged.ambiguous |= other.ambiguous;
        merged
    }
}

/// 在单一 resolver 内短时复用全系统双栈 owner 快照，并预建本地端点索引。
///
/// 系统表枚举、排序和集合构造只发生在 TTL 刷新；整机未选高 pps 流量的逐包路径为两次
/// `HashMap` 查询，避免 O(包数×系统 socket 数) 扫描拖垮固定捕获队列。
#[derive(Default)]
struct OwnerSnapshotCache {
    ownersByEndpoint: HashMap<(IpAddr, u16), EndpointOwners>,
    bindingsByProcess: HashMap<u32, Vec<OwnedUdpBinding>>,
    refreshedAt: Option<Instant>,
}

impl OwnerSnapshotCache {
    /// 在需要时刷新一次 owner 索引；尺寸增长会在当前副本 deadline 内重试，超时才升级故障。
    fn refreshUntil(&mut self, deadline: Instant) -> Result<(), UdpObservationError> {
        let fresh = self
            .refreshedAt
            .is_some_and(|refreshedAt| refreshedAt.elapsed() < ownerSnapshotLifetime);
        if fresh {
            return Ok(());
        }
        let bindings = enumerateOwnedUdpBindingsUntil(deadline)?;
        self.ownersByEndpoint.clear();
        self.bindingsByProcess.clear();
        for binding in bindings {
            self.ownersByEndpoint
                .entry((binding.localAddress, binding.localPort))
                .or_default()
                .insert(binding.processId);
            self.bindingsByProcess
                .entry(binding.processId)
                .or_default()
                .push(binding);
        }
        self.refreshedAt = Some(Instant::now());
        Ok(())
    }

    /// O(1) 合并精确地址和同地址族通配绑定；返回值保留全系统共享端口歧义。
    fn ownersFor(&self, localAddress: IpAddr, localPort: u16) -> EndpointOwners {
        let exact = self
            .ownersByEndpoint
            .get(&(localAddress, localPort))
            .copied()
            .unwrap_or_default();
        let unspecifiedAddress = match localAddress {
            IpAddr::V4(_) => IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
        };
        let wildcard = self
            .ownersByEndpoint
            .get(&(unspecifiedAddress, localPort))
            .copied()
            .unwrap_or_default();
        exact.merge(wildcard)
    }

    /// 返回唯一 owner 的预分组绑定；切片有效期仅限当前 TTL 快照。
    fn bindingsForProcess(&self, processId: u32) -> &[OwnedUdpBinding] {
        self.bindingsByProcess
            .get(&processId)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// 在独立 resolver 中完成五元组归属、统一规则、唯一回注和录制；任何等待都不占用 recv 线程。
fn processUdpObservation(
    observation: &PendingUdpObservation,
    divert: &WinDivert<NetworkLayer>,
    context: &UdpObservationContext,
    fragmentAssembler: &mut UdpFragmentAssembler<PendingUdpObservation>,
    ownerSnapshot: &mut OwnerSnapshotCache,
) -> Result<(), UdpObservationError> {
    let fragment = inspectUdpFragment(&observation.packet.data, observation.outbound)
        .map_err(UdpObservationError::Packet)?;
    if let UdpPacketFragment::Fragment(part) = fragment {
        let disposition =
            if let Some((sourceAddress, sourcePort, destinationAddress, destinationPort)) =
                firstFragmentTuple(&part)
            {
                let parsed = ObservedUdpPacket {
                    sourceAddress,
                    destinationAddress,
                    sourcePort,
                    destinationPort,
                    payloadOffset: 0,
                    payloadEnd: 0,
                };
                let direction = observeUdpTupleWithAssociationWait(
                    &parsed,
                    observation,
                    context,
                    ownerSnapshot,
                )?;
                Some(match direction {
                    PacketDirection::ObservedUp(target) => UdpFragmentDisposition::Selected {
                        target: crate::flowTable::withIpv6Scope(target, observation.interfaceIndex),
                        direction: UdpDatagramDirection::Up,
                        clientAddress: SocketAddr::new(sourceAddress, sourcePort),
                        capturedAtMilliseconds: observation.capturedAtMilliseconds,
                    },
                    PacketDirection::ObservedDown(target) => UdpFragmentDisposition::Selected {
                        target: crate::flowTable::withIpv6Scope(target, observation.interfaceIndex),
                        direction: UdpDatagramDirection::Down,
                        clientAddress: SocketAddr::new(destinationAddress, destinationPort),
                        capturedAtMilliseconds: observation.capturedAtMilliseconds,
                    },
                    _ => UdpFragmentDisposition::Ignored,
                })
            } else {
                None
            };
        let reassembledResult = fragmentAssembler.pushWithObservation(
            part,
            disposition,
            observation.clone(),
            observation.packet.data.len(),
        );
        let reassembled = reassembledResult.map_err(UdpObservationError::Recording)?;
        let Some(reassembled) = reassembled else {
            return Ok(());
        };
        let Some(event) = reassembled.event else {
            flushOriginalPackets(divert, reassembled.observations, &context.lastError);
            return Ok(());
        };
        let decision = processDatagram(event.clone(), context)?;
        let Some(event) = applyFragmentDecision(divert, reassembled.observations, event, decision)?
        else {
            return Ok(());
        };
        recordForwardedDatagram(
            event,
            reassembled.packetCount,
            reassembled.packetBytes,
            context,
        )?;
        return Ok(());
    }
    let direction = observeUdpPacketWithAssociationWait(observation, context, ownerSnapshot)?;
    let (target, datagramDirection, packetCounter, byteCounter) = match direction {
        PacketDirection::ObservedUp(target) => (
            target,
            UdpDatagramDirection::Up,
            &context.observedPacketsUp,
            &context.bytesUp,
        ),
        PacketDirection::ObservedDown(target) => (
            target,
            UdpDatagramDirection::Down,
            &context.observedPacketsDown,
            &context.bytesDown,
        ),
        _ => {
            sendOriginal(divert, observation).map_err(UdpObservationError::Send)?;
            return Ok(());
        }
    };
    let event = udpDatagramEvent(
        &observation.packet.data,
        crate::flowTable::withIpv6Scope(target, observation.interfaceIndex),
        datagramDirection,
        observation.capturedAtMilliseconds,
    )
    .ok_or_else(|| UdpObservationError::Packet("命中五元组的数据报缺少完整 UDP 头".to_owned()))?;
    let decision = processDatagram(event.clone(), context)?;
    let Some(event) = applyWholePacketDecision(divert, observation, event, decision)? else {
        return Ok(());
    };
    packetCounter.fetch_add(1, Ordering::Relaxed);
    byteCounter.fetch_add(observation.packet.data.len() as u64, Ordering::Relaxed);
    appendUdpEvent(event, context)
}

/// 调用控制面装配的共享封包处理器；未安装处理器时保持透明直通。
fn processDatagram(
    event: crate::UdpDatagramEvent,
    context: &UdpObservationContext,
) -> Result<UdpDatagramDecision, UdpObservationError> {
    let processor = context
        .udpDatagramProcessor
        .read()
        .expect("UDP 封包处理器读锁中毒")
        .clone();
    let Some(processor) = processor else {
        return Ok(UdpDatagramDecision::Forward {
            payload: event.payload,
            modifications: Vec::new(),
        });
    };
    processor
        .process(&event)
        .map_err(UdpObservationError::Processing)
}

/// 把普通 UDP 数据报的规则结果写回真实 IP 包；缩短正文时同步更新 IP/UDP 长度并重算校验和。
fn applyWholePacketDecision(
    divert: &WinDivert<NetworkLayer>,
    observation: &PendingUdpObservation,
    mut event: crate::UdpDatagramEvent,
    decision: UdpDatagramDecision,
) -> Result<Option<crate::UdpDatagramEvent>, UdpObservationError> {
    let UdpDatagramDecision::Forward {
        payload,
        modifications,
    } = decision
    else {
        return Ok(None);
    };
    if payload.len() > event.payload.len() {
        return Err(UdpObservationError::Processing(format!(
            "WinDivert UDP 封包修改不能扩展已捕获数据报：原始 {} 字节，输出 {} 字节",
            event.payload.len(),
            payload.len()
        )));
    }
    let parsed = parseObservedUdpPacket(&observation.packet.data)
        .map_err(|error| UdpObservationError::Packet(error.to_string()))?
        .ok_or_else(|| UdpObservationError::Packet("完整 UDP 数据报被识别为分片".to_owned()))?;
    let mut packet = observation.packet.clone();
    replaceWholeUdpPayload(&mut packet, &parsed, &payload)?;
    packet
        .recalculate_checksums(ChecksumFlags::default())
        .map_err(|error| UdpObservationError::Processing(error.to_string()))?;
    divert
        .send(&packet)
        .map_err(|error| UdpObservationError::Send(error.to_string()))?;
    event.payload = payload;
    event.modifications = modifications;
    Ok(Some(event))
}

/// 把完整分片组的缩短正文映射回原分片边界；不再承载内容的尾片不会回注。
fn applyFragmentDecision(
    divert: &WinDivert<NetworkLayer>,
    observations: VecDeque<PendingUdpObservation>,
    mut event: crate::UdpDatagramEvent,
    decision: UdpDatagramDecision,
) -> Result<Option<crate::UdpDatagramEvent>, UdpObservationError> {
    let UdpDatagramDecision::Forward {
        payload,
        modifications,
    } = decision
    else {
        return Ok(None);
    };
    if payload.len() > event.payload.len() {
        return Err(UdpObservationError::Processing(format!(
            "WinDivert UDP 分片修改不能扩展已捕获数据报：原始 {} 字节，输出 {} 字节",
            event.payload.len(),
            payload.len()
        )));
    }
    let mut packets = observations.into_iter().collect::<Vec<_>>();
    packets.sort_by_key(|observation| observation.captureSequence);
    let originalPackets = packets.iter().cloned().collect::<VecDeque<_>>();
    let (sourceAddress, sourcePort, destinationAddress, destinationPort, udpHeaderOffset) =
        fragmentTransportMetadata(&packets).map_err(|error| match error {
            UdpObservationError::Packet(detail) | UdpObservationError::Processing(detail) => {
                UdpObservationError::Restore(detail, originalPackets.clone())
            }
            other => other,
        })?;
    let checksum = udpChecksum(
        sourceAddress,
        sourcePort,
        destinationAddress,
        destinationPort,
        &payload,
    );
    let fragmentableBytes =
        buildFragmentableUdpBytes(&packets, udpHeaderOffset, &payload, checksum)
            .map_err(|error| UdpObservationError::Restore(error, originalPackets.clone()))?;
    let mut rewrittenPackets = Vec::with_capacity(packets.len());
    for mut observation in packets {
        let original = observation.clone();
        let retained =
            rewriteFragmentPayload(&mut observation, &fragmentableBytes).map_err(|error| {
                match error {
                    UdpObservationError::Packet(detail)
                    | UdpObservationError::Processing(detail) => {
                        UdpObservationError::Restore(detail, originalPackets.clone())
                    }
                    other => other,
                }
            })?;
        if retained {
            rewrittenPackets.push((observation, original));
        }
    }
    for (packetIndex, (observation, _original)) in rewrittenPackets.iter().enumerate() {
        if let Err(error) = divert.send(&observation.packet) {
            // 已成功回注的前缀不能重复发送；失败位置及其后缀仍以原字节释放，避免把剩余分片永久留在驱动队列。
            let unsentPackets = rewrittenPackets
                .iter()
                .skip(packetIndex)
                .map(|(_modified, original)| original.clone())
                .collect();
            return Err(UdpObservationError::Restore(
                error.to_string(),
                unsentPackets,
            ));
        }
    }
    event.payload = payload;
    event.modifications = modifications;
    Ok(Some(event))
}

/// 重建完整 UDP 包正文并修正协议长度；输出增长由调用方提前拒绝，因此分配始终不超过原包预算。
fn replaceWholeUdpPayload(
    packet: &mut WinDivertPacket<'static, NetworkLayer>,
    parsed: &ObservedUdpPacket,
    payload: &[u8],
) -> Result<(), UdpObservationError> {
    let original = packet.data.as_ref();
    let udpOffset = parsed.payloadOffset - 8;
    let removedBytes = parsed.payloadEnd - parsed.payloadOffset;
    let newPacketLength = original.len() - removedBytes + payload.len();
    let mut rewritten = Vec::with_capacity(newPacketLength);
    rewritten.extend_from_slice(&original[..parsed.payloadOffset]);
    rewritten.extend_from_slice(payload);
    rewritten.extend_from_slice(&original[parsed.payloadEnd..]);
    writeUdpLength(&mut rewritten, udpOffset, payload.len())
        .map_err(UdpObservationError::Processing)?;
    writeIpPacketLength(&mut rewritten)?;
    packet.data = std::borrow::Cow::Owned(rewritten);
    Ok(())
}

/// 从分片组首片提取扩展头与 UDP 头，生成修改后的完整 fragmentable part。
fn buildFragmentableUdpBytes(
    packets: &[PendingUdpObservation],
    udpHeaderOffset: usize,
    payload: &[u8],
    checksum: u16,
) -> Result<Vec<u8>, String> {
    let firstPart = packets
        .iter()
        .filter_map(|observation| {
            inspectUdpFragment(&observation.packet.data, observation.outbound)
                .ok()
                .and_then(|fragment| match fragment {
                    UdpPacketFragment::Fragment(part) if fragmentPayloadRange(&part).0 == 0 => {
                        Some(part)
                    }
                    _ => None,
                })
        })
        .next()
        .ok_or_else(|| "完整 UDP 分片组缺少零偏移首片".to_owned())?;
    let headerEnd = udpHeaderOffset + 8;
    let (headerBytes, sourceAddress) = fragmentPayloadPrefix(&firstPart, headerEnd)
        .ok_or_else(|| "UDP 首片未完整包含传输层头".to_owned())?;
    let mut fragmentable = Vec::with_capacity(headerEnd + payload.len());
    fragmentable.extend_from_slice(headerBytes);
    writeUdpLength(&mut fragmentable, udpHeaderOffset, payload.len())?;
    let checksumOffset = udpHeaderOffset + 6;
    let originalChecksum = u16::from_be_bytes([
        fragmentable[checksumOffset],
        fragmentable[checksumOffset + 1],
    ]);
    let wireChecksum = if matches!(sourceAddress, IpAddr::V4(_)) && originalChecksum == 0 {
        0
    } else {
        checksum
    };
    fragmentable[checksumOffset..checksumOffset + 2].copy_from_slice(&wireChecksum.to_be_bytes());
    fragmentable.extend_from_slice(payload);
    Ok(fragmentable)
}

/// 累计已成功写线的分片物理包并发布单条重组事务；丢弃动作不伪造已转发指标。
fn recordForwardedDatagram(
    event: crate::UdpDatagramEvent,
    packetCount: u64,
    packetBytes: u64,
    context: &UdpObservationContext,
) -> Result<(), UdpObservationError> {
    let (packetCounter, byteCounter) = match event.direction {
        UdpDatagramDirection::Up => (&context.observedPacketsUp, &context.bytesUp),
        UdpDatagramDirection::Down => (&context.observedPacketsDown, &context.bytesDown),
    };
    packetCounter.fetch_add(packetCount, Ordering::Relaxed);
    byteCounter.fetch_add(packetBytes, Ordering::Relaxed);
    appendUdpEvent(event, context)
}

/// 从完整物理片组提取一次 UDP 伪首部所需字段；缺少首片表示重组器契约遭到破坏。
fn fragmentTransportMetadata(
    observations: &[PendingUdpObservation],
) -> Result<(IpAddr, u16, IpAddr, u16, usize), UdpObservationError> {
    for observation in observations {
        let fragment = inspectUdpFragment(&observation.packet.data, observation.outbound)
            .map_err(UdpObservationError::Packet)?;
        let UdpPacketFragment::Fragment(part) = fragment else {
            return Err(UdpObservationError::Packet(
                "分片组混入了完整 UDP 数据报".to_owned(),
            ));
        };
        let Some((sourceAddress, sourcePort, destinationAddress, destinationPort)) =
            firstFragmentTuple(&part)
        else {
            continue;
        };
        let udpHeaderOffset = fragmentUdpHeaderOffset(&part)
            .ok_or_else(|| UdpObservationError::Packet("UDP 首片缺少传输层头偏移".to_owned()))?;
        return Ok((
            sourceAddress,
            sourcePort,
            destinationAddress,
            destinationPort,
            udpHeaderOffset,
        ));
    }
    Err(UdpObservationError::Packet(
        "完整 UDP 分片组缺少首片".to_owned(),
    ))
}

/// 将新 fragmentable part 的对应区间写回单个物理片；尾部空片返回 false 并由调用方丢弃。
fn rewriteFragmentPayload(
    observation: &mut PendingUdpObservation,
    fragmentableBytes: &[u8],
) -> Result<bool, UdpObservationError> {
    let fragment = inspectUdpFragment(&observation.packet.data, observation.outbound)
        .map_err(UdpObservationError::Packet)?;
    let UdpPacketFragment::Fragment(part) = fragment else {
        return Err(UdpObservationError::Packet(
            "分片修改阶段混入完整 UDP 数据报".to_owned(),
        ));
    };
    let (globalOffset, packetOffset, fragmentLength) = fragmentPayloadRange(&part);
    if globalOffset >= fragmentableBytes.len() {
        return Ok(false);
    }
    let retainedLength = fragmentLength.min(fragmentableBytes.len() - globalOffset);
    let original = observation.packet.data.as_ref();
    let mut rewritten = Vec::with_capacity(packetOffset + retainedLength);
    rewritten.extend_from_slice(&original[..packetOffset]);
    rewritten.extend_from_slice(&fragmentableBytes[globalOffset..globalOffset + retainedLength]);
    writeFragmentPacketLength(
        &mut rewritten,
        packetOffset,
        globalOffset + retainedLength < fragmentableBytes.len(),
    )?;
    observation.packet.data = std::borrow::Cow::Owned(rewritten);
    observation
        .packet
        .recalculate_checksums(ChecksumFlags::default().set_no_udp())
        .map_err(|error| UdpObservationError::Processing(error.to_string()))?;
    Ok(true)
}

/// 写入 UDP 头声明长度；超出标准 UDP 16 位边界时拒绝修改而不发送半有效包。
fn writeUdpLength(packet: &mut [u8], udpOffset: usize, payloadLength: usize) -> Result<(), String> {
    let udpLength =
        u16::try_from(payloadLength + 8).map_err(|_| "UDP 修改正文超过 65527 字节".to_owned())?;
    packet[udpOffset + 4..udpOffset + 6].copy_from_slice(&udpLength.to_be_bytes());
    Ok(())
}

/// 修正普通 IPv4/IPv6 包声明长度；输入必须是 NETWORK 层的完整 IP 包。
fn writeIpPacketLength(packet: &mut [u8]) -> Result<(), UdpObservationError> {
    match packet[0] >> 4 {
        4 => {
            let totalLength = u16::try_from(packet.len()).map_err(|_| {
                UdpObservationError::Processing("IPv4 修改包超过 65535 字节".to_owned())
            })?;
            packet[2..4].copy_from_slice(&totalLength.to_be_bytes());
        }
        6 => {
            let payloadLength = u16::try_from(packet.len().saturating_sub(40)).map_err(|_| {
                UdpObservationError::Processing("IPv6 修改包超过标准 payload 长度".to_owned())
            })?;
            packet[4..6].copy_from_slice(&payloadLength.to_be_bytes());
        }
        version => {
            return Err(UdpObservationError::Packet(format!(
                "UDP 修改阶段遇到不支持的 IP 版本 {version}"
            )));
        }
    }
    Ok(())
}

/// 修正单个 IPv4/IPv6 分片的长度与末片标志；分片偏移保持原值以维持重组位置。
fn writeFragmentPacketLength(
    packet: &mut [u8],
    fragmentPayloadOffset: usize,
    moreFragments: bool,
) -> Result<(), UdpObservationError> {
    writeIpPacketLength(packet)?;
    match packet[0] >> 4 {
        4 => {
            let mut fragmentField = u16::from_be_bytes([packet[6], packet[7]]);
            fragmentField = if moreFragments {
                fragmentField | 0x2000
            } else {
                fragmentField & !0x2000
            };
            packet[6..8].copy_from_slice(&fragmentField.to_be_bytes());
        }
        6 => {
            let fragmentHeaderOffset = fragmentPayloadOffset.checked_sub(8).ok_or_else(|| {
                UdpObservationError::Packet("IPv6 分片缺少 Fragment Header".to_owned())
            })?;
            let mut fragmentField = u16::from_be_bytes([
                packet[fragmentHeaderOffset + 2],
                packet[fragmentHeaderOffset + 3],
            ]);
            fragmentField = if moreFragments {
                fragmentField | 1
            } else {
                fragmentField & !1
            };
            packet[fragmentHeaderOffset + 2..fragmentHeaderOffset + 4]
                .copy_from_slice(&fragmentField.to_be_bytes());
        }
        _ => unreachable!("IP 版本已经由 writeIpPacketLength 校验"),
    }
    Ok(())
}

/// 计算覆盖完整 UDP 数据报的标准一补校验和；结果零在线上编码为 `0xffff`。
fn udpChecksum(
    sourceAddress: IpAddr,
    sourcePort: u16,
    destinationAddress: IpAddr,
    destinationPort: u16,
    payload: &[u8],
) -> u16 {
    let udpLength =
        u16::try_from(payload.len() + 8).expect("UDP length 字段限制正文不超过 65527 字节");
    let mut sum = 0_u64;
    match (sourceAddress, destinationAddress) {
        (IpAddr::V4(source), IpAddr::V4(destination)) => {
            addChecksumBytes(&mut sum, &source.octets());
            addChecksumBytes(&mut sum, &destination.octets());
            sum += u64::from(crate::flowTable::udpProtocol);
            sum += u64::from(udpLength);
        }
        (IpAddr::V6(source), IpAddr::V6(destination)) => {
            addChecksumBytes(&mut sum, &source.octets());
            addChecksumBytes(&mut sum, &destination.octets());
            sum += u64::from(udpLength);
            sum += u64::from(crate::flowTable::udpProtocol);
        }
        _ => unreachable!("UDP 源目标地址族已在流表登记时校验一致"),
    }
    addChecksumBytes(&mut sum, &sourcePort.to_be_bytes());
    addChecksumBytes(&mut sum, &destinationPort.to_be_bytes());
    addChecksumBytes(&mut sum, &udpLength.to_be_bytes());
    addChecksumBytes(&mut sum, &[0, 0]);
    addChecksumBytes(&mut sum, payload);
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    let checksum = !(sum as u16);
    if checksum == 0 { u16::MAX } else { checksum }
}

/// 按网络序把任意偶数或奇数长度字节并入一补和；奇数字节作为高八位补零。
fn addChecksumBytes(sum: &mut u64, bytes: &[u8]) {
    for pair in bytes.chunks(2) {
        let low = pair.get(1).copied().unwrap_or(0);
        *sum += u64::from(u16::from_be_bytes([pair[0], low]));
    }
}

/// 把完整 UDP 事务交给顺序 spool；缺少 sink 或容量不足属于持续性完整性故障。
fn appendUdpEvent(
    event: crate::UdpDatagramEvent,
    context: &UdpObservationContext,
) -> Result<(), UdpObservationError> {
    let sink = context
        .udpDatagramSink
        .read()
        .expect("UDP 录制落点读锁中毒")
        .clone()
        .ok_or_else(|| UdpObservationError::Recording("顺序持久化落点未安装".to_owned()))?;
    sink.append(event).map_err(UdpObservationError::Recording)
}

/// 对首个未知 UDP 五元组执行最长 25ms 的 owner 表重查；等待只发生在独立 resolver，recv 持续批收。
fn observeUdpPacketWithAssociationWait(
    observation: &PendingUdpObservation,
    context: &UdpObservationContext,
    ownerSnapshot: &mut OwnerSnapshotCache,
) -> Result<PacketDirection, UdpObservationError> {
    let parsed = parseObservedUdpPacket(&observation.packet.data)
        .map_err(|error| UdpObservationError::Packet(error.to_string()))?;
    let Some(parsed) = parsed else {
        return Ok(PacketDirection::Bypass);
    };
    observeUdpTupleWithAssociationWait(&parsed, observation, context, ownerSnapshot)
}

/// 对普通数据报和分片首片共享的五元组执行 owner 关联等待；枚举瞬态错误不终止 resolver。
fn observeUdpTupleWithAssociationWait(
    parsed: &ObservedUdpPacket,
    observation: &PendingUdpObservation,
    context: &UdpObservationContext,
    ownerSnapshot: &mut OwnerSnapshotCache,
) -> Result<PacketDirection, UdpObservationError> {
    if let Some(direction) = matchedUdpDirection(parsed, observation, &context.flowTable) {
        return Ok(direction);
    }
    let primary = if observation.outbound {
        UdpAssociationCandidate {
            localAddress: parsed.sourceAddress,
            localPort: parsed.sourcePort,
            remoteAddress: parsed.destinationAddress,
            remotePort: parsed.destinationPort,
            direction: UdpDatagramDirection::Up,
        }
    } else {
        UdpAssociationCandidate {
            localAddress: parsed.destinationAddress,
            localPort: parsed.destinationPort,
            remoteAddress: parsed.sourceAddress,
            remotePort: parsed.sourcePort,
            direction: UdpDatagramDirection::Down,
        }
    };
    let reverse = UdpAssociationCandidate {
        localAddress: primary.remoteAddress,
        localPort: primary.remotePort,
        remoteAddress: primary.localAddress,
        remotePort: primary.localPort,
        direction: match primary.direction {
            UdpDatagramDirection::Up => UdpDatagramDirection::Down,
            UdpDatagramDirection::Down => UdpDatagramDirection::Up,
        },
    };
    loop {
        ownerSnapshot.refreshUntil(observation.associationDeadline)?;
        let mut pending = false;
        for candidate in [primary, reverse] {
            match associateUdpCandidate(candidate, observation, context, ownerSnapshot) {
                CandidateAssociation::Matched(direction) => return Ok(direction),
                CandidateAssociation::Pending => pending = true,
                CandidateAssociation::Unselected => {}
            }
        }
        if !pending || Instant::now() >= observation.associationDeadline {
            return Ok(PacketDirection::Bypass);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

#[derive(Clone, Copy)]
struct UdpAssociationCandidate {
    localAddress: IpAddr,
    localPort: u16,
    remoteAddress: IpAddr,
    remotePort: u16,
    direction: UdpDatagramDirection,
}

enum CandidateAssociation {
    Matched(PacketDirection),
    Unselected,
    Pending,
}

/// 尝试用一个候选本地端点补建 UDP 五元组；选中校验、synthetic 写入与控制面更新共享锁。
///
/// 同机回复可能把服务端和客户端都标为 outbound，因此调用方必须先试系统主方向，再试反向，
/// 不能因为主方向属于未选服务端就提前旁路已选客户端的首个入站数据报。
fn associateUdpCandidate(
    candidate: UdpAssociationCandidate,
    observation: &PendingUdpObservation,
    context: &UdpObservationContext,
    ownerSnapshot: &OwnerSnapshotCache,
) -> CandidateAssociation {
    let matchingOwners = ownerSnapshot.ownersFor(candidate.localAddress, candidate.localPort);
    let Some(ownerProcessId) = matchingOwners.processId else {
        return CandidateAssociation::Pending;
    };
    if matchingOwners.ambiguous {
        return CandidateAssociation::Pending;
    }
    let _selectionUpdate = context
        .processSelectionUpdateLock
        .lock()
        .expect("进程选择更新锁中毒");
    let ownerSelected = context
        .selectedProcessIds
        .read()
        .expect("选中进程集合读锁中毒")
        .contains(&ownerProcessId);
    if !ownerSelected {
        return CandidateAssociation::Unselected;
    }
    ensureOwnerUdpBindings(
        &context.flowTable,
        &context.ownerUdpEndpoints,
        &context.nextOwnerUdpEndpointId,
        ownerSnapshot.bindingsForProcess(ownerProcessId),
    );
    let _ = context
        .flowTable
        .associateUdpOutboundAt(UdpAssociationRequest {
            localAddress: candidate.localAddress,
            localPort: candidate.localPort,
            remoteAddress: candidate.remoteAddress,
            remotePort: candidate.remotePort,
            configuredProxyAddress: context.proxyAddress,
            proxyPort: context.proxyPort,
            capturedAtCounter: Some(observation.capturedAtCounter),
        });
    let Some(target) = context.flowTable.udpTargetAt(
        candidate.localAddress,
        candidate.localPort,
        candidate.remoteAddress,
        candidate.remotePort,
        observation.capturedAt,
        observation.capturedAtCounter,
    ) else {
        return CandidateAssociation::Pending;
    };
    CandidateAssociation::Matched(match candidate.direction {
        UdpDatagramDirection::Up => PacketDirection::ObservedUp(target.original),
        UdpDatagramDirection::Down => PacketDirection::ObservedDown(target.original),
    })
}

/// 按选中端点五元组判断真实业务方向；同机 LAN/回环回复在 WinDivert 中仍可能标为 outbound，
/// 因此先查系统方向对应键，再查反向键，禁止把本机服务端回复误当成未选进程的新出站流。
fn matchedUdpDirection(
    parsed: &ObservedUdpPacket,
    observation: &PendingUdpObservation,
    flowTable: &CaptureFlowTable,
) -> Option<PacketDirection> {
    let (primaryLocal, primaryRemote) = if observation.outbound {
        (
            (parsed.sourceAddress, parsed.sourcePort),
            (parsed.destinationAddress, parsed.destinationPort),
        )
    } else {
        (
            (parsed.destinationAddress, parsed.destinationPort),
            (parsed.sourceAddress, parsed.sourcePort),
        )
    };
    if let Some(target) = flowTable.udpTargetAt(
        primaryLocal.0,
        primaryLocal.1,
        primaryRemote.0,
        primaryRemote.1,
        observation.capturedAt,
        observation.capturedAtCounter,
    ) {
        return Some(if observation.outbound {
            PacketDirection::ObservedUp(target.original)
        } else {
            PacketDirection::ObservedDown(target.original)
        });
    }
    flowTable
        .udpTargetAt(
            primaryRemote.0,
            primaryRemote.1,
            primaryLocal.0,
            primaryLocal.1,
            observation.capturedAt,
            observation.capturedAtCounter,
        )
        .map(|target| {
            if observation.outbound {
                PacketDirection::ObservedDown(target.original)
            } else {
                PacketDirection::ObservedUp(target.original)
            }
        })
}

/// 在当前副本固定关联截止时间内重试 owner 表尺寸增长；只有到期仍失败才升级为完整性故障。
fn enumerateOwnedUdpBindingsUntil(
    deadline: Instant,
) -> Result<Vec<OwnedUdpBinding>, UdpObservationError> {
    loop {
        match enumerateAllOwnedUdpBindings() {
            Ok(bindings) => return Ok(bindings),
            Err(_error) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(UdpObservationError::Owner(error.to_string())),
        }
    }
}

/// 返回系统当前毫秒时间；时钟早于 UNIX 纪元时使用零，避免诊断时钟异常中断网络转发。
fn currentTimeMilliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
