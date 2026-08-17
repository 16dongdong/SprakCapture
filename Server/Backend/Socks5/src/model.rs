use std::{
    fmt,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
use serde::Serialize;

const capturedBytesChunkSize: usize = 4 * 1024;

/// 标识同一数据面实例中的录制代际；清空录制会推进代际，使旧队列事件不可复活。
#[derive(Clone)]
pub struct CaptureGeneration {
    value: Arc<AtomicU64>,
}

impl CaptureGeneration {
    /// 创建从零开始的单实例代际；注册表与持有句柄的控制生命周期共同推进清空水位。
    pub(crate) fn new() -> Self {
        Self {
            value: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 返回当前录制代际；投影任务在每次消费事件前据此拒绝清空前快照。
    pub fn current(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    /// 判断两个句柄是否属于同一数据面周期；生命周期清理据此避免删除后来重启实例的水位。
    pub fn sameInstance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.value, &other.value)
    }

    /// 推进到下一代并返回新值；会话表在正常清空时持写锁，停止窗口可直接推进以拒绝所有旧队列事件。
    pub fn advance(&self) -> u64 {
        self.value.fetch_add(1, Ordering::AcqRel) + 1
    }
}

/// 维护可注入的共享镜像预算；生产实例使用平台最大值，显式小预算只服务边界测试。
pub(crate) struct CapturedBytesBudget {
    maximumBytes: usize,
    usedBytes: AtomicUsize,
}

impl CapturedBytesBudget {
    /// 创建固定上限预算；零上限允许测试和禁用场景只记流量长度而不保存正文。
    pub(crate) fn new(maximumBytes: usize) -> Self {
        Self {
            maximumBytes,
            usedBytes: AtomicUsize::new(0),
        }
    }

    /// 原子预留最多 requestedBytes；并发方向共享同一上限，返回实际获批字节数。
    fn reserve(&self, requestedBytes: usize) -> usize {
        let mut usedBytes = self.usedBytes.load(Ordering::Acquire);
        loop {
            let reservedBytes = requestedBytes.min(self.maximumBytes.saturating_sub(usedBytes));
            if reservedBytes == 0 {
                return 0;
            }
            match self.usedBytes.compare_exchange_weak(
                usedBytes,
                usedBytes + reservedBytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return reservedBytes,
                Err(currentBytes) => usedBytes = currentBytes,
            }
        }
    }

    /// 归还已释放缓冲区的实际字节；调用方只能归还此前成功预留的容量。
    fn release(&self, releasedBytes: usize) {
        let previousBytes = self.usedBytes.fetch_sub(releasedBytes, Ordering::AcqRel);
        debug_assert!(previousBytes >= releasedBytes);
    }
}

struct CapturedBytesInner {
    storage: RwLock<CapturedBytesStorage>,
    budget: Option<Arc<CapturedBytesBudget>>,
}

/// 以固定容量块保存完整原始流；reservedBytes 对应真实载荷容量而不是逻辑长度。
struct CapturedBytesStorage {
    chunks: Vec<CapturedBytesChunk>,
    length: usize,
    reservedBytes: usize,
}

/// 保存一个已经从共享预算完整预留的定长块；length 表示当前写入范围。
struct CapturedBytesChunk {
    bytes: Box<[u8]>,
    length: usize,
}

impl Drop for CapturedBytesInner {
    /// 在最后一个事件或注册表引用释放时归还预算，慢订阅者持有正文期间不会低估实际内存。
    fn drop(&mut self) {
        if let Some(budget) = self.budget.as_ref() {
            budget.release(self.storage.get_mut().reservedBytes);
        }
    }
}

/// 保存不会因事件快照克隆而复制的完整字节流；公开序列化始终由 SessionSnapshot 跳过。
#[derive(Clone)]
pub struct CapturedBytes {
    inner: Arc<CapturedBytesInner>,
}

impl CapturedBytes {
    /// 创建绑定实例预算的空镜像；仅注册表在创建活动会话时调用。
    pub(crate) fn withBudget(budget: Arc<CapturedBytesBudget>) -> Self {
        Self {
            inner: Arc::new(CapturedBytesInner {
                storage: RwLock::new(CapturedBytesStorage {
                    chunks: Vec::new(),
                    length: 0,
                    reservedBytes: 0,
                }),
                budget: Some(budget),
            }),
        }
    }

    /// 在注入的单方向上限和实例预算内追加载荷，返回本次实际保存的字节数。
    /// 生产调用的两个上限均为平台最大值，固定块仅用于避免 Vec 扩容时复制已录制正文。
    pub(crate) fn append(&self, payload: &[u8], streamLimit: usize) -> usize {
        let Some(budget) = self.inner.budget.as_ref() else {
            return 0;
        };
        let mut storage = self.inner.storage.write();
        let maximumWriteBytes = payload
            .len()
            .min(streamLimit.saturating_sub(storage.length));
        let mut writtenBytes = 0;
        while writtenBytes < maximumWriteBytes {
            if storage
                .chunks
                .last()
                .is_none_or(|chunk| chunk.length == chunk.bytes.len())
            {
                let streamCapacity = streamLimit.saturating_sub(storage.reservedBytes);
                let requestedCapacity = capturedBytesChunkSize.min(streamCapacity);
                let reservedCapacity = budget.reserve(requestedCapacity);
                if reservedCapacity == 0 {
                    break;
                }
                storage.chunks.push(CapturedBytesChunk {
                    bytes: vec![0; reservedCapacity].into_boxed_slice(),
                    length: 0,
                });
                storage.reservedBytes += reservedCapacity;
            }

            let chunk = storage.chunks.last_mut().expect("刚创建的正文镜像块");
            let writableBytes =
                (chunk.bytes.len() - chunk.length).min(maximumWriteBytes - writtenBytes);
            let chunkEnd = chunk.length + writableBytes;
            chunk.bytes[chunk.length..chunkEnd]
                .copy_from_slice(&payload[writtenBytes..writtenBytes + writableBytes]);
            chunk.length = chunkEnd;
            storage.length += writableBytes;
            writtenBytes += writableBytes;
        }
        writtenBytes
    }

    /// 返回当前镜像长度；诊断和预算测试不复制正文。
    pub fn len(&self) -> usize {
        self.inner.storage.read().length
    }

    /// 返回镜像是否为空；调用方据此跳过没有正文的独立存储请求。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 复制当前完整原始流供 capture-core 接管；复制只发生在限频投影点。
    pub fn toVec(&self) -> Vec<u8> {
        let storage = self.inner.storage.read();
        let mut bytes = Vec::with_capacity(storage.length);
        for chunk in &storage.chunks {
            bytes.extend_from_slice(&chunk.bytes[..chunk.length]);
        }
        bytes
    }
}

impl Default for CapturedBytes {
    /// 创建不绑定预算的空值，用于终态释放后的历史快照和确定性测试夹具。
    fn default() -> Self {
        Self {
            inner: Arc::new(CapturedBytesInner {
                storage: RwLock::new(CapturedBytesStorage {
                    chunks: Vec::new(),
                    length: 0,
                    reservedBytes: 0,
                }),
                budget: None,
            }),
        }
    }
}

impl From<Vec<u8>> for CapturedBytes {
    /// 创建不绑定实例预算的确定字节夹具；生产流量必须使用 withBudget。
    fn from(bytes: Vec<u8>) -> Self {
        let length = bytes.len();
        let chunks = if bytes.is_empty() {
            Vec::new()
        } else {
            vec![CapturedBytesChunk {
                bytes: bytes.into_boxed_slice(),
                length,
            }]
        };
        Self {
            inner: Arc::new(CapturedBytesInner {
                storage: RwLock::new(CapturedBytesStorage {
                    chunks,
                    length,
                    reservedBytes: 0,
                }),
                budget: None,
            }),
        }
    }
}

impl PartialEq for CapturedBytes {
    /// 比较两个镜像的当前字节；指向同一缓冲区时直接返回，避免不必要复制。
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner) || self.toVec() == other.toVec()
    }
}

