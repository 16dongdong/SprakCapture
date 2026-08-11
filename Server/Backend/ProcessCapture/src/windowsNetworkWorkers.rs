//! 承载 NETWORK TCP 收包、关联等待、重写和唯一回注线程。
//!
//! WinDivert recv 线程只分类并进入有界队列；resolver 串行执行发送，避免并发回注重排 TCP 包。

use super::*;

/// 启动纯 recv/decision producer 和唯一 send resolver；任一 spawn 失败由外层 guard 回滚已启动线程。
pub(super) fn spawnNetworkWorkers(
    context: NetworkWorkerContext,
    workers: &mut WorkerSet,
) -> Result<(), ProcessCaptureError> {
    let queues = Arc::new(Mutex::new(ResolverQueues::default()));
    let readyCount = Arc::new(AtomicUsize::new(0));
    let pendingCount = Arc::new(AtomicUsize::new(0));
    let receiverDone = Arc::new(AtomicBool::new(false));
    let (resolverWake, wakeReceiver) = sync_channel::<()>(1);
    workers.resolverWake = Some(resolverWake.clone());
    workers.networkReceiverDone = Some(Arc::clone(&receiverDone));

    let resolverContext = context.clone();
    let resolverQueues = Arc::clone(&queues);
    let resolverReceiverDone = Arc::clone(&receiverDone);
    let resolverReadyCount = Arc::clone(&readyCount);
    let resolverPendingCount = Arc::clone(&pendingCount);
    workers.resolverWorker = Some(
        thread::Builder::new()
            .name("process-capture-network-resolver".to_owned())
            .spawn(move || {
                runPendingResolver(
                    resolverContext,
                    resolverQueues,
                    wakeReceiver,
                    resolverReceiverDone,
                    resolverReadyCount,
                    resolverPendingCount,
                );
            })
            .map_err(|error| ProcessCaptureError::Worker {
                worker: "NETWORK 解析线程创建",
                detail: error.to_string(),
            })?,
    );

    let receiverContext = context.clone();
    let receiverQueues = Arc::clone(&queues);
    let receiverWake = resolverWake.clone();
    let receiverCompletion = Arc::clone(&receiverDone);
    let receiverReadyCount = Arc::clone(&readyCount);
    let receiverPendingCount = Arc::clone(&pendingCount);
    let receiver = thread::Builder::new()
        .name("process-capture-network-recv".to_owned())
        .spawn(move || {
            let mut buffer = vec![0u8; maximumPacketBytes];
            loop {
                if receiverContext.stopRequested.load(Ordering::Acquire) {
                    break;
                }
                let packet = match receiverContext.divert.recv(&mut buffer) {
                    Ok(packet) => packet.into_owned(),
                    Err(error) => {
                        if receiverContext.stopRequested.load(Ordering::Acquire) {
                            break;
                        }
                        recordWorkerError(
                            &receiverContext.lastError,
                            "NETWORK 接收",
                            error.to_string(),
                        );
                        receiverContext.stopRequested.store(true, Ordering::Release);
                        let _ = receiverWake.try_send(());
                        break;
                    }
                };
                let capturedAt = Instant::now();
                let mut packet = packet;
                let (pendingCandidate, readyPacket) = match receiverContext
                    .connectionResetState
                    .takeIpv6ResetPackets(packet.data.as_ref(), packet.address.outbound())
                {
                    Some(packets) => (
                        false,
                        ReadyPacket::Ipv6Reset {
                            packets,
                            address: packet.address.clone(),
                        },
                    ),
                    None => {
                        let direction = classifyForwardPacket(&receiverContext, &mut packet);
                        let pendingCandidate = shouldKeepPending(
                            &direction,
                            packet.data.as_ref(),
                            packet.address.outbound(),
                            associationDeadline(capturedAt),
                            capturedAt,
                        );
                        (pendingCandidate, ReadyPacket::Forward { packet, direction })
                    }
                };
                let mut queues = receiverQueues.lock().expect("解析队列锁中毒");
                match selectQueue(
                    pendingCandidate,
                    receiverReadyCount.load(Ordering::Acquire),
                    receiverPendingCount.load(Ordering::Acquire),
                ) {
                    QueueSelection::Pending => {
                        receiverPendingCount.fetch_add(1, Ordering::AcqRel);
                        let ReadyPacket::Forward { packet, .. } = readyPacket else {
                            unreachable!("IPv6 RST 不进入 SYN 等待队列")
                        };
                        queues.pending.push_back(PendingPacket {
                            packet,
                            deadline: associationDeadline(capturedAt),
                        });
                    }
                    QueueSelection::Ready => {
                        receiverReadyCount.fetch_add(1, Ordering::AcqRel);
                        queues.ready.push_back(readyPacket);
                    }
                    QueueSelection::OverflowPending => {
                        // emergency 槽不属于普通容量，保留导致 pending 溢出的当前 SYN 供 resolver 原样恢复。
                        debug_assert!(queues.emergency.is_none());
                        let ReadyPacket::Forward { packet, .. } = readyPacket else {
                            unreachable!("IPv6 RST 不进入 SYN 溢出队列")
                        };
                        queues.emergency = Some(EmergencyPacket::Pending(PendingPacket {
                            packet,
                            deadline: associationDeadline(capturedAt),
                        }));
                        drop(queues);
                        recordWorkerError(
                            &receiverContext.lastError,
                            "NETWORK pending 队列",
                            format!("已达有界上限 {maximumPendingPackets}"),
                        );
                        receiverContext.stopRequested.store(true, Ordering::Release);
                        let _ = receiverWake.try_send(());
                        break;
                    }
                    QueueSelection::OverflowReady => {
                        // 已决策包必须保留 direction，resolver 在故障排空中按原决策完成而不旁路。
                        debug_assert!(queues.emergency.is_none());
                        queues.emergency = Some(EmergencyPacket::Ready(readyPacket));
                        drop(queues);
                        recordWorkerError(
                            &receiverContext.lastError,
                            "NETWORK ready 队列",
                            format!("已达有界上限 {maximumReadyPackets}"),
                        );
                        receiverContext.stopRequested.store(true, Ordering::Release);
                        let _ = receiverWake.try_send(());
                        break;
                    }
                }
                drop(queues);
                let _ = receiverWake.try_send(());
            }
            // release 发布确保 resolver 观察到完成时，producer 已不会再向 incoming 入队。
            receiverCompletion.store(true, Ordering::Release);
            let _ = receiverWake.try_send(());
        })
        .map_err(|error| ProcessCaptureError::Worker {
            worker: "NETWORK 收包线程创建",
            detail: error.to_string(),
        })?;
    workers.producerWorkers.push(receiver);
    Ok(())
}

