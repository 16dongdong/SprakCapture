//! 将 WinDivert 已完成统一写线决策的 UDP 数据报先顺序写入有界磁盘队列，再逐包投影为可检查事务。
//!
//! 驱动线程只负责按捕获顺序追加帧；录制会话变慢时正文停留在磁盘而不是无界堆内存。
//! 单一消费者在事务完整提交后才推进读游标，因此服务 stop/restart 不会丢弃已经捕获的正文。

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};

use capture_core::{
    BeginTransaction, BodyWrite, MessageSide, RecordingSession, StreamPacket,
    StreamPacketModification, TransactionCompletion, TransactionProtocol,
};
use location_core::ResolvedLocation;
use process_capture_core::{UdpDatagramDirection, UdpDatagramEvent, UdpDatagramSink};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot, watch};

use crate::controlApi::ProcessSelectionStore;

/// 为 UDP 事务解析本机进程显示名；实现必须按当前 PID 查询，进程退出时返回 `None`。
pub trait UdpProcessNameResolver: Send + Sync {
    fn resolve(&self, processId: u32) -> Option<String>;
}

impl UdpProcessNameResolver for ProcessSelectionStore {
    /// 使用进程选择存储的实时系统枚举解析 PID，禁止跨 PID 回收周期保存旧名称。
    fn resolve(&self, processId: u32) -> Option<String> {
        self.processIdentity(processId).map(|process| process.name)
    }
}

impl<F> UdpProcessNameResolver for F
where
    F: Fn(u32) -> Option<String> + Send + Sync,
{
    /// 调用注入的解析函数；闭包由调用方负责遵守实时 PID 语义。
    fn resolve(&self, processId: u32) -> Option<String> {
        self(processId)
    }
}

