use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    mem::size_of,
    net::{Ipv4Addr, Ipv6Addr},
    ptr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP_STATE_CLOSED, MIB_TCP_STATE_DELETE_TCB, MIB_TCP_STATE_LISTEN,
        MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_LH, MIB_TCPROW_LH_0, MIB_TCPROW_OWNER_PID, SetTcpEntry,
        TCP_TABLE_OWNER_PID_CONNECTIONS,
    },
    Networking::WinSock::{AF_INET, AF_INET6},
};

use crate::ProcessCaptureError;

const noError: u32 = 0;
const errorInsufficientBuffer: u32 = 122;
const errorInvalidParameter: u32 = 87;
const errorNotFound: u32 = 1_168;
const tcpProtocol: u8 = 6;
const tcpResetFlag: u8 = 0x04;
const tcpAcknowledgementFlag: u8 = 0x10;
const ipv6HeaderBytes: usize = 40;
const tcpHeaderBytes: usize = 20;
const ipv6ResetLifetime: Duration = Duration::from_secs(30);

/// 标识一条需要在下一份真实报文上重置的 IPv6 TCP 连接。
///
/// Windows 没有与 `SetTcpEntry` 对等的 IPv6 删除接口，因此必须保留所有者表中的完整
/// 四元组，并在 NETWORK 层拿到真实序列号后构造 RST。带非零 scopeId 的记录无法与
/// WinDivert 接口元数据无歧义地互证，登记阶段会明确排除，避免跨接口误匹配。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Ipv6Connection {
    localAddress: Ipv6Addr,
    localPort: u16,
    remoteAddress: Ipv6Addr,
    remotePort: u16,
}

/// 保存等待真实 TCP 报文的 IPv6 重置请求；克隆实例共享同一集合，供控制线程和收包线程并发使用。
#[derive(Clone, Default)]
pub(crate) struct ConnectionResetState {
    pendingIpv6Connections: Arc<Mutex<BTreeMap<Ipv6Connection, Instant>>>,
}

/// 描述一次所有者表同步的可观察结果；IPv6 数量表示已登记等待 RST，不代表 TCB 已经删除。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ConnectionResetSummary {
    pub(crate) resetIpv4Connections: usize,
    pub(crate) pendingIpv6Connections: usize,
}

/// 保存由一份已确认 IPv6 报文推导出的双向 RST 原始字节。
///
/// 第一份报文沿原方向终止接收端，第二份报文反向终止发送端；调用方必须分别继承和反转
/// WinDivert 的方向元数据，并在发送前重算校验和。构造失败返回 `None`，原连接仍保留在待处理表中。
pub(crate) struct Ipv6ResetPackets {
    pub(crate) forward: Vec<u8>,
    pub(crate) reverse: Vec<u8>,
    connection: Ipv6Connection,
    deadline: Instant,
}

/// 判断 IPv4 所有者表记录是否属于需要重新接入 WinDivert 的活动连接。
///
/// 参数保持 Windows API 原始网络字节序；返回 `false` 表示监听、关闭或无远端记录不会交给
/// `SetTcpEntry`。本函数不执行系统调用，因此可用于验证过滤边界。
pub(crate) fn shouldResetIpv4Connection(
    row: &MIB_TCPROW_OWNER_PID,
    selectedProcessIds: &BTreeSet<u32>,
) -> bool {
    let localAddress = Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes());
    let remoteAddress = Ipv4Addr::from(row.dwRemoteAddr.to_ne_bytes());
    selectedProcessIds.contains(&row.dwOwningPid)
        && row.dwRemoteAddr != 0
        // 进程捕获的数据面明确排除 127/8；控制 API、公开代理与内部反射入口都依赖回环连接。
        // 热移除 PID 时若删除这些 TCB，当前 PUT 响应会先断链，界面便会误报一次失败。
        && !localAddress.is_loopback()
        && !remoteAddress.is_loopback()
        && isResettableState(row.dwState)
}