/// 从 NETWORK 捕获时刻生成绝对截止时间；队列积压不得为 SYN 额外延长关联窗口。
pub(super) fn associationDeadline(capturedAt: Instant) -> Instant {
    capturedAt + Duration::from_millis(associationWaitMilliseconds)
}

/// 优先注入已决策包，再重查待关联 SYN；所有 WinDivertSend 只能在本线程执行。
fn runPendingResolver(
    context: NetworkWorkerContext,
    queues: Arc<Mutex<ResolverQueues>>,
    wakeReceiver: Receiver<()>,
    receiverDone: Arc<AtomicBool>,
    readyCount: Arc<AtomicUsize>,
    pendingCount: Arc<AtomicUsize>,
) {
    let mut proxyInterfaces = HashMap::<IpAddr, u32>::new();
    loop {
        let (mut ready, mut pending, emergency) = {
            let mut queues = queues.lock().expect("解析出队锁中毒");
            (
                std::mem::take(&mut queues.ready),
                std::mem::take(&mut queues.pending),
                queues.emergency.take(),
            )
        };
        while let Some(readyPacket) = ready.pop_front() {
            dispatchReadyPacket(&context, readyPacket, &mut proxyInterfaces);
            readyCount.fetch_sub(1, Ordering::AcqRel);
        }
        let pendingScanCount = pending.len();
        for _ in 0..pendingScanCount {
            let mut candidate = pending.pop_front().expect("待解析长度已固定");
            if context.stopRequested.load(Ordering::Acquire) {
                // 停止只强制关闭被选 PID 的 socket；未关联 SYN 可能属于任意系统进程，必须立即原样回注。
                sendOriginalPacket(&context, &candidate.packet, "NETWORK 停止恢复");
                pendingCount.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
            let direction = classifyForwardPacket(&context, &mut candidate.packet);
            if shouldKeepPending(
                &direction,
                candidate.packet.data.as_ref(),
                candidate.packet.address.outbound(),
                candidate.deadline,
                Instant::now(),
            ) {
                pending.push_back(candidate);
                continue;
            }
            dispatchNetworkPacket(&context, candidate.packet, direction, &mut proxyInterfaces);
            pendingCount.fetch_sub(1, Ordering::AcqRel);
        }
        if let Some(emergency) = emergency {
            match emergency {
                EmergencyPacket::Ready(readyPacket) => {
                    dispatchReadyPacket(&context, readyPacket, &mut proxyInterfaces)
                }
                EmergencyPacket::Pending(pendingPacket) => {
                    sendOriginalPacket(&context, &pendingPacket.packet, "NETWORK pending 容量恢复")
                }
            }
        }
        if !pending.is_empty() {
            queues
                .lock()
                .expect("待关联回队锁中毒")
                .pending
                .append(&mut pending);
        }
        let queuesEmpty = {
            let queues = queues.lock().expect("解析停止锁中毒");
            queues.ready.is_empty() && queues.pending.is_empty() && queues.emergency.is_none()
        };
        if resolverCanExit(
            context.stopRequested.load(Ordering::Acquire),
            receiverDone.load(Ordering::Acquire),
            queuesEmpty,
        ) {
            break;
        }
        let nextDeadline = queues
            .lock()
            .expect("待关联截止时间锁中毒")
            .pending
            .iter()
            .map(|entry| entry.deadline)
            .min();
        let waitResult = match nextDeadline {
            Some(deadline) => {
                wakeReceiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            }
            None => wakeReceiver
                .recv()
                .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
        };
        if matches!(
            waitResult,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ) {
            context.stopRequested.store(true, Ordering::Release);
        }
    }
}

/// 分派已决策动作；普通包按既有改写路径注入，IPv6 重置必须连续发送两个方向且逐次校验结果。
///
/// 运行上下文仅限 resolver 线程，保证同一 WinDivert 句柄的发送顺序稳定。任一校验和或发送
/// 失败都会保留精确四元组等待下一份 ACK 重试，绝不把辅助重置失败扩大为数据面故障。
fn dispatchReadyPacket(
    context: &NetworkWorkerContext,
    readyPacket: ReadyPacket,
    proxyInterfaces: &mut HashMap<IpAddr, u32>,
) {
    match readyPacket {
        ReadyPacket::Forward { packet, direction } => {
            dispatchNetworkPacket(context, packet, direction, proxyInterfaces)
        }
        ReadyPacket::Ipv6Reset { packets, address } => {
            sendIpv6ResetPackets(context, packets, address)
        }
    }
}

/// 使用捕获报文的接口元数据发送双向 IPv6 RST；反向包只翻转方向，不猜测机器相关接口编号。
///
/// `packets` 已携带真实 seq/ack 推导值，调用方提供的 `address` 来自同一条被截获连接。
/// 失败时仍尝试另一方向，以最大化端点回收并保留首个精确错误；单条辅助 RST 失败不得
/// 终止整个 NETWORK 数据面，否则一次旧连接清理异常会使后续全部新连接静默漏捕。
fn sendIpv6ResetPackets(
    context: &NetworkWorkerContext,
    packets: Ipv6ResetPackets,
    address: windivert::address::WinDivertAddress<NetworkLayer>,
) {
    let mut forward = unsafe { WinDivertPacket::<NetworkLayer>::new(packets.forward.clone()) };
    forward.address = address.clone();
    let mut reverse = unsafe { WinDivertPacket::<NetworkLayer>::new(packets.reverse.clone()) };
    reverse.address = address;
    reverse.address.set_outbound(!forward.address.outbound());

    let mut firstFailure = None;
    for (direction, packet) in [("正向", &mut forward), ("反向", &mut reverse)] {
        if let Err(error) = packet.recalculate_checksums(ChecksumFlags::default()) {
            firstFailure.get_or_insert_with(|| format!("{direction} RST 校验和：{error}"));
            continue;
        }
        if let Err(error) = context.divert.send(packet) {
            firstFailure.get_or_insert_with(|| format!("{direction} RST 注入：{error}"));
        }
    }
    if let Some(detail) = firstFailure {
        // 当前原包已被数据面接管，注入失败后必须恢复精确四元组等待下一份重传；
        // 直接丢弃重试状态会让旧连接继续直连，并再次表现为已选进程漏捕。
        packets.restoreAfterFailure(&context.connectionResetState);
        // RST 是清理既有直连连接的辅助动作，失败后已有下一包重试路径；写入 lastError
        // 会把仍在正常收发的 WinDivert 数据面永久标成停止，因此这里只输出可检索诊断。
        eprintln!("NETWORK IPv6 连接重置待重试：{detail}");
    }
}

/// 判定 resolver 是否可进入最终空队检查；必须先观察到 producer 发布完成。
pub(super) fn resolverCanExit(stopRequested: bool, receiverDone: bool, pendingEmpty: bool) -> bool {
    stopRequested && receiverDone && pendingEmpty
}

/// 对 TCP 包执行地址反射；UDP 由独立主动句柄完成统一规则决策和唯一回注，不进入 TCP 改写队列。
fn classifyForwardPacket(
    context: &NetworkWorkerContext,
    packet: &mut WinDivertPacket<'static, NetworkLayer>,
) -> Result<PacketDirection, crate::PacketRewriteError> {
    rewriteTcpPacket(
        packet.data.to_mut(),
        packet.address.outbound(),
        context.proxyPort,
        &context.flowTable,
    )
}

/// 判断未命中的 TCP SYN 是否仍应等待 PID 五元组关联。
/// UDP 的 FLOW ESTABLISHED 在首个数据报前提供权威归属；把所有未知 UDP 暂停 25ms 会拖慢整机 DNS/QUIC，
/// 因此 UDP 未命中必须立即原样回注，绝不以捕获完整性为由影响未选进程。
pub(super) fn shouldKeepPending(
    direction: &Result<PacketDirection, crate::PacketRewriteError>,
    packet: &[u8],
    outbound: bool,
    deadline: Instant,
    now: Instant,
) -> bool {
    matches!(direction, Ok(PacketDirection::Bypass))
        && outbound
        && isTcpStartPacket(packet)
        && now < deadline
}

/// 标记成功注入包应累计到哪个透明捕获方向；旁路包不进入进程流量指标。
enum CapturedTrafficDirection {
    Up,
    Down,
}

/// 应用 `direction` 给出的既定改写结果并注入；成功注入后按真实包长累计上下行流量。
/// Blocked 和缺少恢复接口的包按设计丢弃；校验和或注入失败记录工作线程根因并终止当前捕获代际。
fn dispatchNetworkPacket(
    context: &NetworkWorkerContext,
    mut packet: WinDivertPacket<'static, NetworkLayer>,
    direction: Result<PacketDirection, crate::PacketRewriteError>,
    proxyInterfaces: &mut HashMap<IpAddr, u32>,
) {
    let trafficDirection = match direction {
        Ok(PacketDirection::Redirected {
            proxyAddress,
            reflectedPort,
            ..
        }) => {
            let proxyInterfaceIndex = match proxyInterfaces.get(&proxyAddress).copied() {
                Some(interfaceIndex) => interfaceIndex,
                None => match bestInterfaceIndex(proxyAddress) {
                    Ok(interfaceIndex) => {
                        proxyInterfaces.insert(proxyAddress, interfaceIndex);
                        interfaceIndex
                    }
                    Err(detail) => {
                        recordWorkerError(&context.lastError, "NETWORK 回环接口", detail);
                        return;
                    }
                },
            };
            let originalInterface = NetworkInterface {
                interfaceIndex: packet.address.interface_index(),
                subinterfaceIndex: packet.address.subinterface_index(),
            };
            if !context.flowTable.setReflectedInterface(
                proxyAddress,
                reflectedPort,
                proxyAddress,
                originalInterface,
            ) {
                // SOCKET/FLOW 的关闭事件可能先于已捕获 NETWORK 包完成分发。
                // 此时反射索引消失代表连接已经结束，并非驱动故障；丢弃这一个陈旧包即可，
                // 若把正常生命周期竞态写入 lastError，会错误停止整个进程捕获服务。
                return;
            }
            prepareRedirectedAddress(&mut packet.address, proxyInterfaceIndex);
            if let Err(error) = packet.recalculate_checksums(ChecksumFlags::default()) {
                recordWorkerError(&context.lastError, "NETWORK 校验和", error.to_string());
            }
            Some(CapturedTrafficDirection::Up)
        }
        Ok(PacketDirection::Restored(_, Some(originalInterface))) => {
            packet.address.set_outbound(false);
            packet.address.set_loopback(false);
            packet
                .address
                .set_interface_index(originalInterface.interfaceIndex);
            packet
                .address
                .set_subinterface_index(originalInterface.subinterfaceIndex);
            if let Err(error) = packet.recalculate_checksums(ChecksumFlags::default()) {
                recordWorkerError(&context.lastError, "NETWORK 校验和", error.to_string());
            }
            Some(CapturedTrafficDirection::Down)
        }
        Ok(PacketDirection::Restored(_, None)) => {
            recordWorkerError(
                &context.lastError,
                "NETWORK 接口恢复",
                "代理回复缺少原始外部接口".to_owned(),
            );
            return;
        }
        // 首片命中后会登记短生命周期分片组；首片与所有同组后续片均在此丢弃。
        Ok(PacketDirection::Blocked(_)) => return,
        Ok(PacketDirection::ObservedUp(_))
        | Ok(PacketDirection::ObservedDown(_))
        | Ok(PacketDirection::Bypass)
        | Err(_) => None,
    };
    let packetBytes = packet.data.len() as u64;
    if let Err(error) = context.divert.send(&packet) {
        recordWorkerError(&context.lastError, "NETWORK 注入", error.to_string());
        context.stopRequested.store(true, Ordering::Release);
        return;
    }
    // 只有成功回注的数据包才能进入工作台指标；在发送前记账会把校验和或驱动注入失败伪装为有效流量。
    match trafficDirection {
        Some(CapturedTrafficDirection::Up) => {
            context.redirectedPackets.fetch_add(1, Ordering::Relaxed);
            context.bytesUp.fetch_add(packetBytes, Ordering::Relaxed);
        }
        Some(CapturedTrafficDirection::Down) => {
            context.restoredPackets.fetch_add(1, Ordering::Relaxed);
            context.bytesDown.fetch_add(packetBytes, Ordering::Relaxed);
        }
        None => {}
    }
}

/// 枚举选中进程已经绑定的 IPv4/IPv6 UDP 本地端点；热加入无需等待新的 SOCKET BIND 事件。
pub(super) fn enumerateOwnedUdpBindings(
    selectedProcessIds: &BTreeSet<u32>,
) -> Result<Vec<OwnedUdpBinding>, ProcessCaptureError> {
    let mut bindings = enumerateAllOwnedUdpBindings()?;
    bindings.retain(|binding| selectedProcessIds.contains(&binding.processId));
    Ok(bindings)
}

/// 返回系统当前全部双栈 UDP owner 绑定；调用方据此区分未选端点与尚未进入 owner 表的新端点。
pub(super) fn enumerateAllOwnedUdpBindings() -> Result<Vec<OwnedUdpBinding>, ProcessCaptureError> {
    let mut bindings = enumerateOwnedUdpBindingsForFamily::<MIB_UDPROW_OWNER_PID>(
        AF_INET as u32,
        "IPv4 UDP",
        |row| OwnedUdpBinding {
            processId: row.dwOwningPid,
            localAddress: IpAddr::V4(std::net::Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes())),
            localPort: u16::from_be(row.dwLocalPort as u16),
        },
    )?;
    bindings.extend(enumerateOwnedUdpBindingsForFamily::<MIB_UDP6ROW_OWNER_PID>(
        AF_INET6 as u32,
        "IPv6 UDP",
        |row| OwnedUdpBinding {
            processId: row.dwOwningPid,
            localAddress: IpAddr::V6(std::net::Ipv6Addr::from(row.ucLocalAddr)),
            localPort: u16::from_be(row.dwLocalPort as u16),
        },
    )?);
    bindings.retain(|binding| binding.localPort != 0 && !binding.localAddress.is_loopback());
    bindings.sort_unstable();
    bindings.dedup();
    Ok(bindings)
}