const binaryContentType: &str = "application/octet-stream";
const binaryEncoding: &str = "binary";
const spoolFilePrefix: &str = "udpCapture-";
const spoolFileSuffix: &str = ".spool";
const legacySpoolFileName: &str = "udpCapture.spool";
const maximumSpoolBytes: u64 = 2 * 1024 * 1024 * 1024;
const maximumSegmentBytes: u64 = 64 * 1024 * 1024;
const frameLengthBytes: u64 = 8;
// WPE 差异同时保留原值和写线值，最坏情况下整份 64 KiB UDP 正文都发生变化；JSON 数组会放大元数据，
// 因此头预算必须覆盖该合法上界，同时仍以固定 1 MiB 限制损坏 spool 的分配规模。
const maximumHeaderBytes: u32 = 1024 * 1024;
const maximumPayloadBytes: u32 = 65_535;
pub const captureQueueCapacity: usize = 1_024;
const processIdentityCacheLifetime: Duration = Duration::from_secs(1);
const recordingEpochFileName: &str = "udpRecordingEpochs.json";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpoolHeader {
    sequence: u64,
    #[serde(default)]
    recordingProcessGeneration: String,
    #[serde(default)]
    recordingEpoch: u64,
    processId: u32,
    clientAddress: SocketAddr,
    targetAddress: SocketAddr,
    direction: UdpDatagramDirectionWire,
    capturedAtMilliseconds: u64,
    #[serde(default)]
    modifications: Vec<process_capture_core::UdpDatagramModification>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum UdpDatagramDirectionWire {
    Up,
    Down,
}

struct SpoolSegment {
    id: u64,
    path: PathBuf,
    cursorPath: PathBuf,
    file: Option<File>,
    length: u64,
    readOffset: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedCursor {
    recordingGeneration: String,
    segmentId: u64,
    sequence: u64,
    readOffset: u64,
}

struct SpoolState {
    segments: VecDeque<SpoolSegment>,
    nextSegmentId: u64,
    nextSequence: u64,
    pendingBytes: u64,
    closed: bool,
    retiringSegmentId: Option<u64>,
    retirementBlocked: bool,
    processGenerations: std::collections::BTreeSet<String>,
}

/// 暂时移出状态锁的队首文件句柄；删除失败时必须原样放回同一节点。
struct SegmentRetirement {
    segmentId: u64,
    path: PathBuf,
    cursorPath: PathBuf,
    file: File,
}

/// 保存一条尚未确认的磁盘帧；`endOffset` 只在事务完整提交后用于推进游标。
pub struct SpoolEntry {
    pub sequence: u64,
    pub event: UdpDatagramEvent,
    segmentId: u64,
    endOffset: u64,
    frameBytes: u64,
    recordingProcessGeneration: String,
    recordingEpoch: u64,
}

/// 为一次控制进程内的 clear 建立线性化水位；服务热重启共享对象，clear 才推进 epoch。
pub struct UdpRecordingCoordination {
    processGeneration: Arc<str>,
    epoch: AtomicU64,
    clearedEpochs: Mutex<BTreeMap<String, u64>>,
    sealedGenerations: Mutex<std::collections::BTreeSet<String>>,
    knownSpoolGenerations: Mutex<std::collections::BTreeSet<String>>,
    committedButUnacknowledged: Mutex<Option<SpoolIdentity>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SpoolIdentity {
    processGeneration: String,
    segmentId: u64,
    sequence: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedRecordingEpochs {
    clearedEpochs: BTreeMap<String, u64>,
    #[serde(default)]
    sealedGenerations: std::collections::BTreeSet<String>,
}

impl UdpRecordingCoordination {
    /// 创建控制进程级代际；`processGeneration` 必须在进程重建后变化，避免跳过遗留 spool。
    pub fn new(processGeneration: impl Into<Arc<str>>) -> Arc<Self> {
        Arc::new(Self {
            processGeneration: processGeneration.into(),
            epoch: AtomicU64::new(0),
            clearedEpochs: Mutex::new(BTreeMap::new()),
            sealedGenerations: Mutex::new(std::collections::BTreeSet::new()),
            knownSpoolGenerations: Mutex::new(std::collections::BTreeSet::new()),
            committedButUnacknowledged: Mutex::new(None),
        })
    }

    /// 加载跨进程 clear 屏障；损坏文件阻止启动，禁止旧 backlog 在新 RecordingSession 重现。
    pub fn load(
        dataDirectory: &Path,
        processGeneration: impl Into<Arc<str>>,
    ) -> io::Result<Arc<Self>> {
        let path = dataDirectory.join(recordingEpochFileName);
        let persisted = if path.exists() {
            let bytes = std::fs::read(path)?;
            serde_json::from_slice::<PersistedRecordingEpochs>(&bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("UDP clear 水位文件损坏：{error}"),
                )
            })?
        } else {
            PersistedRecordingEpochs {
                clearedEpochs: BTreeMap::new(),
                sealedGenerations: std::collections::BTreeSet::new(),
            }
        };
        Ok(Arc::new(Self {
            processGeneration: processGeneration.into(),
            epoch: AtomicU64::new(0),
            clearedEpochs: Mutex::new(persisted.clearedEpochs),
            sealedGenerations: Mutex::new(persisted.sealedGenerations),
            knownSpoolGenerations: Mutex::new(std::collections::BTreeSet::new()),
            committedButUnacknowledged: Mutex::new(None),
        }))
    }

    /// 在 clear 的共享串行锁内推进水位；此前已经捕获的帧随后只确认、不重建事务。
    pub fn advance(&self) -> u64 {
        let next = self.epoch.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        self.clearedEpochs
            .lock()
            .expect("UDP clear 水位锁中毒")
            .insert(self.processGeneration.to_string(), next);
        next
    }

    /// 先原子持久化新 clear 屏障再发布内存 epoch；落盘失败不改变当前录制可见性。
    pub fn advanceAndPersist(&self, dataDirectory: &Path) -> io::Result<u64> {
        let current = self.epoch.load(Ordering::Acquire);
        let next = current
            .checked_add(1)
            .ok_or_else(|| io::Error::other("UDP clear 水位耗尽"))?;
        let spoolGenerations = self
            .knownSpoolGenerations
            .lock()
            .expect("UDP spool 已知代际锁中毒")
            .clone();
        let mut clearedEpochs = self.clearedEpochs.lock().expect("UDP clear 水位锁中毒");
        let mut sealedGenerations = self.sealedGenerations.lock().expect("UDP 封存代际锁中毒");
        let previous = clearedEpochs.insert(self.processGeneration.to_string(), next);
        let previousSealed = sealedGenerations.clone();
        sealedGenerations.extend(
            spoolGenerations
                .into_iter()
                .filter(|generation| generation != self.processGeneration.as_ref()),
        );
        let persistResult =
            persistRecordingEpochs(dataDirectory, &clearedEpochs, &sealedGenerations);
        if let Err(error) = persistResult {
            match previous {
                Some(previous) => {
                    clearedEpochs.insert(self.processGeneration.to_string(), previous);
                }
                None => {
                    clearedEpochs.remove(self.processGeneration.as_ref());
                }
            }
            *sealedGenerations = previousSealed;
            return Err(error);
        }
        self.epoch.store(next, Ordering::Release);
        Ok(next)
    }

    /// 发布当前 spool 正文引用的全部代际；clear 据此一次封存旧进程仍未消费的后续帧。
    fn registerSpoolGenerations(&self, generations: &std::collections::BTreeSet<String>) {
        *self
            .knownSpoolGenerations
            .lock()
            .expect("UDP spool 已知代际锁中毒") = generations.clone();
    }

    /// 判断磁盘帧是否仍属于可见 epoch；持久屏障对控制进程崩溃后的遗留帧同样生效。
    fn shouldPersist(&self, processGeneration: &str, epoch: u64) -> bool {
        if processGeneration == self.processGeneration.as_ref() {
            return epoch == self.epoch.load(Ordering::Acquire);
        }
        if self
            .sealedGenerations
            .lock()
            .expect("UDP 封存代际锁中毒")
            .contains(processGeneration)
        {
            return false;
        }
        self.clearedEpochs
            .lock()
            .expect("UDP clear 水位锁中毒")
            .get(processGeneration)
            .is_none_or(|minimumVisibleEpoch| epoch >= *minimumVisibleEpoch)
    }

    /// 删除已经没有任何 spool 正文引用的旧进程水位；当前进程项始终保留以覆盖内存队列。
    fn pruneClearedEpochs(
        &self,
        dataDirectory: &Path,
        referencedGenerations: &std::collections::BTreeSet<String>,
    ) -> io::Result<()> {
        let mut clearedEpochs = self.clearedEpochs.lock().expect("UDP clear 水位锁中毒");
        let mut sealedGenerations = self.sealedGenerations.lock().expect("UDP 封存代际锁中毒");
        let previousLength = clearedEpochs.len();
        let previousSealedLength = sealedGenerations.len();
        clearedEpochs.retain(|generation, _| {
            generation == self.processGeneration.as_ref()
                || referencedGenerations.contains(generation)
        });
        sealedGenerations.retain(|generation| referencedGenerations.contains(generation));
        self.registerSpoolGenerations(referencedGenerations);
        if clearedEpochs.len() != previousLength || sealedGenerations.len() != previousSealedLength
        {
            persistRecordingEpochs(dataDirectory, &clearedEpochs, &sealedGenerations)?;
        }
        Ok(())
    }

    /// 判断同进程服务重启重放的帧是否已经提交事务但尚未推进磁盘游标。
    fn transactionAlreadyCommitted(&self, identity: &SpoolIdentity) -> bool {
        self.committedButUnacknowledged
            .lock()
            .expect("UDP 已提交未确认键锁中毒")
            .as_ref()
            == Some(identity)
    }

    /// 在事务 commit 返回后同步登记唯一未确认键；顺序消费者保证同一时刻最多存在一项。
    fn markTransactionCommitted(&self, identity: SpoolIdentity) {
        *self
            .committedButUnacknowledged
            .lock()
            .expect("UDP 已提交未确认键锁中毒") = Some(identity);
    }

    /// cursor ack 成功后释放幂等键；键不匹配表示代际或顺序损坏，必须保留现场。
    fn markSpoolAcknowledged(&self, identity: &SpoolIdentity) {
        let mut committed = self
            .committedButUnacknowledged
            .lock()
            .expect("UDP 已提交未确认键锁中毒");
        if committed.as_ref() == Some(identity) {
            *committed = None;
        }
    }

    /// 返回收包瞬间的进程代际与 clear 水位，二者随完整正文一起进入磁盘帧。
    fn snapshot(&self) -> (Arc<str>, u64) {
        (
            Arc::clone(&self.processGeneration),
            self.epoch.load(Ordering::Acquire),
        )
    }
}

/// 保存收包线性化水位和完整事件；固定队列只移动所有权，不复制正文。
pub struct CapturedUdpDatagram {
    event: UdpDatagramEvent,
    recordingProcessGeneration: Arc<str>,
    recordingEpoch: u64,
}

pub struct SpoolAcknowledgement {
    sequence: u64,
    segmentId: u64,
    endOffset: u64,
    frameBytes: u64,
}

/// 把磁盘确认结果返回异步消费者；clear 必须等该结果后才能越过同 epoch 事务。
struct SpoolAcknowledgementRequest {
    acknowledgement: SpoolAcknowledgement,
    completion: oneshot::Sender<io::Result<()>>,
}

struct CachedProcessIdentity {
    resolvedAt: Instant,
    name: Option<String>,
}

impl SpoolEntry {
    /// 在正文所有权移交给事务前复制轻量确认边界，保证失败时不会提前推进 spool。
    pub fn acknowledgement(&self) -> SpoolAcknowledgement {
        SpoolAcknowledgement {
            sequence: self.sequence,
            segmentId: self.segmentId,
            endOffset: self.endOffset,
            frameBytes: self.frameBytes,
        }
    }
}

/// 为 UDP 最终写线事件提供有界磁盘容量和严格 FIFO 读取语义。
pub struct UdpRecordingSpool {
    directory: PathBuf,
    recordingGeneration: String,
    state: Mutex<SpoolState>,
    acknowledgementLock: Mutex<()>,
    segmentRemover: Arc<dyn SpoolSegmentRemover>,
    changed: Condvar,
}

/// 抽象已确认分段的最终删除操作；生产环境直接删除文件，文件系统测试可注入可控延迟。
pub trait SpoolSegmentRemover: Send + Sync {
    /// 删除 `path` 指向的完整分段；失败必须保留原文件并返回精确 I/O 错误。
    fn remove(&self, path: &Path) -> io::Result<()>;
}

impl<F> SpoolSegmentRemover for F
where
    F: Fn(&Path) -> io::Result<()> + Send + Sync,
{
    fn remove(&self, path: &Path) -> io::Result<()> {
        self(path)
    }
}

impl UdpRecordingSpool {
    /// 在持久数据目录打开 spool；异常退出遗留的完整帧会在新录制会话中继续按序提交。
    /// 截断或损坏文件会阻止控制面启动并保留原文件，禁止静默丢弃未确认正文。
    pub fn create(
        dataDirectory: &Path,
        recordingGeneration: impl Into<String>,
    ) -> io::Result<Arc<Self>> {
        Self::createWithSegmentRemover(
            dataDirectory,
            recordingGeneration,
            Arc::new(|path: &Path| std::fs::remove_file(path)),
        )
    }

    /// 以指定分段删除器打开 spool；删除器只处理已经完整确认且不再追加的队首分段。
    pub fn createWithSegmentRemover(
        dataDirectory: &Path,
        recordingGeneration: impl Into<String>,
        segmentRemover: Arc<dyn SpoolSegmentRemover>,
    ) -> io::Result<Arc<Self>> {
        let recordingGeneration = recordingGeneration.into();
        if recordingGeneration.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP spool 录制代际不能为空",
            ));
        }
        let captureDirectory = dataDirectory.join("capture");
        std::fs::create_dir_all(&captureDirectory)?;
        migrateLegacySpool(&captureDirectory)?;
        let mut segmentPaths = discoverSegmentPaths(&captureDirectory)?;
        cleanupInterruptedCursorFiles(&captureDirectory, &segmentPaths)?;
        let mut segments = VecDeque::new();
        let mut pendingBytes = 0_u64;
        let mut nextSequence = 1_u64;
        let mut nextSegmentId = 1_u64;
        let mut processGenerations = std::collections::BTreeSet::new();
        for (segmentId, path) in segmentPaths.drain(..) {
            let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
            let (length, recoveredNextSequence, segmentGenerations) =
                scanExistingFrames(&mut file)?;
            processGenerations.extend(segmentGenerations);
            let cursorPath = cursorPathForSegment(&path);
            let readOffset = loadPersistedCursor(
                &mut file,
                &cursorPath,
                &recordingGeneration,
                segmentId,
                length,
            )?;
            file.seek(SeekFrom::Start(length))?;
            pendingBytes = pendingBytes
                .checked_add(length - readOffset)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "UDP spool 总长度溢出")
                })?;
            nextSequence = nextSequence.max(recoveredNextSequence);
            nextSegmentId = segmentId
                .checked_add(1)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "UDP spool 分段耗尽"))?;
            if readOffset == length && length > 0 {
                drop(file);
                removeRecoveredSegment(&path, &cursorPath)?;
                continue;
            }
            segments.push_back(SpoolSegment {
                id: segmentId,
                path,
                cursorPath,
                file: Some(file),
                length,
                readOffset,
            });
        }
        if segments.is_empty() {
            segments.push_back(createSegment(&captureDirectory, nextSegmentId)?);
            nextSegmentId = nextSegmentId.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "UDP spool 分段编号已经耗尽")
            })?;
        }
        Ok(Arc::new(Self {
            directory: captureDirectory,
            recordingGeneration,
            state: Mutex::new(SpoolState {
                segments,
                nextSegmentId,
                nextSequence,
                pendingBytes,
                closed: false,
                retiringSegmentId: None,
                retirementBlocked: false,
                processGenerations,
            }),
            acknowledgementLock: Mutex::new(()),
            segmentRemover,
            changed: Condvar::new(),
        }))
    }

    /// 阻塞读取当前最早未确认帧；关闭后仍会先返回全部已写入内容。
    pub fn readNext(&self) -> io::Result<Option<SpoolEntry>> {
        let mut state = self.state.lock().expect("UDP spool 状态锁中毒");
        loop {
            // 两阶段回收期间队首节点暂时没有文件句柄；读取者等待回收提交或恢复，追加者仍可
            // 使用队尾活动分段。该边界保证删除失败不会越过原队首而破坏事务顺序。
            while state.retiringSegmentId.is_some() {
                state = self.changed.wait(state).expect("UDP spool 回收等待锁中毒");
            }
            let frontComplete = state
                .segments
                .front()
                .is_some_and(|segment| segment.readOffset == segment.length);
            if !frontComplete {
                break;
            }
            if state.retirementBlocked {
                return Ok(None);
            }
            if state.segments.len() == 1 && !state.closed {
                state = self.changed.wait(state).expect("UDP spool 等待锁中毒");
                continue;
            }
            if state.segments.is_empty() {
                return Ok(None);
            }
            // 最后一帧确认时 writer 可能尚未滚动；后续滚动发生后，已确认队首必须在这里
            // 循环回收并继续下一段。直接返回 None 会让唯一 reader 永久退出并使 stop 排空卡死。
            drop(state);
            self.retireCompletedFront()?;
            state = self.state.lock().expect("UDP spool 状态锁中毒");
        }
        let Some(segment) = state.segments.front_mut() else {
            return Ok(None);
        };
        let readOffset = segment.readOffset;
        let file = segment
            .file
            .as_mut()
            .expect("UDP spool 活动分段必须持有文件");
        file.seek(SeekFrom::Start(readOffset))?;
        let headerLength = readU32(file)?;
        let payloadLength = readU32(file)?;
        validateFrameLengths(headerLength, payloadLength)?;
        let mut headerBytes = vec![
            0_u8;
            usize::try_from(headerLength).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "UDP spool 头长度超出平台范围")
            })?
        ];
        file.read_exact(&mut headerBytes)?;
        let header: SpoolHeader = serde_json::from_slice(&headerBytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("UDP spool 头损坏：{error}"),
            )
        })?;
        let mut payload = vec![
            0_u8;
            usize::try_from(payloadLength).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "UDP spool 正文长度超出平台范围")
            })?
        ];
        file.read_exact(&mut payload)?;
        let endOffset = readOffset
            .checked_add(frameLengthBytes)
            .and_then(|value| value.checked_add(u64::from(headerLength)))
            .and_then(|value| value.checked_add(u64::from(payloadLength)))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "UDP spool 偏移溢出"))?;
        Ok(Some(SpoolEntry {
            sequence: header.sequence,
            recordingProcessGeneration: header.recordingProcessGeneration,
            recordingEpoch: header.recordingEpoch,
            event: UdpDatagramEvent {
                processId: header.processId,
                clientAddress: header.clientAddress,
                targetAddress: header.targetAddress,
                direction: match header.direction {
                    UdpDatagramDirectionWire::Up => UdpDatagramDirection::Up,
                    UdpDatagramDirectionWire::Down => UdpDatagramDirection::Down,
                },
                payload,
                capturedAtMilliseconds: header.capturedAtMilliseconds,
                modifications: header.modifications,
            },
            segmentId: segment.id,
            endOffset,
            frameBytes: endOffset - readOffset,
        }))
    }

    /// 顺序回收已确认队首；确认锁阻止与事务 ack 竞争，磁盘 I/O 仍在状态锁外完成。
    fn retireCompletedFront(&self) -> io::Result<()> {
        let _acknowledgementGuard = self
            .acknowledgementLock
            .lock()
            .expect("UDP spool 顺序确认锁中毒");
        let retirement = {
            let mut state = self.state.lock().expect("UDP spool 滚动回收锁中毒");
            if state.retiringSegmentId.is_some() {
                return Ok(());
            }
            let Some(segment) = state.segments.front() else {
                return Ok(());
            };
            if segment.readOffset != segment.length || (state.segments.len() == 1 && !state.closed)
            {
                return Ok(());
            }
            beginSegmentRetirement(&mut state)?
        };
        self.finishSegmentRetirement(retirement)
    }

    /// 在对应事务提交后确认当前帧；全部消费完成时截断文件并立即归还磁盘预算。
    pub fn acknowledge(&self, entry: SpoolAcknowledgement) -> io::Result<()> {
        // 独立确认锁把游标提交严格串行化，但不阻塞追加 writer 使用的 state 锁。
        // 磁盘 create/write/rename 全部发生在 state 锁外，高流量期间确认盘抖动只会增加
        // 未确认 spool 占用，不会卡住固定内存队列的落盘线程。
        let _acknowledgementGuard = self
            .acknowledgementLock
            .lock()
            .expect("UDP spool 顺序确认锁中毒");
        let cursorPath = {
            let state = self.state.lock().expect("UDP spool 确认预检锁中毒");
            let Some(segment) = state.segments.front() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "UDP spool 没有可确认分段",
                ));
            };
            if segment.id != entry.segmentId
                || entry.endOffset <= segment.readOffset
                || entry.endOffset > segment.length
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "UDP spool 确认边界非法：sequence={}, segment={}, end={}",
                        entry.sequence, entry.segmentId, entry.endOffset
                    ),
                ));
            }
            segment.cursorPath.clone()
        };
        persistCursor(
            &cursorPath,
            PersistedCursor {
                recordingGeneration: self.recordingGeneration.clone(),
                segmentId: entry.segmentId,
                sequence: entry.sequence,
                readOffset: entry.endOffset,
            },
        )?;
        let retirement = {
            let mut state = self.state.lock().expect("UDP spool 确认提交锁中毒");
            let segmentComplete = {
                let Some(segment) = state.segments.front_mut() else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "UDP spool 确认期间分段消失",
                    ));
                };
                if segment.id != entry.segmentId
                    || entry.endOffset <= segment.readOffset
                    || entry.endOffset > segment.length
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "UDP spool 确认期间顺序边界发生变化",
                    ));
                }
                segment.readOffset = entry.endOffset;
                segment.readOffset == segment.length
            };
            state.pendingBytes = state
                .pendingBytes
                .checked_sub(entry.frameBytes)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "UDP spool 未确认计数下溢")
                })?;
            if segmentComplete && (state.segments.len() > 1 || state.closed) {
                Some(beginSegmentRetirement(&mut state)?)
            } else {
                None
            }
        };
        if let Some(retirement) = retirement {
            self.finishSegmentRetirement(retirement)?;
        }
        Ok(())
    }

    /// 在状态锁外关闭并删除已确认分段，再以短临界区提交成功或恢复失败节点。
    fn finishSegmentRetirement(&self, retirement: SegmentRetirement) -> io::Result<()> {
        let segmentId = retirement.segmentId;
        match retireSegmentFiles(retirement, self.segmentRemover.as_ref()) {
            Ok(cursorCleanupError) => {
                let mut state = self.state.lock().expect("UDP spool 回收提交锁中毒");
                validateRetiringSegment(&state, segmentId)?;
                state.segments.pop_front();
                state.retiringSegmentId = None;
                state.retirementBlocked = false;
                self.changed.notify_all();
                drop(state);
                if let Some(error) = cursorCleanupError {
                    // 正文分段已经成功删除，孤立游标不会参与恢复；下次打开 spool 会再次清理。
                    eprintln!("UDP spool 已确认游标延迟清理：segment={segmentId}, error={error}");
                }
                Ok(())
            }
            Err((error, recoveryFile)) => {
                let mut state = self.state.lock().expect("UDP spool 回收恢复锁中毒");
                validateRetiringSegment(&state, segmentId)?;
                state
                    .segments
                    .front_mut()
                    .expect("UDP spool 回收失败时队首必须存在")
                    .file = Some(recoveryFile);
                state.retiringSegmentId = None;
                state.retirementBlocked = true;
                self.changed.notify_all();
                Err(error)
            }
        }
    }

    /// 关闭生产端并唤醒读取线程；读取线程仍会排空关闭前已经追加的全部帧。
    pub fn close(&self) {
        let mut state = self.state.lock().expect("UDP spool 关闭锁中毒");
        state.closed = true;
        self.changed.notify_all();
    }

    /// 返回当前正文分段实际引用的控制进程代际，供 clear 屏障安全裁剪历史项。
    fn processGenerations(&self) -> std::collections::BTreeSet<String> {
        self.state
            .lock()
            .expect("UDP spool 代际读取锁中毒")
            .processGenerations
            .clone()
    }
}