impl Eq for CapturedBytes {}

/// 表示一段已经成功写向对端的原始流；偏移指向同方向的完整正文镜像。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedPacket {
    pub direction: TrafficDirection,
    pub sequence: u64,
    pub capturedAtMilliseconds: u64,
    pub storedOffsetBytes: usize,
    pub storedBytes: usize,
    pub originalBytes: u64,
    pub modifications: Vec<plugin_host::WireByteModification>,
}

#[derive(Eq, PartialEq)]
struct CapturedPacketStorage {
    nextUpSequence: u64,
    nextDownSequence: u64,
    packets: Vec<CapturedPacket>,
}

/// 以共享完整索引记录转发片段，SessionSnapshot 的高频克隆只复制 Arc，不复制整个片段列表。
#[derive(Clone)]
pub struct CapturedPacketList {
    storage: Arc<RwLock<CapturedPacketStorage>>,
}

/// 描述单个已录制转发片段的完整写入信息；索引层据此原子分配方向序号并保存元数据。
pub(crate) struct CapturedPacketWrite {
    pub direction: TrafficDirection,
    pub capturedAtMilliseconds: u64,
    pub storedOffsetBytes: usize,
    pub storedBytes: usize,
    pub originalBytes: u64,
    pub modifications: Vec<plugin_host::WireByteModification>,
}