/// 判断 IPv6 所有者表记录是否需要进入待重置集合。
///
/// 未指定远端地址不对应可迁移的客户端会话；监听、关闭和已删除状态同样必须排除，防止把
/// 服务端监听器误当成长连接。返回值只表示需要登记，不表示连接已经关闭。
pub(crate) fn shouldQueueIpv6Connection(
    row: &MIB_TCP6ROW_OWNER_PID,
    selectedProcessIds: &BTreeSet<u32>,
) -> bool {
    let localAddress = Ipv6Addr::from(row.ucLocalAddr);
    let remoteAddress = Ipv6Addr::from(row.ucRemoteAddr);
    selectedProcessIds.contains(&row.dwOwningPid)
        && row.ucRemoteAddr != [0; 16]
        // ::1 与 IPv4 回环使用同一保护边界；它们不进入 WinDivert 捕获，也不得被热更新 RST 误伤。
        && !localAddress.is_loopback()
        && !remoteAddress.is_loopback()
        // NETWORK 包只携带接口索引而不携带 MIB 的本地/远端 scope 对；在无法证明映射
        // 唯一时拒绝登记，避免相同 link-local 四元组跨接口复用后误杀无关连接。
        && row.dwLocalScopeId == 0
        && row.dwRemoteScopeId == 0
        && isResettableState(row.dwState)
}

/// 统一判断 TCP 状态是否对应活动控制块，确保 IPv4 删除和 IPv6 RST 使用同一状态边界。
fn isResettableState(state: u32) -> bool {
    state != MIB_TCP_STATE_LISTEN as u32
        && state != MIB_TCP_STATE_CLOSED as u32
        && state != MIB_TCP_STATE_DELETE_TCB as u32
}

/// 读取指定地址族的 TCP 所有者表并复制到 Rust 所有内存。
///
/// Windows 可能在两次调用间扩张表，因此按返回大小重新分配；`rowBytes` 必须等于对应
/// `MIB_*ROW_OWNER_PID` 的大小。非容量错误和损坏的长度分别返回精确地址族及系统状态码。
fn readOwnerPidConnectionTable(
    addressFamily: u32,
    familyName: &'static str,
    rowBytes: usize,
) -> Result<(Vec<u32>, usize), ProcessCaptureError> {
    let mut requiredBytes = 0_u32;
    let initialStatus = unsafe {
        GetExtendedTcpTable(
            ptr::null_mut(),
            &mut requiredBytes,
            0,
            addressFamily,
            TCP_TABLE_OWNER_PID_CONNECTIONS,
            0,
        )
    };
    if initialStatus != errorInsufficientBuffer && initialStatus != noError {
        return Err(ProcessCaptureError::EnumerateConnections {
            addressFamily: familyName,
            status: initialStatus,
        });
    }

    loop {
        let wordCount = (requiredBytes as usize).div_ceil(size_of::<u32>());
        let mut tableWords = vec![0_u32; wordCount.max(1)];
        let status = unsafe {
            GetExtendedTcpTable(
                tableWords.as_mut_ptr().cast::<c_void>(),
                &mut requiredBytes,
                0,
                addressFamily,
                TCP_TABLE_OWNER_PID_CONNECTIONS,
                0,
            )
        };
        if status == errorInsufficientBuffer {
            continue;
        }
        if status != noError {
            return Err(ProcessCaptureError::EnumerateConnections {
                addressFamily: familyName,
                status,
            });
        }
        let entryCount = tableWords[0] as usize;
        let availableRows =
            requiredBytes.saturating_sub(size_of::<u32>() as u32) as usize / rowBytes;
        if entryCount > availableRows {
            return Err(ProcessCaptureError::InvalidConnectionTable(familyName));
        }
        return Ok((tableWords, entryCount));
    }
}

/// 将所有者表缓冲区复制为强类型记录；调用方已验证行数不会越过系统返回长度。
///
/// Windows 表头只有一个 `u32`，行首可能不满足 Rust 类型对齐要求，因此必须逐行执行
/// `read_unaligned`。失败边界由上层长度校验承担，本函数不访问表尾之外的字节。
fn copyOwnerRows<Row: Copy>(tableWords: &[u32], entryCount: usize) -> Vec<Row> {
    let rowsAddress = unsafe { tableWords.as_ptr().cast::<u8>().add(size_of::<u32>()) };
    (0..entryCount)
        .map(|index| unsafe {
            ptr::read_unaligned(rowsAddress.add(index * size_of::<Row>()).cast::<Row>())
        })
        .collect()
}