impl UdpRecordingSpool {
    /// 由独立 writer 按队列顺序追加帧；写入前按未确认字节硬性检查磁盘预算。
    pub fn appendEvent(&self, event: &UdpDatagramEvent) -> Result<(), String> {
        self.appendCapturedEvent(&CapturedUdpDatagram {
            event: event.clone(),
            recordingProcessGeneration: Arc::from(self.recordingGeneration.clone()),
            recordingEpoch: 0,
        })
    }

    /// 保存收包时固定的 clear 水位；writer 不能在排队后重新读取 epoch，否则旧包会穿越 clear。
    fn appendCapturedEvent(&self, captured: &CapturedUdpDatagram) -> Result<(), String> {
        let mut state = self.state.lock().expect("UDP spool 追加锁中毒");
        if state.closed {
            return Err("UDP spool 已关闭".to_owned());
        }
        let sequence = state.nextSequence;
        let followingSequence = sequence
            .checked_add(1)
            .ok_or_else(|| "UDP spool 序号已经耗尽".to_owned())?;
        let header = SpoolHeader {
            sequence,
            recordingProcessGeneration: captured.recordingProcessGeneration.to_string(),
            recordingEpoch: captured.recordingEpoch,
            processId: captured.event.processId,
            clientAddress: captured.event.clientAddress,
            targetAddress: captured.event.targetAddress,
            direction: match captured.event.direction {
                UdpDatagramDirection::Up => UdpDatagramDirectionWire::Up,
                UdpDatagramDirection::Down => UdpDatagramDirectionWire::Down,
            },
            capturedAtMilliseconds: captured.event.capturedAtMilliseconds,
            modifications: captured.event.modifications.clone(),
        };
        state
            .processGenerations
            .insert(header.recordingProcessGeneration.clone());
        let headerBytes = serde_json::to_vec(&header)
            .map_err(|error| format!("序列化 UDP spool 头失败：{error}"))?;
        let headerLength =
            u32::try_from(headerBytes.len()).map_err(|_| "UDP spool 头超过 u32 长度".to_owned())?;
        let payloadLength = u32::try_from(captured.event.payload.len())
            .map_err(|_| "UDP spool 正文超过 u32 长度".to_owned())?;
        validateFrameLengths(headerLength, payloadLength)
            .map_err(|error| format!("UDP spool 帧长度无效：{error}"))?;
        let frameBytes = frameLengthBytes + u64::from(headerLength) + u64::from(payloadLength);
        let nextPendingBytes = state
            .pendingBytes
            .checked_add(frameBytes)
            .ok_or_else(|| "UDP spool 未确认占用溢出".to_owned())?;
        if nextPendingBytes > maximumSpoolBytes {
            return Err(format!(
                "UDP spool 未确认占用 {} B，当前帧 {frameBytes} B 将超过预算 {maximumSpoolBytes} B",
                state.pendingBytes
            ));
        }
        let shouldRotate = state.segments.back().is_some_and(|segment| {
            segment.length > 0 && segment.length + frameBytes > maximumSegmentBytes
        });
        if shouldRotate {
            let segmentId = state.nextSegmentId;
            state.nextSegmentId = state
                .nextSegmentId
                .checked_add(1)
                .ok_or_else(|| "UDP spool 分段编号已经耗尽".to_owned())?;
            state.segments.push_back(
                createSegment(&self.directory, segmentId)
                    .map_err(|error| format!("创建 UDP spool 分段失败：{error}"))?,
            );
        }
        let segment = state
            .segments
            .back_mut()
            .expect("UDP spool 必须存在活动分段");
        let writeOffset = segment.length;
        let file = segment
            .file
            .as_mut()
            .expect("UDP spool 活动分段必须持有文件");
        file.seek(SeekFrom::Start(writeOffset))
            .map_err(|error| format!("定位 UDP spool 分段末尾失败：{error}"))?;
        {
            file.write_all(&headerLength.to_le_bytes())
                .and_then(|_| file.write_all(&payloadLength.to_le_bytes()))
                .and_then(|_| file.write_all(&headerBytes))
                .and_then(|_| file.write_all(&captured.event.payload))
                .map_err(|error| format!("追加 UDP spool 失败：{error}"))?;
        }
        segment.length = segment
            .length
            .checked_add(frameBytes)
            .ok_or_else(|| "UDP spool 写偏移溢出".to_owned())?;
        state.pendingBytes = nextPendingBytes;
        state.nextSequence = followingSequence;
        self.changed.notify_one();
        Ok(())
    }
}