impl CapturedPacketList {
    /// 创建完整片段索引；每个成功转发的非空片段都必须有可追溯的偏移和长度。
    pub(crate) fn new() -> Self {
        Self {
            storage: Arc::new(RwLock::new(CapturedPacketStorage {
                nextUpSequence: 1,
                nextDownSequence: 1,
                packets: Vec::new(),
            })),
        }
    }

    /// 记录一个已录制字节区间；未录制任何字节的片段没有可查看载荷，因此不进入索引。
    pub(crate) fn append(&self, packet: CapturedPacketWrite) {
        let CapturedPacketWrite {
            direction,
            capturedAtMilliseconds,
            storedOffsetBytes,
            storedBytes,
            originalBytes,
            modifications,
        } = packet;
        if storedBytes == 0 {
            return;
        }
        let mut storage = self.storage.write();
        // 请求和响应在界面中属于两棵独立子树，序号必须分别递增；共享计数会让首个响应显示成总流量中的第 N 包。
        let sequence = match direction {
            TrafficDirection::Up => {
                let sequence = storage.nextUpSequence;
                storage.nextUpSequence = storage.nextUpSequence.saturating_add(1);
                sequence
            }
            TrafficDirection::Down => {
                let sequence = storage.nextDownSequence;
                storage.nextDownSequence = storage.nextDownSequence.saturating_add(1);
                sequence
            }
        };
        storage.packets.push(CapturedPacket {
            direction,
            sequence,
            capturedAtMilliseconds,
            storedOffsetBytes,
            storedBytes,
            originalBytes,
            modifications,
        });
    }

    /// 返回指定方向的稳定快照；调用方在会话终态投影时复制一次，避免把锁带入录制 I/O。
    pub fn forDirection(&self, direction: TrafficDirection) -> Vec<CapturedPacket> {
        self.storage
            .read()
            .packets
            .iter()
            .filter(|packet| packet.direction == direction)
            .cloned()
            .collect()
    }
}

impl Default for CapturedPacketList {
    /// 创建不包含任何录制索引的空值，用于 clear、归档和终态释放后的资源回收。
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for CapturedPacketList {
    /// 会话快照比较优先复用同一索引；不同索引仅在测试或恢复路径按完整元数据做值比较。
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.storage, &other.storage) || *self.storage.read() == *other.storage.read()
    }
}

impl Eq for CapturedPacketList {}

/// 定义会话对外发布的生命周期状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    Negotiating,
    Authenticating,
    Connecting,
    Binding,
    UdpAssociating,
    Relaying,
    Closed,
    Failed,
}

/// 定义流量累计方向；上行表示客户端到目标，下行表示目标到客户端。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrafficDirection {
    Up,
    Down,
}