/// 读取 IPv4 TCP 所有者表；返回值不持有系统分配的指针。
fn readIpv4OwnerPidConnections() -> Result<Vec<MIB_TCPROW_OWNER_PID>, ProcessCaptureError> {
    let (tableWords, entryCount) =
        readOwnerPidConnectionTable(AF_INET as u32, "IPv4", size_of::<MIB_TCPROW_OWNER_PID>())?;
    Ok(copyOwnerRows(&tableWords, entryCount))
}

/// 读取 IPv6 TCP 所有者表；该 API 只负责枚举，Windows 不提供对应的 IPv6 TCB 删除函数。
fn readIpv6OwnerPidConnections() -> Result<Vec<MIB_TCP6ROW_OWNER_PID>, ProcessCaptureError> {
    let (tableWords, entryCount) =
        readOwnerPidConnectionTable(AF_INET6 as u32, "IPv6", size_of::<MIB_TCP6ROW_OWNER_PID>())?;
    Ok(copyOwnerRows(&tableWords, entryCount))
}

impl ConnectionResetState {
    /// 清除跨生命周期的 IPv6 待重置请求；停止或重新启动时不得把旧四元组应用到新连接。
    pub(crate) fn clear(&self) {
        self.pendingIpv6Connections
            .lock()
            .expect("IPv6 待重置连接锁中毒")
            .clear();
    }

    /// 按给定单调时钟清理过期请求并返回剩余数量；运行路径和回归测试共享同一过期语义。
    pub(crate) fn pruneExpired(&self, now: Instant) -> usize {
        let mut pending = self
            .pendingIpv6Connections
            .lock()
            .expect("IPv6 待重置连接锁中毒");
        pruneExpiredConnections(&mut pending, now);
        pending.len()
    }

    /// 登记所有选中 PID 的活动 IPv6 连接，并返回去重后的待处理总数。
    ///
    /// 控制线程在启动、停止前和 PID 热更新时调用；空闲连接保持登记，下一份真实 ACK 报文
    /// 到达后由 NETWORK resolver 注入双向 RST。该返回值绝不表示 TCB 已删除。
    pub(crate) fn queueIpv6Connections(
        &self,
        rows: &[MIB_TCP6ROW_OWNER_PID],
        selectedProcessIds: &BTreeSet<u32>,
    ) -> usize {
        self.pruneExpired(Instant::now());
        let mut pending = self
            .pendingIpv6Connections
            .lock()
            .expect("IPv6 待重置连接锁中毒");
        let now = Instant::now();
        let deadline = now + ipv6ResetLifetime;
        for connection in rows
            .iter()
            .filter(|row| shouldQueueIpv6Connection(row, selectedProcessIds))
            .map(ipv6ConnectionFromOwnerRow)
        {
            pending.insert(connection, deadline);
        }
        pending.len()
    }

    /// 以捕获报文的真实序列号构造双向 IPv6 RST，并原子移除对应待处理项。
    ///
    /// `outbound` 决定本地端点位于报文源还是目的；只有完整、未分片且带 ACK 的 IPv6 TCP
    /// 报文才能提供两个方向都可接受的序列号。解析失败或未命中时返回 `None`，请求继续等待。
    pub(crate) fn takeIpv6ResetPackets(
        &self,
        packet: &[u8],
        outbound: bool,
    ) -> Option<Ipv6ResetPackets> {
        let parsed = parseIpv6TcpPacket(packet)?;
        let connection = if outbound {
            Ipv6Connection {
                localAddress: parsed.sourceAddress,
                localPort: parsed.sourcePort,
                remoteAddress: parsed.destinationAddress,
                remotePort: parsed.destinationPort,
            }
        } else {
            Ipv6Connection {
                localAddress: parsed.destinationAddress,
                localPort: parsed.destinationPort,
                remoteAddress: parsed.sourceAddress,
                remotePort: parsed.sourcePort,
            }
        };
        self.pruneExpired(Instant::now());
        let mut pending = self
            .pendingIpv6Connections
            .lock()
            .expect("IPv6 待重置连接锁中毒");
        let deadline = pending.remove(&connection)?;
        Some(Ipv6ResetPackets {
            forward: buildIpv6ResetPacket(Ipv6ResetSpecification {
                sourceAddress: parsed.sourceAddress,
                destinationAddress: parsed.destinationAddress,
                sourcePort: parsed.sourcePort,
                destinationPort: parsed.destinationPort,
                sequenceNumber: parsed.sequenceNumber,
            }),
            reverse: buildIpv6ResetPacket(Ipv6ResetSpecification {
                sourceAddress: parsed.destinationAddress,
                destinationAddress: parsed.sourceAddress,
                sourcePort: parsed.destinationPort,
                destinationPort: parsed.sourcePort,
                sequenceNumber: parsed.acknowledgementNumber,
            }),
            connection,
            deadline,
        })
    }
}