struct CaptureQueueState {
    sender: Option<SyncSender<CapturedUdpDatagram>>,
    emergency: Option<CapturedUdpDatagram>,
    retainedAfterFailure: VecDeque<CapturedUdpDatagram>,
    fault: Option<String>,
}

/// 以固定内存容量隔离 WinDivert 收包与磁盘抖动；唯一 emergency 槽保存触发满载的当前包。
pub struct QueuedUdpDatagramSink {
    state: Mutex<CaptureQueueState>,
    coordination: Arc<UdpRecordingCoordination>,
}

impl QueuedUdpDatagramSink {
    /// 创建固定容量 FIFO；返回值中的 receiver 只交给唯一磁盘 writer。
    fn create(
        coordination: Arc<UdpRecordingCoordination>,
    ) -> (Arc<Self>, Receiver<CapturedUdpDatagram>) {
        let (sender, receiver) = sync_channel(captureQueueCapacity);
        (
            Arc::new(Self {
                state: Mutex::new(CaptureQueueState {
                    sender: Some(sender),
                    emergency: None,
                    retainedAfterFailure: VecDeque::new(),
                    fault: None,
                }),
                coordination,
            }),
            receiver,
        )
    }

    /// 保存首个异步故障并关闭生产端；已进入 FIFO 的数据仍由 writer 顺序排空。
    fn fail(&self, detail: String) {
        let mut state = self.state.lock().expect("UDP 捕获队列故障锁中毒");
        state.fault.get_or_insert(detail);
        state.sender.take();
    }