/// 标记 SOCKS5 会话经首段协议识别后的应用层处理路径。
///
/// 运行上下文：该值只服务于进程内事务投影，不序列化到会话控制接口；HTTP/HTTPS 已由专用处理器录制，原始 TCP/UDP 则保留流片段索引。
/// 参数：无；枚举值由 CONNECT 分类器或 UDP 命令分派写入。
/// 失败语义：Undetermined 表示尚未获得首段字节，投影器不得提前创建原始流事务。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionApplicationProtocol {
    Undetermined,
    Tcp,
    /// 已确认 TLS ClientHello 但未进入本地解密链路；数据仍按原始 TCP 字节流转发。
    Tls,
    Udp,
    Http,
    Https,
}

/// 保存跨线程读取的不可变会话状态，控制面序列化时不接触网络对象。
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub sessionId: String,
    pub clientAddress: String,
    pub username: String,
    pub command: String,
    pub targetAddress: String,
    pub state: SessionState,
    pub bytesUp: u64,
    pub bytesDown: u64,
    pub createdAtMilliseconds: u64,
    pub updatedAtMilliseconds: u64,
    pub closedAtMilliseconds: u64,
    pub errorMessage: String,
    /// 仅供后台事务投影区分原始流与已解码 HTTP/HTTPS，外部会话快照不暴露内部分类细节。
    #[serde(skip)]
    pub applicationProtocol: SessionApplicationProtocol,
    /// 标识会话创建时的数据面录制代际；只供进程内投影清空水位判断。
    #[serde(skip)]
    pub captureGeneration: u64,
    /// 保存客户端到目标的完整原始流；该字段只供进程内录制桥消费，不进入控制面会话列表。
    #[serde(skip)]
    pub capturedBytesUp: CapturedBytes,
    /// 保存目标到客户端的完整原始流；公开正文必须通过 capture-core 的独立端点读取。
    #[serde(skip)]
    pub capturedBytesDown: CapturedBytes,
    /// 保存两侧原始流的完整片段索引；每个条目只引用 capturedBytes 的一段范围，不复制报文字节。
    #[serde(skip)]
    pub capturedPackets: CapturedPacketList,
}

impl fmt::Debug for SessionSnapshot {
    /// 输出不含原始报文字节的诊断视图；日志只保留长度，正文必须经受控正文端点读取。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionSnapshot")
            .field("sessionId", &self.sessionId)
            .field("clientAddress", &self.clientAddress)
            .field("username", &self.username)
            .field("command", &self.command)
            .field("targetAddress", &self.targetAddress)
            .field("state", &self.state)
            .field("bytesUp", &self.bytesUp)
            .field("bytesDown", &self.bytesDown)
            .field("createdAtMilliseconds", &self.createdAtMilliseconds)
            .field("updatedAtMilliseconds", &self.updatedAtMilliseconds)
            .field("closedAtMilliseconds", &self.closedAtMilliseconds)
            .field("errorMessage", &self.errorMessage)
            .field("captureGeneration", &self.captureGeneration)
            .field("capturedBytesUp", &self.capturedBytesUp.len())
            .field("capturedBytesDown", &self.capturedBytesDown.len())
            .finish()
    }
}

/// 发布会话创建、更新和关闭事件；事件携带同一时刻的完整快照。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    pub eventType: String,
    pub snapshot: SessionSnapshot,
}

/// 提供控制面一次读取所需的数据面快照。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSnapshot {
    pub boundAddress: SocketAddr,
    pub sessions: Vec<SessionSnapshot>,
    pub metrics: ServiceMetrics,
}

/// 返回一次数据面停止后的最终快照与独立错误；即使任务异常也保留已归一化会话历史。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStopOutcome {
    pub snapshot: ServerSnapshot,
    pub errorMessage: Option<String>,
}

/// 保存服务生命周期累计指标；字段与桌面控制协议保持一致。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceMetrics {
    pub acceptedConnections: u64,
    pub activeConnections: u64,
    pub failedConnections: u64,
    pub bytesUp: u64,
    pub bytesDown: u64,
    pub udpPacketsUp: u64,
    pub udpPacketsDown: u64,
    pub droppedUdpPackets: u64,
}

/// 返回 Unix 毫秒时间戳；系统时间早于纪元时返回零并保留单调非负契约。
pub fn currentTimeMilliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}
