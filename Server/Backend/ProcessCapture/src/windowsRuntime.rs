use std::os::windows::io::AsRawHandle;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    mem::size_of,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use windivert::prelude::{
    NetworkLayer, ShutdownHandle, WinDivert, WinDivertError, WinDivertEvent, WinDivertFlags,
    WinDivertPacket, WinDivertParam,
};
use windivert_sys::{
    ChecksumFlags, WINDIVERT_BATCH_MAX, WINDIVERT_MTU_MAX, WINDIVERT_PARAM_QUEUE_LENGTH_MAX,
    WINDIVERT_PARAM_QUEUE_SIZE_MAX, WINDIVERT_PARAM_QUEUE_TIME_MAX,
};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NOT_FOUND, GetLastError, HANDLE},
    NetworkManagement::IpHelper::{
        GetBestInterfaceEx, GetExtendedUdpTable, MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID,
        UDP_TABLE_OWNER_PID,
    },
    Networking::WinSock::{
        AF_INET, AF_INET6, IN_ADDR, IN_ADDR_0, IN6_ADDR, IN6_ADDR_0, SOCKADDR, SOCKADDR_IN,
        SOCKADDR_IN6, SOCKADDR_IN6_0,
    },
    System::IO::CancelSynchronousIo,
};

use crate::connectionReset::{
    ConnectionResetState, Ipv6ResetPackets, resetExistingConnections, resetIpv4Connections,
};
#[path = "windowsEventWorkers.rs"]
mod windowsEventWorkers;

use windowsEventWorkers::{spawnFlowWorker, spawnSocketWorker};

#[path = "windowsNetworkWorkers.rs"]
mod windowsNetworkWorkers;

#[path = "windowsUdpObservation.rs"]
mod windowsUdpObservation;

#[cfg(test)]
use windowsNetworkWorkers::{associationDeadline, resolverCanExit, shouldKeepPending};
use windowsNetworkWorkers::{
    enumerateAllOwnedUdpBindings, enumerateOwnedUdpBindings, spawnNetworkWorkers,
};
use windowsUdpObservation::spawnUdpObservationWorkers;

use crate::flowTable::UdpAssociationRequest;
use crate::{
    CaptureFlow, CaptureFlowTable, NetworkInterface, OriginalTarget, PacketDirection,
    ProcessCaptureConfiguration, ProcessCaptureError, ProcessCaptureSnapshot,
    SharedUdpDatagramProcessor, SharedUdpDatagramSink, UdpDatagramDecision, UdpDatagramDirection,
    isTcpStartPacket,
    packetRewrite::{ObservedUdpPacket, parseObservedUdpPacket, udpDatagramEvent},
    rewriteTcpPacket,
    udpFragment::{
        UdpFragmentAssembler, UdpFragmentDisposition, UdpPacketFragment, firstFragmentTuple,
        fragmentPayloadPrefix, fragmentPayloadRange, fragmentUdpHeaderOffset, inspectUdpFragment,
    },
};

// 使用驱动公开最大包常量，避免合法 IPv6 包触发 `ERROR_INSUFFICIENT_BUFFER` 并终止收包线程。
const maximumPacketBytes: usize = WINDIVERT_MTU_MAX as usize;
const udpReceiveBatchPackets: u8 = WINDIVERT_BATCH_MAX as u8;
const tcpNetworkPriority: i16 = 120;
const udpObservationPriority: i16 = 119;
const eventPriority: i16 = 121;
const associationWaitMilliseconds: u64 = 25;
const maximumPendingPackets: usize = 1_024;
const maximumReadyPackets: usize = 4_096;
// 容量覆盖 QUIC/媒体的完整 4K 双向突发；owner 索引以 O(1) 速度排除未选流量。
const maximumUdpObservationPackets: usize = 8_192;
const ownerSnapshotLifetime: Duration = Duration::from_millis(10);
// HRESULT_FROM_WIN32(ERROR_INVALID_HANDLE)，用于识别已关闭 WinDivert 句柄的幂等停止结果。
const invalidHandleHresult: i32 = 0x8007_0006_u32 as i32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OwnedUdpBinding {
    processId: u32,
    localAddress: IpAddr,
    localPort: u16,
}

/// 保存一次捕获会话的分角色线程与句柄；停止时必须先回收 producer，再由 resolver 排空已捕获包。
struct CaptureRuntime {
    workers: WorkerSet,
}