    /// 取走触发队列满载的最后一个包；必须在普通 FIFO 完全断开并排空后调用。
    fn takeEmergency(&self) -> Option<CapturedUdpDatagram> {
        self.state
            .lock()
            .expect("UDP 捕获队列 emergency 锁中毒")
            .emergency
            .take()
    }

    /// spool 不可写或达到预算时保留尚未落盘的有界事件并暴露故障，禁止 receiver drop 静默丢包。
    fn retainAfterSpoolFailure(
        &self,
        current: CapturedUdpDatagram,
        receiver: &Receiver<CapturedUdpDatagram>,
        detail: String,
    ) {
        let mut state = self.state.lock().expect("UDP 捕获队列保留锁中毒");
        state.fault.get_or_insert(detail);
        state.sender.take();
        state.retainedAfterFailure.push_back(current);
        state.retainedAfterFailure.extend(receiver.try_iter());
        if let Some(emergency) = state.emergency.take() {
            state.retainedAfterFailure.push_back(emergency);
        }
    }

    /// 正常控制进程关闭时停止接收新副本；writer 会继续排空 FIFO 和 emergency。
    pub fn close(&self) {
        self.state
            .lock()
            .expect("UDP 捕获队列关闭锁中毒")
            .sender
            .take();
    }
}

impl UdpDatagramSink for QueuedUdpDatagramSink {
    /// 收包线程只执行一次有界 `try_send`；满载时保存当前包并立即返回显式故障，绝不访问磁盘。
    fn append(&self, event: UdpDatagramEvent) -> Result<(), String> {
        let (recordingProcessGeneration, recordingEpoch) = self.coordination.snapshot();
        let captured = CapturedUdpDatagram {
            event,
            recordingProcessGeneration,
            recordingEpoch,
        };
        let mut state = self.state.lock().expect("UDP 捕获队列追加锁中毒");
        let Some(sender) = state.sender.as_ref() else {
            return Err(state
                .fault
                .clone()
                .unwrap_or_else(|| "UDP 捕获队列已关闭".to_owned()));
        };
        match sender.try_send(captured) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(captured)) | Err(TrySendError::Disconnected(captured)) => {
                state.emergency = Some(captured);
                let detail = format!(
                    "UDP 捕获内存队列达到固定上限 {captureQueueCapacity}，当前包已保留到 emergency，观察线程已停止"
                );
                state.fault = Some(detail.clone());
                state.sender.take();
                Err(detail)
            }
        }
    }

    /// 返回 writer、容量或事务提交的首个故障。
    fn fault(&self) -> Option<String> {
        let state = self.state.lock().expect("UDP 捕获队列故障读取锁中毒");
        state.fault.as_ref().map(|detail| {
            if state.retainedAfterFailure.is_empty() {
                detail.clone()
            } else {
                format!(
                    "{detail}；内存保留 {} 个尚未落盘事件",
                    state.retainedAfterFailure.len()
                )
            }
        })
    }
}

/// 创建固定内存队列和磁盘 spool；调用方把 sink 安装到 ProcessCapture，再把其余对象交给消费者。
pub fn createUdpRecordingPipeline(
    dataDirectory: &Path,
    recordingGeneration: &str,
) -> io::Result<(
    Arc<QueuedUdpDatagramSink>,
    Arc<UdpRecordingSpool>,
    Receiver<CapturedUdpDatagram>,
)> {
    let coordination = UdpRecordingCoordination::new(Arc::<str>::from(recordingGeneration));
    createCoordinatedUdpRecordingPipeline(dataDirectory, recordingGeneration, coordination)
}

/// 创建共享 clear 水位的固定队列和 spool；控制面所有服务代际必须复用同一 coordination。
fn createCoordinatedUdpRecordingPipeline(
    dataDirectory: &Path,
    recordingGeneration: &str,
    coordination: Arc<UdpRecordingCoordination>,
) -> io::Result<(
    Arc<QueuedUdpDatagramSink>,
    Arc<UdpRecordingSpool>,
    Receiver<CapturedUdpDatagram>,
)> {
    let spool = UdpRecordingSpool::create(dataDirectory, recordingGeneration)?;
    let (sink, receiver) = QueuedUdpDatagramSink::create(coordination);
    Ok((sink, spool, receiver))
}

/// 拥有单次服务代际的 UDP 录制资源；停止必须等待 FIFO、emergency 和磁盘帧全部提交。
pub struct UdpRecordingRuntime {
    sink: Arc<QueuedUdpDatagramSink>,
    shutdownSender: watch::Sender<bool>,
    completion: tokio::task::JoinHandle<()>,
}

/// 聚合单次 UDP 录制消费者的并发资源；所有成员必须属于同一服务代际，禁止跨代复用。
struct UdpRecordingConsumerContext {
    recording: RecordingSession,
    processNameResolver: Arc<dyn UdpProcessNameResolver>,
    sink: Arc<QueuedUdpDatagramSink>,
    spool: Arc<UdpRecordingSpool>,
    captureReceiver: Receiver<CapturedUdpDatagram>,
    shutdownReceiver: watch::Receiver<bool>,
    coordination: Arc<UdpRecordingCoordination>,
    recordingUpdateLock: Arc<AsyncMutex<()>>,
}

impl UdpRecordingRuntime {
    /// 返回当前代际唯一 UDP 写线事件落点；下一次服务启动会创建全新实例，不继承 fault 状态。
    pub fn sink(&self) -> Arc<QueuedUdpDatagramSink> {
        Arc::clone(&self.sink)
    }

    /// 停止接收新事件并等待 writer、reader 和 RecordingSession 严格排空。
    pub async fn stopAndDrain(self) -> Result<(), String> {
        self.shutdownSender.send_replace(true);
        self.completion
            .await
            .map_err(|error| format!("等待 UDP 录制代际排空失败：{error}"))?;
        // completion 只表示线程均已退出；磁盘、读取或事务提交故障会通过 sink 留存。
        // 停服必须把该故障返回控制面，避免把“有正文尚未持久化”误报为正常排空。
        if let Some(error) = self.sink.fault() {
            return Err(error);
        }
        Ok(())
    }
}

/// 创建并启动单次服务代际的完整 UDP 录制管线；创建失败不会修改 ProcessCapture。
pub fn startUdpRecordingGeneration(
    dataDirectory: &Path,
    recording: RecordingSession,
    processNameResolver: Arc<dyn UdpProcessNameResolver>,
    recordingGeneration: &str,
) -> io::Result<UdpRecordingRuntime> {
    startCoordinatedUdpRecordingGeneration(
        dataDirectory,
        recording,
        processNameResolver,
        UdpRecordingCoordination::new(Arc::<str>::from(recordingGeneration)),
        Arc::new(AsyncMutex::new(())),
    )
}

/// 启动与 clear 共享 epoch 和串行锁的 UDP 录制代际；服务重启不得创建新的 coordination。
pub fn startCoordinatedUdpRecordingGeneration(
    dataDirectory: &Path,
    recording: RecordingSession,
    processNameResolver: Arc<dyn UdpProcessNameResolver>,
    coordination: Arc<UdpRecordingCoordination>,
    recordingUpdateLock: Arc<AsyncMutex<()>>,
) -> io::Result<UdpRecordingRuntime> {
    let recordingGeneration = coordination.processGeneration.to_string();
    let (sink, spool, captureReceiver) = createCoordinatedUdpRecordingPipeline(
        dataDirectory,
        &recordingGeneration,
        Arc::clone(&coordination),
    )?;
    let spoolGenerations = spool.processGenerations();
    coordination.registerSpoolGenerations(&spoolGenerations);
    coordination.pruneClearedEpochs(dataDirectory, &spoolGenerations)?;
    let (shutdownSender, shutdownReceiver) = watch::channel(false);
    let completion = startUdpRecording(UdpRecordingConsumerContext {
        recording,
        processNameResolver,
        sink: Arc::clone(&sink),
        spool,
        captureReceiver,
        shutdownReceiver,
        coordination,
        recordingUpdateLock,
    });
    Ok(UdpRecordingRuntime {
        sink,
        shutdownSender,
        completion,
    })
}