impl Ipv6ResetPackets {
    /// 在双向 RST 注入失败时恢复仍有效的待处理项，等待下一份真实 ACK 再重试。
    ///
    /// 注入器会先尝试两个方向，因此恢复可能发生在其中一端已成功之后；重复 RST 对已关闭端点无害。
    /// 若控制线程已重新登记同一四元组，则保留较晚截止时间，避免旧请求缩短新请求生命周期。
    pub(crate) fn restoreAfterFailure(&self, state: &ConnectionResetState) {
        let now = Instant::now();
        if self.deadline <= now {
            return;
        }
        let mut pending = state
            .pendingIpv6Connections
            .lock()
            .expect("IPv6 待重置连接锁中毒");
        pruneExpiredConnections(&mut pending, now);
        pending
            .entry(self.connection)
            .and_modify(|deadline| *deadline = (*deadline).max(self.deadline))
            .or_insert(self.deadline);
    }
}

/// 删除已经超过观察窗口的 IPv6 请求，防止静默关闭后的四元组在端口复用时误杀新连接。
///
/// `now` 由调用方一次性读取，保证同批记录使用一致边界；严格保留截止时刻仍未到达的项。
fn pruneExpiredConnections(pending: &mut BTreeMap<Ipv6Connection, Instant>, now: Instant) {
    pending.retain(|_, deadline| *deadline > now);
}

/// 把 Windows 所有者表的网络字节序端点转换为稳定连接键。
fn ipv6ConnectionFromOwnerRow(row: &MIB_TCP6ROW_OWNER_PID) -> Ipv6Connection {
    Ipv6Connection {
        localAddress: Ipv6Addr::from(row.ucLocalAddr),
        localPort: ownerTablePort(row.dwLocalPort),
        remoteAddress: Ipv6Addr::from(row.ucRemoteAddr),
        remotePort: ownerTablePort(row.dwRemotePort),
    }
}

/// 读取 MIB 所有者表低 16 位中的网络字节序端口；高 16 位由系统保留且不得参与比较。
fn ownerTablePort(port: u32) -> u16 {
    u16::from_be(port as u16)
}

struct ParsedIpv6TcpPacket {
    sourceAddress: Ipv6Addr,
    destinationAddress: Ipv6Addr,
    sourcePort: u16,
    destinationPort: u16,
    sequenceNumber: u32,
    acknowledgementNumber: u32,
}

/// 汇总一份最小 IPv6 RST 的端点与序列号，避免构造函数依赖易错的位置参数顺序。
struct Ipv6ResetSpecification {
    sourceAddress: Ipv6Addr,
    destinationAddress: Ipv6Addr,
    sourcePort: u16,
    destinationPort: u16,
    sequenceNumber: u32,
}

/// 解析可用于可靠 RST 的 IPv6 TCP 报文，支持常见扩展头并拒绝分片、RST 及无 ACK 报文。
///
/// 分片可能缺少完整 TCP 头，无 ACK 报文也无法推导反向可接受序列号；两者均返回 `None`，
/// 待处理连接会等待下一份完整报文，而不是注入无法验证的伪 RST。
fn parseIpv6TcpPacket(packet: &[u8]) -> Option<ParsedIpv6TcpPacket> {
    if packet.len() < ipv6HeaderBytes || packet[0] >> 4 != 6 {
        return None;
    }
    let sourceAddress = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[8..24]).ok()?);
    let destinationAddress = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[24..40]).ok()?);
    let mut nextHeader = packet[6];
    let mut offset = ipv6HeaderBytes;
    while nextHeader != tcpProtocol {
        match nextHeader {
            0 | 43 | 60 => {
                let header = packet.get(offset..offset + 2)?;
                nextHeader = header[0];
                offset = offset.checked_add((usize::from(header[1]) + 1) * 8)?;
            }
            51 => {
                let header = packet.get(offset..offset + 2)?;
                nextHeader = header[0];
                offset = offset.checked_add((usize::from(header[1]) + 2) * 4)?;
            }
            44 => return None,
            _ => return None,
        }
    }
    let tcp = packet.get(offset..offset + tcpHeaderBytes)?;
    let flags = tcp[13];
    if flags & tcpAcknowledgementFlag == 0 || flags & tcpResetFlag != 0 {
        return None;
    }
    Some(ParsedIpv6TcpPacket {
        sourceAddress,
        destinationAddress,
        sourcePort: u16::from_be_bytes([tcp[0], tcp[1]]),
        destinationPort: u16::from_be_bytes([tcp[2], tcp[3]]),
        sequenceNumber: u32::from_be_bytes(tcp[4..8].try_into().ok()?),
        acknowledgementNumber: u32::from_be_bytes(tcp[8..12].try_into().ok()?),
    })
}