/// 集中持有启动阶段可能部分构造的线程；同一停止程序同时服务启动回滚与正常 stop。
struct WorkerSet {
    stopRequested: Arc<AtomicBool>,
    shutdownHandles: Vec<(&'static str, windivert::ShutdownHandle)>,
    resolverWake: Option<SyncSender<()>>,
    networkReceiverDone: Option<Arc<AtomicBool>>,
    producerWorkers: Vec<JoinHandle<()>>,
    resolverWorker: Option<JoinHandle<()>>,
}

/// 启动失败时自动回收已创建线程；成功移交后不再触发回滚。
struct StartupGuard {
    workers: Option<WorkerSet>,
    flowTable: CaptureFlowTable,
}

/// 管理 SOCKET、FLOW、TCP 改写与 UDP 统一封包句柄的共同生命周期，并向透明代理暴露原目标查询。
pub struct ProcessCapture {
    flowTable: CaptureFlowTable,
    runtime: Mutex<Option<CaptureRuntime>>,
    configuration: Mutex<ProcessCaptureConfiguration>,
    selectedProcessIds: Arc<RwLock<BTreeSet<u32>>>,
    processSelectionUpdateLock: Arc<Mutex<()>>,
    pendingConnectionResets: Mutex<BTreeMap<u32, u64>>,
    nextConnectionResetGeneration: AtomicU64,
    ownerUdpEndpoints: Arc<Mutex<BTreeMap<OwnedUdpBinding, u64>>>,
    nextOwnerUdpEndpointId: Arc<AtomicU64>,
    connectionResetDiagnostic: Mutex<Option<String>>,
    connectionResetState: ConnectionResetState,
    acceptedConnections: Arc<AtomicU64>,
    redirectedPackets: Arc<AtomicU64>,
    restoredPackets: Arc<AtomicU64>,
    bytesUp: Arc<AtomicU64>,
    bytesDown: Arc<AtomicU64>,
    lastError: Arc<Mutex<Option<String>>>,
    udpDatagramSink: Arc<RwLock<Option<SharedUdpDatagramSink>>>,
    udpDatagramProcessor: Arc<RwLock<Option<SharedUdpDatagramProcessor>>>,
    nextUdpCaptureSequence: Arc<AtomicU64>,
}

impl Default for ProcessCapture {
    /// 创建关闭状态控制器；只有显式启用配置才会触发管理员驱动加载。
    fn default() -> Self {
        Self {
            flowTable: CaptureFlowTable::default(),
            runtime: Mutex::new(None),
            configuration: Mutex::new(ProcessCaptureConfiguration::default()),
            selectedProcessIds: Arc::new(RwLock::new(BTreeSet::new())),
            processSelectionUpdateLock: Arc::new(Mutex::new(())),
            pendingConnectionResets: Mutex::new(BTreeMap::new()),
            nextConnectionResetGeneration: AtomicU64::new(1),
            ownerUdpEndpoints: Arc::new(Mutex::new(BTreeMap::new())),
            nextOwnerUdpEndpointId: Arc::new(AtomicU64::new(1_u64 << 63)),
            connectionResetDiagnostic: Mutex::new(None),
            connectionResetState: ConnectionResetState::default(),
            acceptedConnections: Arc::new(AtomicU64::new(0)),
            redirectedPackets: Arc::new(AtomicU64::new(0)),
            restoredPackets: Arc::new(AtomicU64::new(0)),
            bytesUp: Arc::new(AtomicU64::new(0)),
            bytesDown: Arc::new(AtomicU64::new(0)),
            lastError: Arc::new(Mutex::new(None)),
            udpDatagramSink: Arc::new(RwLock::new(None)),
            udpDatagramProcessor: Arc::new(RwLock::new(None)),
            nextUdpCaptureSequence: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl ProcessCapture {
    /// 创建 WinDivert 捕获控制器，实际驱动句柄延迟到 `start` 打开。
    pub fn new() -> Self {
        Self::default()
    }

    /// 安装 UDP 数据报录制通道；`None` 停止发布事件，录制延迟不影响数据面或累计指标。
    pub fn setUdpDatagramSink(&self, sink: Option<SharedUdpDatagramSink>) {
        *self.udpDatagramSink.write().expect("UDP 录制落点写锁中毒") = sink;
    }

    /// 安装 Socket 写入前的统一封包处理器；热更新只替换共享快照，不重建 WinDivert 句柄。
    pub fn setUdpDatagramProcessor(&self, processor: Option<SharedUdpDatagramProcessor>) {
        *self
            .udpDatagramProcessor
            .write()
            .expect("UDP 封包处理器写锁中毒") = processor;
    }

    /// 切换捕获配置并重建选中进程的既有 TCP 会话；IPv4 同步删除，IPv6 在真实 ACK 上注入 RST。
    pub fn start(
        &self,
        configuration: ProcessCaptureConfiguration,
    ) -> Result<(), ProcessCaptureError> {
        configuration.validate(std::process::id())?;
        if !configuration.enabled {
            self.stop()?;
            *self.configuration.lock().expect("捕获配置锁中毒") = configuration;
            return Ok(());
        }

        // 配置切换先关闭旧句柄，杜绝两组同优先级 NETWORK 句柄重叠捕获同一数据包。
        self.stop()?;

        // PID 是会话证据，不能固化进 WinDivert 过滤式；事件层复制元数据后由工作线程读取热更新集合。
        let socketFilter = "(tcp or udp) and (event == BIND or event == CONNECT or event == CLOSE)";
        let socket = WinDivert::socket(
            socketFilter,
            eventPriority,
            WinDivertFlags::default().set_sniff(),
        )
        .map_err(|error| ProcessCaptureError::OpenDriver {
            layer: "SOCKET",
            detail: error.to_string(),
        })?;
        let flow = WinDivert::flow("tcp or udp", eventPriority, WinDivertFlags::default())
            .map_err(|error| ProcessCaptureError::OpenDriver {
                layer: "FLOW",
                detail: error.to_string(),
            })?;
        // TCP 与 UDP 分属独立句柄，但最终字节都在统一封包数据面得到一次写线决定；UDP resolver
        // 独占校验和重算和回注，录制只发生在回注成功之后，禁止界面正文领先于真实网络。
        // TCP 还需观察 IPv6 入站 ACK 来重置既有连接。流表拒绝真实 127/8 与 ::1 目标，只保留透明监听器
        // 回复包；Windows 会把本机 LAN 地址互访标成 loopback，因此过滤式必须按实际回环地址排除。
        let tcpNetworkFilter = format!(
            "!impostor and tcp and (((outbound or ipv6) and (!loopback or (ip and ip.SrcAddr != 127.0.0.1 and ip.DstAddr != 127.0.0.1) or (ipv6 and ipv6.SrcAddr != ::1 and ipv6.DstAddr != ::1))) or (loopback and tcp.SrcPort == {}))",
            configuration.proxyPort
        );
        let network = WinDivert::network(
            &tcpNetworkFilter,
            tcpNetworkPriority,
            WinDivertFlags::default(),
        )
        .map_err(|error| ProcessCaptureError::OpenDriver {
            layer: "NETWORK TCP",
            detail: error.to_string(),
        })?;
        // 非首片没有 UDP 头，必须显式纳入双栈 fragment 并由 resolver 有界重组后整组处理和回注。
        let udpObservationFilter = "!impostor and (udp or fragment) and (!loopback or (ip and ip.SrcAddr != 127.0.0.1 and ip.DstAddr != 127.0.0.1) or (ipv6 and ipv6.SrcAddr != ::1 and ipv6.DstAddr != ::1))";
        let udpObservation = WinDivert::network(
            udpObservationFilter,
            udpObservationPriority,
            WinDivertFlags::default(),
        )
        .map_err(|error| ProcessCaptureError::OpenDriver {
            layer: "NETWORK UDP 拦截",
            detail: error.to_string(),
        })?;
        // 驱动默认队列不足以承接媒体与 QUIC 突发，三项使用公开上限给纯复制线程完整吸收窗口。
        for (parameter, value) in [
            (
                WinDivertParam::QueueLength,
                WINDIVERT_PARAM_QUEUE_LENGTH_MAX,
            ),
            (WinDivertParam::QueueTime, WINDIVERT_PARAM_QUEUE_TIME_MAX),
            (WinDivertParam::QueueSize, WINDIVERT_PARAM_QUEUE_SIZE_MAX),
        ] {
            udpObservation
                .set_param(parameter, value)
                .map_err(|error| ProcessCaptureError::OpenDriver {
                    layer: "NETWORK UDP 拦截队列",
                    detail: error.to_string(),
                })?;
        }

        let ownerUdpBindings = enumerateOwnedUdpBindings(&configuration.processIds)?;
        self.flowTable.clear();
        // 流表清空时同步丢弃合成索引，避免 stop/start 后误判端点仍在而跳过既有 UDP socket 重建。
        self.ownerUdpEndpoints
            .lock()
            .expect("UDP owner 端点集合锁中毒")
            .clear();
        *self
            .selectedProcessIds
            .write()
            .expect("选中进程集合写锁中毒") = configuration.processIds.clone();
        self.installOwnerUdpBindings(&ownerUdpBindings);
        self.pendingConnectionResets
            .lock()
            .expect("待关闭连接集合锁中毒")
            .clear();
        *self
            .connectionResetDiagnostic
            .lock()
            .expect("连接重置诊断锁中毒") = None;
        self.connectionResetState.clear();
        self.acceptedConnections.store(0, Ordering::Relaxed);
        self.redirectedPackets.store(0, Ordering::Relaxed);
        self.restoredPackets.store(0, Ordering::Relaxed);
        self.bytesUp.store(0, Ordering::Relaxed);
        self.bytesDown.store(0, Ordering::Relaxed);
        *self.lastError.lock().expect("捕获错误锁中毒") = None;

        let stopRequested = Arc::new(AtomicBool::new(false));
        let shutdownHandles = vec![
            ("SOCKET", socket.shutdown_handle()),
            ("FLOW", flow.shutdown_handle()),
            ("NETWORK TCP", network.shutdown_handle()),
            ("NETWORK UDP 拦截", udpObservation.shutdown_handle()),
        ];
        let mut startup = StartupGuard {
            workers: Some(WorkerSet {
                stopRequested: Arc::clone(&stopRequested),
                shutdownHandles,
                resolverWake: None,
                networkReceiverDone: None,
                producerWorkers: Vec::with_capacity(4),
                resolverWorker: None,
            }),
            flowTable: self.flowTable.clone(),
        };
        let network = Arc::new(network);
        let networkContext = NetworkWorkerContext {
            divert: network,
            flowTable: self.flowTable.clone(),
            proxyPort: configuration.proxyPort,
            stopRequested: Arc::clone(&stopRequested),
            redirectedPackets: Arc::clone(&self.redirectedPackets),
            restoredPackets: Arc::clone(&self.restoredPackets),
            bytesUp: Arc::clone(&self.bytesUp),
            bytesDown: Arc::clone(&self.bytesDown),
            lastError: Arc::clone(&self.lastError),
            connectionResetState: self.connectionResetState.clone(),
        };
        spawnNetworkWorkers(
            networkContext,
            startup.workers.as_mut().expect("启动工作集已移交"),
        )?;
        startup
            .workers
            .as_mut()
            .expect("启动工作集已移交")
            .producerWorkers
            .extend(spawnUdpObservationWorkers(
                udpObservation,
                UdpObservationContext {
                    flowTable: self.flowTable.clone(),
                    proxyAddress: configuration.proxyAddress,
                    proxyPort: configuration.proxyPort,
                    stopRequested: Arc::clone(&stopRequested),
                    observedPacketsUp: Arc::clone(&self.redirectedPackets),
                    observedPacketsDown: Arc::clone(&self.restoredPackets),
                    bytesUp: Arc::clone(&self.bytesUp),
                    bytesDown: Arc::clone(&self.bytesDown),
                    lastError: Arc::clone(&self.lastError),
                    udpDatagramSink: Arc::clone(&self.udpDatagramSink),
                    udpDatagramProcessor: Arc::clone(&self.udpDatagramProcessor),
                    selectedProcessIds: Arc::clone(&self.selectedProcessIds),
                    ownerUdpEndpoints: Arc::clone(&self.ownerUdpEndpoints),
                    nextOwnerUdpEndpointId: Arc::clone(&self.nextOwnerUdpEndpointId),
                    nextUdpCaptureSequence: Arc::clone(&self.nextUdpCaptureSequence),
                    processSelectionUpdateLock: Arc::clone(&self.processSelectionUpdateLock),
                },
            )?);
        let resolverWake = startup
            .workers
            .as_ref()
            .and_then(|workers| workers.resolverWake.clone())
            .expect("网络解析器未安装唤醒通道");
        self.flowTable.setAssociationNotifier(Some(resolverWake));
        startup
            .workers
            .as_mut()
            .expect("启动工作集已移交")
            .producerWorkers
            .push(spawnSocketWorker(
                socket,
                SocketWorkerContext {
                    flowTable: self.flowTable.clone(),
                    proxyAddress: configuration.proxyAddress,
                    proxyPort: configuration.proxyPort,
                    stopRequested: Arc::clone(&stopRequested),
                    lastError: Arc::clone(&self.lastError),
                    selectedProcessIds: Arc::clone(&self.selectedProcessIds),
                    acceptedConnections: Arc::clone(&self.acceptedConnections),
                    ownerUdpEndpoints: Arc::clone(&self.ownerUdpEndpoints),
                    processSelectionUpdateLock: Arc::clone(&self.processSelectionUpdateLock),
                },
            )?);
        startup
            .workers
            .as_mut()
            .expect("启动工作集已移交")
            .producerWorkers
            .push(spawnFlowWorker(
                flow,
                SocketWorkerContext {
                    flowTable: self.flowTable.clone(),
                    proxyAddress: configuration.proxyAddress,
                    proxyPort: configuration.proxyPort,
                    stopRequested: Arc::clone(&stopRequested),
                    lastError: Arc::clone(&self.lastError),
                    selectedProcessIds: Arc::clone(&self.selectedProcessIds),
                    acceptedConnections: Arc::clone(&self.acceptedConnections),
                    ownerUdpEndpoints: Arc::clone(&self.ownerUdpEndpoints),
                    processSelectionUpdateLock: Arc::clone(&self.processSelectionUpdateLock),
                },
            )?);
        let selectedProcessIds = configuration.processIds.clone();
        *self.configuration.lock().expect("捕获配置锁中毒") = configuration;
        *self.runtime.lock().expect("捕获运行锁中毒") = Some(CaptureRuntime {
            workers: startup.take(),
        });
        // 既有 TCP 无可重放握手状态；驱动就绪后断开，使客户端从新握手建立确定代理语义。
        if let Err(resetError) =
            resetExistingConnections(&selectedProcessIds, &self.connectionResetState)
        {
            let resetGeneration = self
                .nextConnectionResetGeneration
                .fetch_add(1, Ordering::Relaxed);
            self.pendingConnectionResets
                .lock()
                .expect("待关闭连接集合锁中毒")
                .extend(
                    selectedProcessIds
                        .into_iter()
                        .map(|processId| (processId, resetGeneration)),
                );
            recordConnectionResetDiagnostic(&self.connectionResetDiagnostic, &resetError);
        } else {
            clearConnectionResetDiagnostic(&self.connectionResetDiagnostic);
        }
        Ok(())
    }

    /// 请求线程停止并等待所有 WinDivert 句柄关闭；关闭句柄后新流量自动恢复系统直连。
    pub fn stop(&self) -> Result<(), ProcessCaptureError> {
        let selectedProcessIds = self
            .selectedProcessIds
            .read()
            .expect("选中进程集合读锁中毒")
            .clone();
        let runtime = self.runtime.lock().expect("捕获运行锁中毒").take();
        let Some(mut runtime) = runtime else {
            self.flowTable.clear();
            self.ownerUdpEndpoints
                .lock()
                .expect("UDP owner 端点集合锁中毒")
                .clear();
            self.selectedProcessIds
                .write()
                .expect("选中进程集合写锁中毒")
                .clear();
            self.pendingConnectionResets
                .lock()
                .expect("待关闭连接集合锁中毒")
                .clear();
            self.connectionResetState.clear();
            clearConnectionResetDiagnostic(&self.connectionResetDiagnostic);
            return Ok(());
        };
        // IPv4 TCB 可同步删除；IPv6 由中止代理关闭，辅助清理失败只写诊断而不阻碍强制停止。
        if let Err(error) = resetIpv4Connections(&selectedProcessIds) {
            recordConnectionResetDiagnostic(&self.connectionResetDiagnostic, &error);
        }
        self.flowTable.setAssociationNotifier(None);
        let (shutdownError, workerPanicked) = stopWorkerSet(&mut runtime.workers);
        self.flowTable.clear();
        self.ownerUdpEndpoints
            .lock()
            .expect("UDP owner 端点集合锁中毒")
            .clear();
        self.selectedProcessIds
            .write()
            .expect("选中进程集合写锁中毒")
            .clear();
        self.pendingConnectionResets
            .lock()
            .expect("待关闭连接集合锁中毒")
            .clear();
        self.connectionResetState.clear();
        if let Some(detail) = shutdownError {
            return Err(ProcessCaptureError::Worker {
                worker: "WinDivert 停止",
                detail,
            });
        }
        if workerPanicked {
            return Err(ProcessCaptureError::WorkerPanicked);
        }
        Ok(())
    }

    /// 原子替换目标 PID，不重建 WinDivert；非法输入不改状态，旧连接清理失败保留待重试诊断。
    pub fn updateProcessIds(&self, processIds: BTreeSet<u32>) -> Result<(), ProcessCaptureError> {
        // owner 快照、目标发布和端点 diff 必须串行，避免旧快照覆盖新状态或重开 BIND 窗口。
        let selectionUpdate = self
            .processSelectionUpdateLock
            .lock()
            .expect("进程选择更新锁中毒");
        if processIds.contains(&0) {
            return Err(ProcessCaptureError::InvalidProcessId);
        }
        if processIds.contains(&std::process::id()) {
            return Err(ProcessCaptureError::ProxyProcessSelected(std::process::id()));
        }
        if self.runtime.lock().expect("捕获运行锁中毒").is_none() {
            return Err(ProcessCaptureError::NotRunning);
        }
        let previousProcessIds = self
            .selectedProcessIds
            .read()
            .expect("选中进程集合读锁中毒")
            .clone();
        // 新 PID 先进入事件集合再读 owner 表；枚举失败回滚集合与动态绑定。
        *self
            .selectedProcessIds
            .write()
            .expect("选中进程集合写锁中毒") = processIds.clone();
        let ownerUdpBindings = match enumerateOwnedUdpBindings(&processIds) {
            Ok(bindings) => bindings,
            Err(error) => {
                *self
                    .selectedProcessIds
                    .write()
                    .expect("选中进程集合回滚锁中毒") = previousProcessIds.clone();
                self.flowTable.retainProcessIds(&previousProcessIds);
                // owner 枚举失败必须回滚 bootstrap 窗口补建的端点，避免残留映射被误判为已安装。
                self.ownerUdpEndpoints
                    .lock()
                    .expect("UDP owner 端点回滚锁中毒")
                    .retain(|binding, _| previousProcessIds.contains(&binding.processId));
                return Err(error);
            }
        };
        let changedProcessIds = previousProcessIds
            .symmetric_difference(&processIds)
            .copied()
            .collect::<BTreeSet<_>>();
        self.flowTable.retainProcessIds(&processIds);
        self.installOwnerUdpBindings(&ownerUdpBindings);
        self.configuration
            .lock()
            .expect("捕获配置锁中毒")
            .processIds = processIds;
        let resetGeneration = self
            .nextConnectionResetGeneration
            .fetch_add(1, Ordering::Relaxed);
        let resetSnapshot = {
            let mut pending = self
                .pendingConnectionResets
                .lock()
                .expect("待关闭连接集合锁中毒");
            pending.extend(
                changedProcessIds
                    .into_iter()
                    .map(|processId| (processId, resetGeneration)),
            );
            pending.clone()
        };
        if resetSnapshot.is_empty() {
            clearConnectionResetDiagnostic(&self.connectionResetDiagnostic);
            return Ok(());
        }
        // selected/owner/flow 已原子发布后立即释放选择锁；慢速 TCB 枚举不得阻塞 UDP resolver。
        // generation 快照保证并发热更新重新加入的同一 PID 不会被旧调用的成功结果误删。
        drop(selectionUpdate);
        let resetProcessIds = resetSnapshot.keys().copied().collect::<BTreeSet<_>>();
        match resetExistingConnections(&resetProcessIds, &self.connectionResetState) {
            Ok(_) => {
                let pendingEmpty = {
                    let mut pending = self
                        .pendingConnectionResets
                        .lock()
                        .expect("待关闭连接集合锁中毒");
                    pending.retain(|processId, generation| {
                        resetSnapshot.get(processId) != Some(generation)
                    });
                    pending.is_empty()
                };
                if pendingEmpty {
                    clearConnectionResetDiagnostic(&self.connectionResetDiagnostic);
                }
                Ok(())
            }
            Err(error) => {
                recordConnectionResetDiagnostic(&self.connectionResetDiagnostic, &error);
                Ok(())
            }
        }
    }

    /// 原子替换 IP Helper 预注册的 UDP 本地端点；SOCKET/FLOW 后续事件仍可补强远端五元组。
    fn installOwnerUdpBindings(&self, bindings: &[OwnedUdpBinding]) {
        installOwnerUdpBindings(
            &self.flowTable,
            &self.ownerUdpEndpoints,
            &self.nextOwnerUdpEndpointId,
            bindings,
        );
    }

    /// 生成不含连接内容的运行快照；进程编号排序来自 `BTreeSet`，结果稳定可测试。
    pub fn snapshot(&self) -> ProcessCaptureSnapshot {
        let configuration = self.configuration.lock().expect("捕获配置锁中毒").clone();
        let lastError = self
            .lastError
            .lock()
            .expect("捕获错误锁中毒")
            .clone()
            .or_else(|| {
                self.udpDatagramSink
                    .read()
                    .expect("UDP 录制落点读锁中毒")
                    .as_ref()
                    .and_then(|sink| sink.fault())
            });
        ProcessCaptureSnapshot {
            running: lastError.is_none() && self.runtime.lock().expect("捕获运行锁中毒").is_some(),
            configuredProcessIds: configuration.processIds.into_iter().collect(),
            trackedFlows: self.flowTable.len(),
            acceptedConnections: self.acceptedConnections.load(Ordering::Relaxed),
            redirectedPackets: self.redirectedPackets.load(Ordering::Relaxed),
            restoredPackets: self.restoredPackets.load(Ordering::Relaxed),
            bytesUp: self.bytesUp.load(Ordering::Relaxed),
            bytesDown: self.bytesDown.load(Ordering::Relaxed),
            lastError,
        }
    }

    /// 由透明监听器查询反射连接原目标，避免从 TLS 或应用层载荷猜测目的地址。
    pub fn originalTargetForPeer(
        &self,
        localAddress: IpAddr,
        peer: SocketAddr,
    ) -> Option<OriginalTarget> {
        self.flowTable.originalTargetForPeer(localAddress, peer)
    }
}

/// 差量安装双栈 owner 表绑定；共享注册表使控制热更新与 UDP resolver 竞态补查复用同一原子路径。
fn installOwnerUdpBindings(
    flowTable: &CaptureFlowTable,
    ownerUdpEndpoints: &Mutex<BTreeMap<OwnedUdpBinding, u64>>,
    nextOwnerUdpEndpointId: &AtomicU64,
    bindings: &[OwnedUdpBinding],
) {
    let mut endpoints = ownerUdpEndpoints.lock().expect("UDP owner 端点集合锁中毒");
    let desired = bindings.iter().copied().collect::<BTreeSet<_>>();
    let removals = endpoints
        .iter()
        .filter(|(binding, _)| !desired.contains(binding))
        .map(|(_, endpointId)| *endpointId)
        .collect::<Vec<_>>();
    let mut additions = Vec::new();
    let missingBindings = desired
        .iter()
        .filter(|binding| !endpoints.contains_key(binding))
        .copied()
        .collect::<Vec<_>>();
    for binding in missingBindings {
        let endpointId = nextOwnerUdpEndpointId.fetch_add(1, Ordering::Relaxed);
        additions.push((
            binding.processId,
            endpointId,
            binding.localAddress,
            binding.localPort,
        ));
        endpoints.insert(binding, endpointId);
    }
    flowTable.replaceUdpOwnerBindings(&removals, &additions);
    endpoints.retain(|binding, _| desired.contains(binding));
}

/// 仅补建当前唯一 owner 的缺失 synthetic 端点；包级关联不得删除其他已选进程的现有绑定。
fn ensureOwnerUdpBindings(
    flowTable: &CaptureFlowTable,
    ownerUdpEndpoints: &Mutex<BTreeMap<OwnedUdpBinding, u64>>,
    nextOwnerUdpEndpointId: &AtomicU64,
    bindings: &[OwnedUdpBinding],
) {
    let mut endpoints = ownerUdpEndpoints.lock().expect("UDP owner 端点集合锁中毒");
    let missingBindings = bindings
        .iter()
        .filter(|binding| !endpoints.contains_key(binding))
        .copied()
        .collect::<Vec<_>>();
    let mut additions = Vec::with_capacity(missingBindings.len());
    for binding in missingBindings {
        let endpointId = nextOwnerUdpEndpointId.fetch_add(1, Ordering::Relaxed);
        endpoints.insert(binding, endpointId);
        additions.push((
            binding.processId,
            endpointId,
            binding.localAddress,
            binding.localPort,
        ));
    }
    flowTable.replaceUdpOwnerBindings(&[], &additions);
}

/// 以 PID 与本地绑定身份回收预注册 synthetic 端点；真实 CLOSE/FLOW endpointId 无需与其相同。
fn removeSyntheticUdpBinding(
    flowTable: &CaptureFlowTable,
    ownerUdpEndpoints: &Mutex<BTreeMap<OwnedUdpBinding, u64>>,
    binding: OwnedUdpBinding,
    eventTimestamp: i64,
) {
    let normalizedBinding = OwnedUdpBinding {
        localAddress: crate::normalizeIpAddress(binding.localAddress),
        ..binding
    };
    let removedEndpointIds = {
        let mut endpoints = ownerUdpEndpoints.lock().expect("UDP owner 端点集合锁中毒");
        let endpointIds = endpoints
            .iter()
            .filter(|(existing, _)| {
                existing.processId == normalizedBinding.processId
                    && existing.localPort == normalizedBinding.localPort
                    && existing.localAddress.is_ipv4() == normalizedBinding.localAddress.is_ipv4()
                    && (existing.localAddress == normalizedBinding.localAddress
                        || existing.localAddress.is_unspecified()
                        || normalizedBinding.localAddress.is_unspecified())
            })
            .map(|(_, endpointId)| *endpointId)
            .collect::<Vec<_>>();
        endpoints.retain(|_, endpointId| !endpointIds.contains(endpointId));
        endpointIds
    };
    flowTable.removeEndpointsAt(&removedEndpointIds, eventTimestamp);
}

impl StartupGuard {
    /// 移交已完整启动的工作集；调用后 Drop 不再执行回滚。
    fn take(&mut self) -> WorkerSet {
        self.workers.take().expect("启动工作集已移交")
    }
}

impl Drop for StartupGuard {
    /// 按正常 stop 顺序回滚部分启动；回滚错误不覆盖原始线程创建错误。
    fn drop(&mut self) {
        self.flowTable.setAssociationNotifier(None);
        if let Some(mut workers) = self.workers.take() {
            let _ = stopWorkerSet(&mut workers);
        }
    }
}

/// 按 shutdown→取消 recv producer→join producer→排空 resolver 的唯一顺序停止工作集。
fn stopWorkerSet(workers: &mut WorkerSet) -> (Option<String>, bool) {
    workers.stopRequested.store(true, Ordering::Release);
    let shutdownResults = workers
        .shutdownHandles
        .drain(..)
        .map(|(layer, shutdown)| shutdownReceiveLayer(layer, shutdown))
        .collect::<Vec<_>>();
    // producer 只执行 recv 和入队，因此 CancelSynchronousIo 不会命中任何重注入发送。
    let cancellationResults = workers
        .producerWorkers
        .iter()
        .map(cancelWorkerSynchronousIo)
        .collect::<Vec<_>>();
    let shutdownError = firstStopFailure(shutdownResults, cancellationResults);
    let mut workerPanicked = false;
    for worker in workers.producerWorkers.drain(..) {
        workerPanicked |= worker.join().is_err();
    }
    if let Some(receiverDone) = workers.networkReceiverDone.take() {
        receiverDone.store(true, Ordering::Release);
    }
    if let Some(resolverWake) = workers.resolverWake.take() {
        let _ = resolverWake.try_send(());
    }
    if let Some(resolverWorker) = workers.resolverWorker.take() {
        workerPanicked |= resolverWorker.join().is_err();
    }
    (shutdownError, workerPanicked)
}

/// 关闭一个 WinDivert 接收层；句柄已由工作线程关闭时按幂等成功处理，其余驱动错误保留层名返回。
fn shutdownReceiveLayer(layer: &str, shutdown: ShutdownHandle) -> Result<(), String> {
    match shutdown.shutdown_recv() {
        Ok(()) => Ok(()),
        Err(WinDivertError::OSError(error)) if error.code().0 == invalidHandleHresult => {
            // stop 与工作线程观察驱动关闭可能并发；ERROR_INVALID_HANDLE 证明该层已无可接收句柄，
            // 再把它上报为停止失败只会让已完成的数据面关闭被控制层误标为 faulted。
            Ok(())
        }
        Err(error) => Err(format!("{layer} shutdown：{error}")),
    }
}

/// 取消工作线程当前挂起的同步 Win32 IO；没有挂起调用时同样视为完成。
fn cancelWorkerSynchronousIo(worker: &JoinHandle<()>) -> Result<(), String> {
    // SAFETY: JoinHandle 在调用期间持有有效线程句柄；本函数不关闭或转移该句柄。
    let cancelled = unsafe { CancelSynchronousIo(worker.as_raw_handle() as HANDLE) };
    if cancelled != 0 {
        return Ok(());
    }
    // SAFETY: 紧跟失败的 Win32 调用读取当前线程 last-error，不跨越其它 FFI。
    let error = unsafe { GetLastError() };
    if error == ERROR_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("取消工作线程同步 IO 失败：系统错误 {error}"))
    }
}

/// 合并停止阶段错误；调用方已先执行全部 shutdown 和 cancellation，本函数只选择首个根因。
fn firstStopFailure(
    shutdownResults: Vec<Result<(), String>>,
    cancellationResults: Vec<Result<(), String>>,
) -> Option<String> {
    shutdownResults
        .into_iter()
        .chain(cancellationResults)
        .find_map(Result::err)
}

impl Drop for ProcessCapture {
    /// 析构复用同步停止路径，确保异常退出前关闭句柄并清除会话五元组。
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Clone)]
struct NetworkWorkerContext {
    divert: Arc<WinDivert<NetworkLayer>>,
    flowTable: CaptureFlowTable,
    proxyPort: u16,
    stopRequested: Arc<AtomicBool>,
    redirectedPackets: Arc<AtomicU64>,
    restoredPackets: Arc<AtomicU64>,
    bytesUp: Arc<AtomicU64>,
    bytesDown: Arc<AtomicU64>,
    lastError: Arc<Mutex<Option<String>>>,
    connectionResetState: ConnectionResetState,
}

/// 汇聚 WinDivert UDP 拦截线程所需状态；resolver 是唯一执行封包决策、校验和重算与回注的写线端。
struct UdpObservationContext {
    flowTable: CaptureFlowTable,
    proxyAddress: IpAddr,
    proxyPort: u16,
    stopRequested: Arc<AtomicBool>,
    observedPacketsUp: Arc<AtomicU64>,
    observedPacketsDown: Arc<AtomicU64>,
    bytesUp: Arc<AtomicU64>,
    bytesDown: Arc<AtomicU64>,
    lastError: Arc<Mutex<Option<String>>>,
    udpDatagramSink: Arc<RwLock<Option<SharedUdpDatagramSink>>>,
    udpDatagramProcessor: Arc<RwLock<Option<SharedUdpDatagramProcessor>>>,
    selectedProcessIds: Arc<RwLock<BTreeSet<u32>>>,
    ownerUdpEndpoints: Arc<Mutex<BTreeMap<OwnedUdpBinding, u64>>>,
    nextOwnerUdpEndpointId: Arc<AtomicU64>,
    nextUdpCaptureSequence: Arc<AtomicU64>,
    processSelectionUpdateLock: Arc<Mutex<()>>,
}

/// 汇聚 SOCKET 观察线程的共享状态；用单一领域对象保证连接登记与指标记账使用同一捕获代际。
struct SocketWorkerContext {
    flowTable: CaptureFlowTable,
    proxyAddress: IpAddr,
    proxyPort: u16,
    stopRequested: Arc<AtomicBool>,
    lastError: Arc<Mutex<Option<String>>>,
    selectedProcessIds: Arc<RwLock<BTreeSet<u32>>>,
    acceptedConnections: Arc<AtomicU64>,
    ownerUdpEndpoints: Arc<Mutex<BTreeMap<OwnedUdpBinding, u64>>>,
    processSelectionUpdateLock: Arc<Mutex<()>>,
}

/// 携带 NETWORK 捕获时生成的绝对截止时间，确保队列等待不会扩大关联窗口。
struct PendingPacket {
    packet: WinDivertPacket<'static, NetworkLayer>,
    deadline: Instant,
}

/// 保存 receiver 已完成五元组判定的数据包；只有 resolver 可执行同步注入。
enum ReadyPacket {
    Forward {
        packet: WinDivertPacket<'static, NetworkLayer>,
        direction: Result<PacketDirection, crate::PacketRewriteError>,
    },
    Ipv6Reset {
        packets: Ipv6ResetPackets,
        address: windivert::address::WinDivertAddress<NetworkLayer>,
    },
}

/// 保留首个容量故障的当前包；已决策包继续按 direction 完成，未知 SYN 按原始语义恢复。
enum EmergencyPacket {
    Ready(ReadyPacket),
    Pending(PendingPacket),
}

/// 以独立上限隔离已决策包与待关联 SYN，pending 压力不得使已知流旁路。
#[derive(Default)]
struct ResolverQueues {
    ready: VecDeque<ReadyPacket>,
    pending: VecDeque<PendingPacket>,
    emergency: Option<EmergencyPacket>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueSelection {
    Pending,
    Ready,
    OverflowPending,
    OverflowReady,
}

/// 在不移动数据包的情况下选择有界队列；pending 满载不得影响已决策包。
fn selectQueue(pendingCandidate: bool, readyCount: usize, pendingCount: usize) -> QueueSelection {
    if pendingCandidate {
        if pendingCount < maximumPendingPackets {
            QueueSelection::Pending
        } else {
            QueueSelection::OverflowPending
        }
    } else if readyCount < maximumReadyPackets {
        QueueSelection::Ready
    } else {
        QueueSelection::OverflowReady
    }
}

/// 原样注入容量失败或停止恢复包；发送失败会终止整个捕获生命周期。
fn sendOriginalPacket(
    context: &NetworkWorkerContext,
    packet: &WinDivertPacket<'static, NetworkLayer>,
    worker: &'static str,
) {
    if let Err(error) = context.divert.send(packet) {
        recordWorkerError(&context.lastError, worker, error.to_string());
        context.stopRequested.store(true, Ordering::Release);
    }
}

/// 查询本地反射地址对应的实际接口索引，禁止依赖机器相关的固定接口编号。
fn bestInterfaceIndex(address: IpAddr) -> Result<u32, String> {
    let mut interfaceIndex = 0_u32;
    let result = match address {
        IpAddr::V4(address) => {
            let socketAddress = SOCKADDR_IN {
                sin_family: AF_INET,
                sin_port: 0,
                sin_addr: IN_ADDR {
                    S_un: IN_ADDR_0 {
                        S_addr: u32::from_ne_bytes(address.octets()),
                    },
                },
                sin_zero: [0; 8],
            };
            // SAFETY: `SOCKADDR_IN` 在调用期间保持有效，输出指针指向已初始化的 u32。
            unsafe {
                GetBestInterfaceEx(
                    (&raw const socketAddress).cast::<SOCKADDR>(),
                    &raw mut interfaceIndex,
                )
            }
        }
        IpAddr::V6(address) => {
            let socketAddress = SOCKADDR_IN6 {
                sin6_family: AF_INET6,
                sin6_port: 0,
                sin6_flowinfo: 0,
                sin6_addr: IN6_ADDR {
                    u: IN6_ADDR_0 {
                        Byte: address.octets(),
                    },
                },
                Anonymous: SOCKADDR_IN6_0 { sin6_scope_id: 0 },
            };
            // SAFETY: `SOCKADDR_IN6` 在调用期间保持有效，输出指针指向已初始化的 u32。
            unsafe {
                GetBestInterfaceEx(
                    (&raw const socketAddress).cast::<SOCKADDR>(),
                    &raw mut interfaceIndex,
                )
            }
        }
    };
    if result == 0 && interfaceIndex != 0 {
        Ok(interfaceIndex)
    } else {
        Err(format!(
            "解析本地地址 {address} 的接口失败：系统错误 {result}"
        ))
    }
}

/// 按全本地反射模型同步 WinDivert 注入元数据，确保重定向包从实际本地接口进入 TCP 栈。
///
/// 运行上下文：仅在目标地址和端口已经改写完成后调用。Windows/WinDivert 把回环流量视为仅出站，
/// 因此源和目标均已改为代理本地地址的包必须按仅出站 loopback 语义注入。
fn prepareRedirectedAddress(
    address: &mut windivert::address::WinDivertAddress<windivert::layer::NetworkLayer>,
    proxyInterfaceIndex: u32,
) {
    address.set_outbound(true);
    address.set_loopback(true);
    address.set_interface_index(proxyInterfaceIndex);
    address.set_subinterface_index(0);
}

/// 将 SOCKET 地址转换为规范化流，协议或端口不完整时保持系统直连。
fn socketFlow(
    address: &windivert::address::WinDivertAddress<windivert::layer::SocketLayer>,
) -> Option<CaptureFlow> {
    match address.protocol() {
        6 => CaptureFlow::tcp(
            address.process_id(),
            address.endpoint_id(),
            address.local_address(),
            address.local_port(),
            address.remote_address(),
            address.remote_port(),
        ),
        17 => CaptureFlow::udp(
            address.process_id(),
            address.endpoint_id(),
            address.local_address(),
            address.local_port(),
            address.remote_address(),
            address.remote_port(),
        ),
        _ => None,
    }
}

/// 将 FLOW ESTABLISHED 的最终五元组转换为关联记录；该事件可修正 SOCKET CONNECT 的通配本地地址。
fn flowLayerFlow(
    address: &windivert::address::WinDivertAddress<windivert::layer::FlowLayer>,
) -> Option<CaptureFlow> {
    match address.protocol() {
        6 => CaptureFlow::tcp(
            address.process_id(),
            address.endpoint_id(),
            address.local_address(),
            address.local_port(),
            address.remote_address(),
            address.remote_port(),
        ),
        17 => CaptureFlow::udp(
            address.process_id(),
            address.endpoint_id(),
            address.local_address(),
            address.local_port(),
            address.remote_address(),
            address.remote_port(),
        ),
        _ => None,
    }
}

/// 保留首个工作线程故障，避免后续派生错误覆盖根因。
fn recordWorkerError(lastError: &Mutex<Option<String>>, worker: &'static str, detail: String) {
    let mut error = lastError.lock().expect("捕获错误锁中毒");
    if error.is_none() {
        *error = Some(format!("{worker}：{detail}"));
    }
}

/// 记录不影响 WinDivert 数据面运行的旧连接清理诊断；相同错误只输出一次并由后续周期继续重试。
fn recordConnectionResetDiagnostic(
    diagnostic: &Mutex<Option<String>>,
    error: &ProcessCaptureError,
) {
    let detail = error.to_string();
    let mut diagnostic = diagnostic.lock().expect("连接重置诊断锁中毒");
    if diagnostic.as_deref() != Some(detail.as_str()) {
        eprintln!("进程捕获旧连接清理将在后台重试：{detail}");
        *diagnostic = Some(detail);
    }
}

/// 清除已经恢复的旧连接清理诊断；该字段从不参与 `snapshot.running` 的致命状态判定。
fn clearConnectionResetDiagnostic(diagnostic: &Mutex<Option<String>>) {
    *diagnostic.lock().expect("连接重置诊断锁中毒") = None;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// 构造可被 `isTcpStartPacket` 识别的最小 IPv4 SYN，供 pending 纯逻辑测试使用。
    fn testSynPacket() -> Vec<u8> {
        let mut packet = vec![0_u8; 40];
        packet[0] = 0x45;
        packet[9] = 6;
        packet[12..16].copy_from_slice(&[192, 0, 2, 10]);
        packet[16..20].copy_from_slice(&[198, 51, 100, 20]);
        packet[20..22].copy_from_slice(&52_000_u16.to_be_bytes());
        packet[22..24].copy_from_slice(&443_u16.to_be_bytes());
        packet[32] = 0x50;
        packet[33] = 0x02;
        packet
    }

    #[test]
    /// 验证启用但暂无运行实例时配置仍有效，使路径监视器后续可无重启加入新 PID。
    fn acceptsEnabledSelectionWithoutRunningProcess() {
        let configuration = ProcessCaptureConfiguration {
            enabled: true,
            processIds: BTreeSet::new(),
            proxyPort: 1080,
            proxyAddress: "0.0.0.0".parse().unwrap(),
        };
        assert!(configuration.validate(u32::MAX).is_ok());
    }

    #[test]
    /// 验证全本地反射使用仅出站 loopback 元数据，并能从系统路由动态取得 IPv4/IPv6 接口。
    fn synchronizesRedirectInjectionMetadata() {
        // SAFETY: 测试只读写地址元数据，不会把零初始化地址交给 WinDivertSend。
        let mut address = unsafe {
            windivert::address::WinDivertAddress::<windivert::layer::NetworkLayer>::new()
        };
        address.set_interface_index(12);
        address.set_subinterface_index(3);
        prepareRedirectedAddress(&mut address, 42);
        assert!(address.outbound());
        assert!(address.loopback());
        assert_eq!(address.interface_index(), 42);
        assert_eq!(address.subinterface_index(), 0);

        assert!(bestInterfaceIndex("127.0.0.1".parse().unwrap()).unwrap() > 0);
        assert!(bestInterfaceIndex("::1".parse().unwrap()).unwrap() > 0);
    }

    #[test]
    /// 验证析构始终复用 stop 清理会话状态；真实句柄线程由同一路径同步关闭和 join。
    fn dropClearsCaptureState() {
        let capture = ProcessCapture::new();
        assert!(
            capture.flowTable.insert(
                CaptureFlow::tcp(
                    42,
                    7,
                    "192.0.2.10".parse().unwrap(),
                    52000,
                    "198.51.100.20".parse().unwrap(),
                    443,
                )
                .unwrap(),
                "127.0.0.1".parse().unwrap(),
                1080,
            )
        );
        let observedTable = capture.flowTable.clone();
        drop(capture);
        assert!(observedTable.isEmpty());
    }

    #[test]
    /// 验证任一 shutdown 失败仍会先执行全部取消动作，再由纯逻辑合并器返回最早根因。
    fn shutdownFailureDoesNotSkipWorkerCancellation() {
        let shutdownCalls = AtomicUsize::new(0);
        let cancellationCalls = AtomicUsize::new(0);
        let shutdownResults = (0..3)
            .map(|index| {
                shutdownCalls.fetch_add(1, Ordering::Relaxed);
                if index == 0 {
                    Err("SOCKET shutdown 失败".to_owned())
                } else {
                    Ok(())
                }
            })
            .collect();
        let cancellationResults = (0..3)
            .map(|_| {
                cancellationCalls.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
            .collect();
        assert_eq!(
            firstStopFailure(shutdownResults, cancellationResults),
            Some("SOCKET shutdown 失败".to_owned())
        );
        assert_eq!(shutdownCalls.load(Ordering::Relaxed), 3);
        assert_eq!(cancellationCalls.load(Ordering::Relaxed), 3);
    }

    #[test]
    /// 验证 resolver 必须同时观察到停止、producer 完成和本地队列为空，避免入队窗口丢包。
    fn resolverExitRequiresProducerCompletion() {
        assert!(!resolverCanExit(true, false, true));
        assert!(!resolverCanExit(true, true, false));
        assert!(!resolverCanExit(false, true, true));
        assert!(resolverCanExit(true, true, true));
    }

    #[test]
    /// 验证 SYN 关联截止时间固定于 NETWORK 捕获时刻，后续队列积压不会重置窗口。
    fn associationDeadlineStartsAtCaptureTime() {
        let capturedAt = Instant::now();
        let deadline = associationDeadline(capturedAt);
        let resolverObservedAt = capturedAt + Duration::from_millis(10);
        assert_eq!(
            deadline.duration_since(capturedAt),
            Duration::from_millis(associationWaitMilliseconds)
        );
        assert_eq!(
            deadline.duration_since(resolverObservedAt),
            Duration::from_millis(associationWaitMilliseconds - 10)
        );
    }

    #[test]
    /// 验证 B 流关联通知不会使仍未命中的 A 流 SYN 提前旁路；A 只受自身重查结果和截止时间控制。
    fn unrelatedAssociationCannotReleasePendingSyn() {
        let packet = testSynPacket();
        let now = Instant::now();
        let deadline = now + Duration::from_millis(associationWaitMilliseconds);
        assert!(shouldKeepPending(
            &Ok(PacketDirection::Bypass),
            &packet,
            true,
            deadline,
            now,
        ));
        assert!(!shouldKeepPending(
            &Ok(PacketDirection::Bypass),
            &packet,
            true,
            deadline,
            deadline,
        ));
    }

    #[test]
    /// 验证未选 SYN 满载时精确丢弃，而已决策包仍进入独立 ready 队列。
    fn pendingCapacityCannotBypassOrStarveKnownTraffic() {
        assert_eq!(
            selectQueue(true, 0, maximumPendingPackets),
            QueueSelection::OverflowPending
        );
        assert_eq!(
            selectQueue(false, 0, maximumPendingPackets),
            QueueSelection::Ready
        );
        assert_eq!(
            selectQueue(false, maximumReadyPackets, 0),
            QueueSelection::OverflowReady
        );
    }

    #[test]
    /// 验证两种容量故障都保留当前包；pending 故障恢复原包，ready 故障保留已决策 direction。
    fn capacityFaultRetainsEmergencyPacketWithoutReplacingQueuedPackets() {
        // SAFETY: 测试只检查队列所有权和决策类型，不会把零初始化地址交给 WinDivertSend。
        let pendingPacket = unsafe { WinDivertPacket::<NetworkLayer>::new(testSynPacket()) };
        let mut pendingQueues = ResolverQueues::default();
        pendingQueues.pending.push_back(PendingPacket {
            // SAFETY: 同上，仅作为已入队包的所有权占位。
            packet: unsafe { WinDivertPacket::<NetworkLayer>::new(testSynPacket()) },
            deadline: Instant::now(),
        });
        pendingQueues.emergency = Some(EmergencyPacket::Pending(PendingPacket {
            packet: pendingPacket,
            deadline: Instant::now(),
        }));
        assert_eq!(pendingQueues.pending.len(), 1);
        assert!(matches!(
            pendingQueues.emergency,
            Some(EmergencyPacket::Pending(_))
        ));

        let mut readyQueues = ResolverQueues::default();
        readyQueues.ready.push_back(ReadyPacket::Forward {
            // SAFETY: 同上，数据包不进入真实注入路径。
            packet: unsafe { WinDivertPacket::<NetworkLayer>::new(testSynPacket()) },
            direction: Ok(PacketDirection::Bypass),
        });
        readyQueues.emergency = Some(EmergencyPacket::Ready(ReadyPacket::Forward {
            // SAFETY: 同上，只验证容量故障决策未丢失。
            packet: unsafe { WinDivertPacket::<NetworkLayer>::new(testSynPacket()) },
            direction: Ok(PacketDirection::Bypass),
        }));
        assert_eq!(readyQueues.ready.len(), 1);
        assert!(matches!(
            readyQueues.emergency,
            Some(EmergencyPacket::Ready(ReadyPacket::Forward {
                direction: Ok(PacketDirection::Bypass),
                ..
            }))
        ));
    }

    #[test]
    /// 验证 resolver、NETWORK、SOCKET、FLOW 任一创建边界失败时，guard 都会 join 已启动线程。
    fn startupGuardRollsBackEverySpawnBoundary() {
        for failedAt in 0..4 {
            let joined = Arc::new(AtomicUsize::new(0));
            let mut workers = WorkerSet {
                stopRequested: Arc::new(AtomicBool::new(false)),
                shutdownHandles: Vec::new(),
                resolverWake: None,
                networkReceiverDone: None,
                producerWorkers: Vec::new(),
                resolverWorker: None,
            };
            for role in 0..4 {
                if role == failedAt {
                    break;
                }
                let joined = Arc::clone(&joined);
                let worker = thread::spawn(move || {
                    joined.fetch_add(1, Ordering::Release);
                });
                if role == 0 {
                    workers.resolverWorker = Some(worker);
                } else {
                    workers.producerWorkers.push(worker);
                }
            }
            drop(StartupGuard {
                workers: Some(workers),
                flowTable: CaptureFlowTable::default(),
            });
            assert_eq!(joined.load(Ordering::Acquire), failedAt);
        }
    }
}