/// 启动唯一顺序磁盘 writer；内存队列满载后的 emergency 始终排在普通 FIFO 末尾落盘。
pub fn spawnSpoolWriter(
    spool: Arc<UdpRecordingSpool>,
    sink: Arc<QueuedUdpDatagramSink>,
    captureReceiver: Receiver<CapturedUdpDatagram>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("udp-recording-spool-writer".to_owned())
        .spawn(move || {
            while let Ok(captured) = captureReceiver.recv() {
                if let Err(detail) = spool.appendCapturedEvent(&captured) {
                    sink.retainAfterSpoolFailure(captured, &captureReceiver, detail);
                    spool.close();
                    return;
                }
            }
            if let Some(emergency) = sink.takeEmergency()
                && let Err(error) = spool.appendCapturedEvent(&emergency)
            {
                sink.retainAfterSpoolFailure(
                    emergency,
                    &captureReceiver,
                    format!("UDP spool emergency 保存失败：{error}"),
                );
            }
            spool.close();
        })
        .expect("创建 UDP spool 写入线程失败")
}

/// 启动顺序 spool writer 与事务消费者；服务 stop/restart 不会取消已经接受的帧。
fn startUdpRecording(context: UdpRecordingConsumerContext) -> tokio::task::JoinHandle<()> {
    let UdpRecordingConsumerContext {
        recording,
        processNameResolver,
        sink,
        spool,
        captureReceiver,
        mut shutdownReceiver,
        coordination,
        recordingUpdateLock,
    } = context;
    let writer = spawnSpoolWriter(Arc::clone(&spool), Arc::clone(&sink), captureReceiver);
    let (entrySender, mut entryReceiver) = mpsc::channel::<SpoolEntry>(1);
    let (acknowledgementSender, mut acknowledgementReceiver) =
        mpsc::channel::<SpoolAcknowledgementRequest>(1);
    let readerSpool = Arc::clone(&spool);
    let readerSink = Arc::clone(&sink);
    let reader = thread::Builder::new()
        .name("udp-recording-spool-reader".to_owned())
        .spawn(move || {
            loop {
                let entry = match readerSpool.readNext() {
                    Ok(Some(entry)) => entry,
                    Ok(None) => return,
                    Err(error) => {
                        readerSink.fail(format!("读取 UDP spool 失败：{error}"));
                        eprintln!("读取 UDP spool 失败：{error}");
                        return;
                    }
                };
                if entrySender.blocking_send(entry).is_err() {
                    readerSink.fail("UDP spool 事务消费者已关闭".to_owned());
                    return;
                }
                let Some(request) = acknowledgementReceiver.blocking_recv() else {
                    readerSink.fail("UDP spool 事务确认通道已关闭".to_owned());
                    return;
                };
                let result = readerSpool.acknowledge(request.acknowledgement);
                let failed = result.is_err();
                let errorDetail = result.as_ref().err().map(ToString::to_string);
                let _ = request.completion.send(result);
                if failed {
                    let error = errorDetail.expect("UDP spool 确认失败必须有错误详情");
                    readerSink.fail(format!("确认 UDP spool 失败：{error}"));
                    eprintln!("确认 UDP spool 失败：{error}");
                    return;
                }
            }
        })
        .expect("创建 UDP spool 读取线程失败");
    tokio::spawn(async move {
        let mut shutdownObserved = false;
        let mut processNames = HashMap::<u32, CachedProcessIdentity>::new();
        loop {
            tokio::select! {
                changed = shutdownReceiver.changed(), if !shutdownObserved => {
                    if changed.is_err() || *shutdownReceiver.borrow() {
                        shutdownObserved = true;
                        sink.close();
                    }
                }
                entry = entryReceiver.recv() => {
                    let Some(entry) = entry else { break; };
                    let acknowledgement = entry.acknowledgement();
                    let identity = SpoolIdentity {
                        processGeneration: entry.recordingProcessGeneration.clone(),
                        segmentId: entry.segmentId,
                        sequence: entry.sequence,
                    };
                    // 收包 epoch、三步事务提交和磁盘确认共用 clear 串行锁。clear 推进水位后，
                    // 旧积压只推进 spool，不再创建刚被用户删除的事务；新进程遗留帧始终重放。
                    let persistResult = {
                        let _recordingGuard = recordingUpdateLock.lock().await;
                        let shouldPersist = coordination.shouldPersist(
                            &entry.recordingProcessGeneration,
                            entry.recordingEpoch,
                        );
                        let alreadyCommitted = coordination.transactionAlreadyCommitted(&identity);
                        if shouldPersist && !alreadyCommitted {
                            let processName = resolveProcessName(
                                processNameResolver.as_ref(),
                                &mut processNames,
                                entry.event.processId,
                            );
                            persistUdpDatagram(&recording, processName, entry.event).await
                        } else {
                            Ok(())
                        }
                    };
                    match persistResult {
                        Ok(()) => {
                            if coordination.shouldPersist(
                                &entry.recordingProcessGeneration,
                                entry.recordingEpoch,
                            ) {
                                coordination.markTransactionCommitted(identity.clone());
                            }
                            let (completion, completed) = oneshot::channel();
                            if acknowledgementSender
                                .send(SpoolAcknowledgementRequest {
                                    acknowledgement,
                                    completion,
                                })
                                .await
                                .is_err()
                            {
                                break;
                            }
                            match completed.await {
                                Ok(Ok(())) => {
                                    coordination.markSpoolAcknowledged(&identity);
                                }
                                Ok(Err(_)) | Err(_) => break,
                            }
                        }
                        Err(error) => {
                            sink.fail(format!(
                                "UDP 事务提交失败：code={}, sequence={}",
                                error.code(), entry.sequence
                            ));
                            eprintln!(
                                "UDP 数据报录制失败：code={}, sequence={}, operation=udpDatagramRecord；正文保留在 {}",
                                error.code(),
                                entry.sequence,
                                spool.directory.display()
                            );
                            break;
                        }
                    }
                }
            }
        }
        drop(acknowledgementSender);
        let _ = tokio::task::spawn_blocking(move || reader.join()).await;
        let _ = tokio::task::spawn_blocking(move || writer.join()).await;
    })
}

/// 以严格顺序保存 UDP 包，供独立测试直接验证正文和双栈地址。
pub async fn recordUdpDatagram(
    recording: &RecordingSession,
    processName: Option<String>,
    event: UdpDatagramEvent,
) {
    if let Err(error) = persistUdpDatagram(recording, processName, event).await {
        eprintln!(
            "UDP 数据报录制失败：code={}, operation=udpDatagramRecord",
            error.code()
        );
    }
}

/// 以一秒生命周期缓存 PID 显示名；避免逐包枚举系统进程，同时定期刷新以防 PID 回收后误标新进程。
fn resolveProcessName(
    processNameResolver: &dyn UdpProcessNameResolver,
    cache: &mut HashMap<u32, CachedProcessIdentity>,
    processId: u32,
) -> Option<String> {
    let now = Instant::now();
    if let Some(cached) = cache.get(&processId)
        && now.duration_since(cached.resolvedAt) < processIdentityCacheLifetime
    {
        return cached.name.clone();
    }
    let name = processNameResolver.resolve(processId);
    cache.insert(
        processId,
        CachedProcessIdentity {
            resolvedAt: now,
            name: name.clone(),
        },
    );
    name
}