/// 构造最小 IPv6 TCP RST；校验和由携带真实接口元数据的 WinDivert 包在发送前统一计算。
fn buildIpv6ResetPacket(specification: Ipv6ResetSpecification) -> Vec<u8> {
    let mut packet = vec![0_u8; ipv6HeaderBytes + tcpHeaderBytes];
    packet[0] = 0x60;
    packet[4..6].copy_from_slice(&(tcpHeaderBytes as u16).to_be_bytes());
    packet[6] = tcpProtocol;
    packet[7] = 64;
    packet[8..24].copy_from_slice(&specification.sourceAddress.octets());
    packet[24..40].copy_from_slice(&specification.destinationAddress.octets());
    packet[40..42].copy_from_slice(&specification.sourcePort.to_be_bytes());
    packet[42..44].copy_from_slice(&specification.destinationPort.to_be_bytes());
    packet[44..48].copy_from_slice(&specification.sequenceNumber.to_be_bytes());
    packet[52] = 0x50;
    packet[53] = tcpResetFlag;
    packet
}

/// 同步删除选中 PID 的 IPv4 TCB；Windows 会立即使对应客户端观察到连接中断。
///
/// 已在枚举与删除之间自然消失的记录按竞态成功处理；枚举、权限或其它系统错误精确返回。
pub(crate) fn resetIpv4Connections(
    selectedProcessIds: &BTreeSet<u32>,
) -> Result<usize, ProcessCaptureError> {
    let ipv4Rows = readIpv4OwnerPidConnections()?;
    let mut resetIpv4Connections = 0_usize;
    for row in ipv4Rows
        .iter()
        .filter(|row| shouldResetIpv4Connection(row, selectedProcessIds))
    {
        let deleteRow = MIB_TCPROW_LH {
            Anonymous: MIB_TCPROW_LH_0 {
                dwState: MIB_TCP_STATE_DELETE_TCB as u32,
            },
            dwLocalAddr: row.dwLocalAddr,
            dwLocalPort: row.dwLocalPort,
            dwRemoteAddr: row.dwRemoteAddr,
            dwRemotePort: row.dwRemotePort,
        };
        let status = unsafe { SetTcpEntry(&deleteRow) };
        if status == noError {
            resetIpv4Connections += 1;
        } else if status != errorInvalidParameter && status != errorNotFound {
            return Err(ProcessCaptureError::ResetConnection {
                addressFamily: "IPv4",
                processId: row.dwOwningPid,
                status,
            });
        }
    }
    Ok(resetIpv4Connections)
}

/// 删除选中 PID 的 IPv4 TCB，并把 IPv6 连接登记为基于真实报文的 RST 请求。
///
/// Windows `SetTcpEntry` 仅支持 IPv4；IPv6 若直接使用任意序列号会被 TCP 栈拒绝。因此本函数
/// 返回的 IPv6 数量只表示已经登记，NETWORK resolver 后续发送成功才表示完成。枚举、权限或
/// `SetTcpEntry` 系统错误会直接返回，调用方不得伪造成功。
pub(crate) fn resetExistingConnections(
    selectedProcessIds: &BTreeSet<u32>,
    resetState: &ConnectionResetState,
) -> Result<ConnectionResetSummary, ProcessCaptureError> {
    let resetIpv4Connections = resetIpv4Connections(selectedProcessIds)?;
    let ipv6Rows = readIpv6OwnerPidConnections()?;
    Ok(ConnectionResetSummary {
        resetIpv4Connections,
        pendingIpv6Connections: resetState.queueIpv6Connections(&ipv6Rows, selectedProcessIds),
    })
}