/// 以对齐缓冲区读取单一地址族的 owner PID 表；长度变化会重试一次而不解析半张表。
fn enumerateOwnedUdpBindingsForFamily<Row>(
    addressFamily: u32,
    familyName: &'static str,
    convert: impl Fn(&Row) -> OwnedUdpBinding,
) -> Result<Vec<OwnedUdpBinding>, ProcessCaptureError> {
    let mut byteLength = 0_u32;
    let probeStatus = unsafe {
        GetExtendedUdpTable(
            std::ptr::null_mut(),
            &mut byteLength,
            0,
            addressFamily,
            UDP_TABLE_OWNER_PID,
            0,
        )
    };
    if probeStatus != ERROR_INSUFFICIENT_BUFFER {
        if probeStatus == 0 && byteLength == 0 {
            return Ok(Vec::new());
        }
        return Err(ProcessCaptureError::Worker {
            worker: "UDP owner 表枚举",
            detail: format!("读取 {familyName} 所需长度失败，系统状态码：{probeStatus}"),
        });
    }
    for _ in 0..2 {
        let wordCount = usize::try_from(byteLength)
            .ok()
            .and_then(|bytes| bytes.checked_add(size_of::<usize>() - 1))
            .map(|bytes| bytes / size_of::<usize>())
            .ok_or_else(|| ProcessCaptureError::Worker {
                worker: "UDP owner 表枚举",
                detail: format!("{familyName} owner 表长度溢出"),
            })?;
        let mut storage = vec![0_usize; wordCount];
        let status = unsafe {
            GetExtendedUdpTable(
                storage.as_mut_ptr().cast(),
                &mut byteLength,
                0,
                addressFamily,
                UDP_TABLE_OWNER_PID,
                0,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != 0 {
            return Err(ProcessCaptureError::Worker {
                worker: "UDP owner 表枚举",
                detail: format!("读取 {familyName} 失败，系统状态码：{status}"),
            });
        }
        let bytesAvailable =
            usize::try_from(byteLength).map_err(|_| ProcessCaptureError::Worker {
                worker: "UDP owner 表枚举",
                detail: format!("{familyName} owner 表长度无法映射到平台 usize"),
            })?;
        if bytesAvailable < size_of::<u32>() {
            return Err(ProcessCaptureError::Worker {
                worker: "UDP owner 表枚举",
                detail: format!("{familyName} owner 表缺少计数字段"),
            });
        }
        let rowCount = unsafe { *storage.as_ptr().cast::<u32>() } as usize;
        let requiredBytes = size_of::<u32>()
            .checked_add(rowCount.checked_mul(size_of::<Row>()).ok_or_else(|| {
                ProcessCaptureError::Worker {
                    worker: "UDP owner 表枚举",
                    detail: format!("{familyName} owner 表行数溢出"),
                }
            })?)
            .ok_or_else(|| ProcessCaptureError::Worker {
                worker: "UDP owner 表枚举",
                detail: format!("{familyName} owner 表总长度溢出"),
            })?;
        if requiredBytes > bytesAvailable {
            return Err(ProcessCaptureError::Worker {
                worker: "UDP owner 表枚举",
                detail: format!("{familyName} owner 表行数超出缓冲区"),
            });
        }
        let rows = unsafe {
            std::slice::from_raw_parts(
                storage
                    .as_ptr()
                    .cast::<u8>()
                    .add(size_of::<u32>())
                    .cast::<Row>(),
                rowCount,
            )
        };
        return Ok(rows.iter().map(convert).collect());
    }
    Err(ProcessCaptureError::Worker {
        worker: "UDP owner 表枚举",
        detail: format!("{familyName} owner 表在重试后仍持续增长"),
    })
}