/// 创建一条逐包事务并保存对应方向的完整 payload；录制暂停返回成功且不创建事务。
async fn persistUdpDatagram(
    recording: &RecordingSession,
    processName: Option<String>,
    event: UdpDatagramEvent,
) -> Result<(), capture_core::CaptureError> {
    // WinDivert 观察到的是独立数据报，并非 SOCKS5 的 UDP ASSOCIATE 控制请求。
    // 按实际方向命名可避免高吞吐媒体流在界面中被误解为客户端持续重发同一请求。
    let method = match event.direction {
        UdpDatagramDirection::Up => "UDP SEND",
        UdpDatagramDirection::Down => "UDP RECEIVE",
    };
    let displayHost = if event.targetAddress.is_ipv6() {
        format!("[{}]", event.targetAddress.ip())
    } else {
        event.targetAddress.ip().to_string()
    };
    let transaction = BeginTransaction {
        protocol: TransactionProtocol::Tunnel,
        method: method.to_owned(),
        location: ResolvedLocation {
            protocol: "udp".to_owned(),
            host: event.targetAddress.ip().to_string(),
            port: event.targetAddress.port(),
            path: String::new(),
            query: String::new(),
            display: format!("udp://{displayHost}:{}", event.targetAddress.port()),
        },
        clientAddress: event.clientAddress.to_string(),
        clientProcessName: processName,
        clientProcessId: Some(event.processId),
        contentType: binaryContentType.to_owned(),
        startAtMilliseconds: event.capturedAtMilliseconds,
    };
    let transactionId = recording.beginTransaction(transaction).await?;
    let Some(transactionId) = transactionId else {
        return Ok(());
    };
    let side = match event.direction {
        UdpDatagramDirection::Up => MessageSide::Request,
        UdpDatagramDirection::Down => MessageSide::Response,
    };
    let payloadBytes = event.payload.len() as u64;
    let capturedAtMilliseconds = event.capturedAtMilliseconds;
    let packetAction = if event.modifications.is_empty() {
        capture_core::StreamPacketAction::Forward
    } else {
        capture_core::StreamPacketAction::Replace
    };
    let modifications = event
        .modifications
        .into_iter()
        .map(|modification| StreamPacketModification {
            offsetBytes: modification.offsetBytes,
            originalBytes: modification.originalBytes,
            modifiedBytes: modification.modifiedBytes,
        })
        .collect();
    recording
        .storeBody(
            &transactionId,
            side,
            BodyWrite {
                bytes: event.payload,
                originalBytes: payloadBytes,
                contentType: binaryContentType.to_owned(),
                encoding: binaryEncoding.to_owned(),
            },
        )
        .await?;
    recording
        .storeStreamPackets(
            &transactionId,
            side,
            vec![StreamPacket {
                sequence: 1,
                capturedAtMilliseconds,
                storedOffsetBytes: 0,
                storedBytes: payloadBytes as usize,
                originalBytes: payloadBytes,
                truncated: false,
                action: packetAction,
                modifications,
            }],
        )
        .await?;
    recording
        .commit(
            &transactionId,
            TransactionCompletion {
                statusCode: 0,
                endAtMilliseconds: capturedAtMilliseconds,
                contentType: binaryContentType.to_owned(),
            },
        )
        .await
}

/// 创建一个新的追加分段；分段编号固定宽度，目录枚举顺序与捕获顺序一致。
fn createSegment(directory: &Path, segmentId: u64) -> io::Result<SpoolSegment> {
    let path = directory.join(format!("{spoolFilePrefix}{segmentId:020}{spoolFileSuffix}"));
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)?;
    Ok(SpoolSegment {
        id: segmentId,
        cursorPath: cursorPathForSegment(&path),
        path,
        file: Some(file),
        length: 0,
        readOffset: 0,
    })
}

/// 返回分段唯一确认游标路径；游标不参与分段枚举，孤立游标不会伪造可读正文。
fn cursorPathForSegment(segmentPath: &Path) -> PathBuf {
    segmentPath.with_extension("ack")
}

/// 原子替换确认游标；临时文件完整写入后再替换，替换失败保留旧游标且不推进内存边界。
fn persistCursor(path: &Path, cursor: PersistedCursor) -> io::Result<()> {
    let temporaryPath = path.with_extension("ack.next");
    let bytes = serde_json::to_vec(&cursor).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("序列化 UDP spool 确认游标失败：{error}"),
        )
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporaryPath)?;
    file.write_all(&bytes)?;
    // 目录项原子替换只保证名称切换，不保证临时文件正文已进入稳定存储；先同步文件内容，
    // 再以 WRITE_THROUGH/父目录 fsync 提交名称，避免崩溃后出现空游标或跳过未提交正文。
    file.sync_all()?;
    drop(file);
    replaceFileAtomically(&temporaryPath, path)
}

/// 原子持久化各控制进程的 clear 水位；正文 spool 只有在该屏障后才允许跳过旧 epoch。
fn persistRecordingEpochs(
    dataDirectory: &Path,
    clearedEpochs: &BTreeMap<String, u64>,
    sealedGenerations: &std::collections::BTreeSet<String>,
) -> io::Result<()> {
    let path = dataDirectory.join(recordingEpochFileName);
    let temporaryPath = path.with_extension("json.next");
    let bytes = serde_json::to_vec(&PersistedRecordingEpochs {
        clearedEpochs: clearedEpochs.clone(),
        sealedGenerations: sealedGenerations.clone(),
    })
    .map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("序列化 UDP clear 水位失败：{error}"),
        )
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporaryPath)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    replaceFileAtomically(&temporaryPath, &path)
}

/// 在 Windows 使用同目录原子替换，在其他平台使用 rename；两条路径都不暴露半写游标。
/// RecordingSession 本身是进程内会话，因此这里只需抵御服务任务异常退出；逐包写穿透会把
/// 每个 UDP 数据报变成磁盘 barrier 并导致持续流量堆积，进程级退出则由 spool 正文完整重放。
#[cfg(windows)]
fn replaceFileAtomically(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };

    let sourceWide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destinationWide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(sourceWide.as_ptr()),
            PCWSTR(destinationWide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|error| io::Error::other(format!("原子替换 UDP spool 确认游标失败：{error}")))
    }
}

/// 非 Windows 平台在同一目录 rename，文件系统负责原子替换目标名称。
#[cfg(not(windows))]
fn replaceFileAtomically(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("UDP 原子替换目标缺少父目录"))?;
    File::open(parent)?.sync_all()
}

/// 加载并验证持久化游标；边界必须落在完整帧末尾且序号必须匹配，损坏时拒绝启动。
fn loadPersistedCursor(
    file: &mut File,
    cursorPath: &Path,
    recordingGeneration: &str,
    segmentId: u64,
    segmentLength: u64,
) -> io::Result<u64> {
    if !cursorPath.exists() {
        return Ok(0);
    }
    let bytes = std::fs::read(cursorPath)?;
    let cursor: PersistedCursor = serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("UDP spool 确认游标损坏：{error}"),
        )
    })?;
    if cursor.recordingGeneration != recordingGeneration {
        // RecordingSession 是进程内状态；控制进程重建后旧事务不存在，因此旧确认边界
        // 必须作废并从正文偏移 0 完整重放。服务在同一进程内重启仍共享代际并继续游标。
        std::fs::remove_file(cursorPath)?;
        return Ok(0);
    }
    if cursor.segmentId != segmentId || cursor.readOffset == 0 || cursor.readOffset > segmentLength
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "UDP spool 确认游标越界：segment={}, cursorSegment={}, offset={}, length={segmentLength}",
                segmentId, cursor.segmentId, cursor.readOffset
            ),
        ));
    }
    validateCursorBoundary(file, &cursor)?;
    Ok(cursor.readOffset)
}

/// 顺序扫描到确认位置并核对最后一帧序号，阻止损坏游标跳过尚未提交的正文。
fn validateCursorBoundary(file: &mut File, cursor: &PersistedCursor) -> io::Result<()> {
    let mut offset = 0_u64;
    let mut sequence = None;
    while offset < cursor.readOffset {
        file.seek(SeekFrom::Start(offset))?;
        let headerLength = readU32(file)?;
        let payloadLength = readU32(file)?;
        validateFrameLengths(headerLength, payloadLength)?;
        let endOffset = offset
            .checked_add(frameLengthBytes)
            .and_then(|value| value.checked_add(u64::from(headerLength)))
            .and_then(|value| value.checked_add(u64::from(payloadLength)))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "UDP spool 游标偏移溢出"))?;
        if endOffset > cursor.readOffset {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "UDP spool 确认游标未落在完整帧边界",
            ));
        }
        let mut headerBytes = vec![
            0_u8;
            usize::try_from(headerLength).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "UDP spool 头长度超出平台范围")
            })?
        ];
        file.read_exact(&mut headerBytes)?;
        let header: SpoolHeader = serde_json::from_slice(&headerBytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("UDP spool 头损坏：{error}"),
            )
        })?;
        sequence = Some(header.sequence);
        offset = endOffset;
    }
    if offset != cursor.readOffset || sequence != Some(cursor.sequence) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UDP spool 确认游标与帧序号不一致",
        ));
    }
    Ok(())
}

/// 恢复时删除已经完全确认但尚未来得及回收的分段；删除失败直接阻止新代际启动。
fn removeRecoveredSegment(segmentPath: &Path, cursorPath: &Path) -> io::Result<()> {
    std::fs::remove_file(segmentPath)?;
    if cursorPath.exists() {
        std::fs::remove_file(cursorPath)?;
    }
    Ok(())
}

/// 把上一版单文件 spool 原子迁移为首个分段；同时存在非空两种格式时拒绝猜测顺序。
fn migrateLegacySpool(directory: &Path) -> io::Result<()> {
    let legacyPath = directory.join(legacySpoolFileName);
    if !legacyPath.exists() {
        return Ok(());
    }
    let segmentPaths = discoverSegmentPaths(directory)?;
    let legacyLength = std::fs::metadata(&legacyPath)?.len();
    if !segmentPaths.is_empty() {
        if legacyLength == 0 {
            return std::fs::remove_file(legacyPath);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "同时发现非空旧版 UDP spool 与分段 spool，无法确定捕获顺序",
        ));
    }
    let target = directory.join(format!("{spoolFilePrefix}{:020}{spoolFileSuffix}", 1_u64));
    std::fs::rename(legacyPath, target)
}

/// 枚举并解析现有分段；未知文件不属于本模块，保持原样且不参与恢复。
/// 返回按编号升序排列的正文分段；非法名称表示持久目录损坏并阻止启动。
fn discoverSegmentPaths(directory: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let mut segments = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let fileName = entry.file_name();
        let fileName = fileName.to_string_lossy();
        let Some(number) = fileName
            .strip_prefix(spoolFilePrefix)
            .and_then(|value| value.strip_suffix(spoolFileSuffix))
        else {
            continue;
        };
        let segmentId = number.parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("UDP spool 分段名称非法：{fileName}"),
            )
        })?;
        segments.push((segmentId, entry.path()));
    }
    segments.sort_by_key(|(segmentId, _)| *segmentId);
    Ok(segments)
}

/// 清理原子替换中断留下的临时游标和没有正文分段的孤立游标，防止编号复用后误跳过新数据。
fn cleanupInterruptedCursorFiles(
    directory: &Path,
    segmentPaths: &[(u64, PathBuf)],
) -> io::Result<()> {
    let liveSegmentIds = segmentPaths
        .iter()
        .map(|(segmentId, _)| *segmentId)
        .collect::<std::collections::BTreeSet<_>>();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let fileName = entry.file_name();
        let fileName = fileName.to_string_lossy();
        let (number, alwaysRemove) = if let Some(number) = fileName
            .strip_prefix(spoolFilePrefix)
            .and_then(|value| value.strip_suffix(".ack.next"))
        {
            (number, true)
        } else if let Some(number) = fileName
            .strip_prefix(spoolFilePrefix)
            .and_then(|value| value.strip_suffix(".ack"))
        {
            (number, false)
        } else {
            continue;
        };
        let segmentId = number.parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("UDP spool 游标名称非法：{fileName}"),
            )
        })?;
        if alwaysRemove || !liveSegmentIds.contains(&segmentId) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// 在锁内标记队首回收并提取文件所有权；本函数不执行任何文件系统 I/O。
fn beginSegmentRetirement(state: &mut SpoolState) -> io::Result<SegmentRetirement> {
    if state.retiringSegmentId.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UDP spool 已有分段正在回收",
        ));
    }
    let segment = state
        .segments
        .front_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "UDP spool 分段队列为空"))?;
    let file = segment
        .file
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "UDP spool 分段文件已释放"))?;
    let retirement = SegmentRetirement {
        segmentId: segment.id,
        path: segment.path.clone(),
        cursorPath: segment.cursorPath.clone(),
        file,
    };
    state.retiringSegmentId = Some(segment.id);
    state.retirementBlocked = false;
    Ok(retirement)
}

/// 校验两阶段回收的队首身份；删除期间追加只允许改变队尾，队首变化属于状态损坏。
fn validateRetiringSegment(state: &SpoolState, segmentId: u64) -> io::Result<()> {
    if state.retiringSegmentId != Some(segmentId)
        || state.segments.front().map(|segment| segment.id) != Some(segmentId)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UDP spool 回收期间队首身份发生变化",
        ));
    }
    Ok(())
}

/// 在状态锁外复制恢复句柄、关闭原句柄并删除分段；共享冲突会返回可直接恢复的句柄。
fn retireSegmentFiles(
    retirement: SegmentRetirement,
    segmentRemover: &dyn SpoolSegmentRemover,
) -> Result<Option<io::Error>, (io::Error, File)> {
    let SegmentRetirement {
        path,
        cursorPath,
        file,
        ..
    } = retirement;
    let recoveryFile = match file.try_clone() {
        Ok(recoveryFile) => recoveryFile,
        Err(error) => return Err((error, file)),
    };
    drop(file);
    if let Err(error) = segmentRemover.remove(&path) {
        return Err((error, recoveryFile));
    }
    drop(recoveryFile);
    let cursorCleanupError = match std::fs::remove_file(cursorPath) {
        Ok(()) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => Some(error),
    };
    Ok(cursorCleanupError)
}

/// 读取 little-endian u32 字段；截断帧返回精确 I/O 错误。
fn readU32(file: &mut File) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    file.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

/// 扫描上次进程遗留的完整帧并恢复下一序号；任何半帧都作为显式初始化故障保留现场。
fn scanExistingFrames(
    file: &mut File,
) -> io::Result<(u64, u64, std::collections::BTreeSet<String>)> {
    let fileLength = file.metadata()?.len();
    let mut offset = 0_u64;
    let mut nextSequence = 1_u64;
    let mut processGenerations = std::collections::BTreeSet::new();
    while offset < fileLength {
        if fileLength - offset < frameLengthBytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("UDP spool 在偏移 {offset} 处存在截断帧头"),
            ));
        }
        file.seek(SeekFrom::Start(offset))?;
        let headerLength = readU32(file)?;
        let payloadLength = readU32(file)?;
        validateFrameLengths(headerLength, payloadLength)?;
        let endOffset = offset
            .checked_add(frameLengthBytes)
            .and_then(|value| value.checked_add(u64::from(headerLength)))
            .and_then(|value| value.checked_add(u64::from(payloadLength)))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "UDP spool 偏移溢出"))?;
        if endOffset > fileLength {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "UDP spool 在偏移 {offset} 处存在截断帧：end={endOffset}, file={fileLength}"
                ),
            ));
        }
        let mut headerBytes = vec![
            0_u8;
            usize::try_from(headerLength).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "UDP spool 头长度超出平台范围")
            })?
        ];
        file.read_exact(&mut headerBytes)?;
        let header: SpoolHeader = serde_json::from_slice(&headerBytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("UDP spool 头损坏：{error}"),
            )
        })?;
        if !header.recordingProcessGeneration.is_empty() {
            processGenerations.insert(header.recordingProcessGeneration.clone());
        }
        nextSequence = header
            .sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "UDP spool 序号已经耗尽"))?;
        offset = endOffset;
    }
    Ok((fileLength, nextSequence, processGenerations))
}

/// 在分配正文缓冲前验证帧边界；损坏 spool 只能形成显式故障，不能诱发巨量堆分配。
fn validateFrameLengths(headerLength: u32, payloadLength: u32) -> io::Result<()> {
    if headerLength == 0 || headerLength > maximumHeaderBytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("UDP spool 头长度非法：{headerLength}"),
        ));
    }
    if payloadLength > maximumPayloadBytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("UDP spool 正文长度非法：{payloadLength}"),
        ));
    }
    Ok(())
}
