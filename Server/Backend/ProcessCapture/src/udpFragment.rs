//! UDP IP 分片的有界顺序重组。
//!
//! WinDivert 的 NETWORK 层会分别交付 IP 分片；非首片没有 UDP 端口，不能独立完成进程归属。
//! 本模块只在独立 resolver 线程中运行，把同一方向、地址和 identification 的片段组合成原始
//! UDP 正文。容量或结构不一致会显式报错，禁止把不完整媒体/DNS 正文伪装成完整事务。

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, Instant},
};

use crate::{OriginalTarget, UdpDatagramDirection, UdpDatagramEvent};

const ipv4FragmentLifetime: Duration = Duration::from_secs(60);
const ipv6FragmentLifetime: Duration = Duration::from_secs(60);
const maximumFragmentGroups: usize = 1_024;
const maximumBufferedFragmentBytes: usize = 64 * 1_024 * 1_024;
const maximumBufferedFragmentObservations: usize = 8_192;
const maximumBufferedObservationBytes: usize = 64 * 1_024 * 1_024;

/// 唯一标识双栈 UDP 分片组；方向参与键，避免同地址 identification 复用时串组。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct UdpFragmentKey {
    sourceAddress: IpAddr,
    destinationAddress: IpAddr,
    identification: u32,
    outbound: bool,
}

/// 描述一个经 IP 长度边界裁剪后的 UDP 分片；`payloadOffset` 相对 UDP 头起点。
pub struct UdpFragmentPart {
    key: UdpFragmentKey,
    payloadOffset: usize,
    moreFragments: bool,
    fragmentPayload: Vec<u8>,
    packetBytes: usize,
    sourcePort: Option<u16>,
    destinationPort: Option<u16>,
    udpHeaderOffset: Option<usize>,
    packetPayloadOffset: usize,
}

/// 区分普通 UDP 包与分片；普通包继续走零重组开销的既有解析路径。
pub enum UdpPacketFragment {
    Whole,
    Fragment(UdpFragmentPart),
}

/// 首片按 owner/五元组完成的最终归属；未选进程也需记为 Ignored，才能安全消费后续片。
#[derive(Clone, Copy)]
pub enum UdpFragmentDisposition {
    Selected {
        target: OriginalTarget,
        direction: UdpDatagramDirection,
        clientAddress: SocketAddr,
        capturedAtMilliseconds: u64,
    },
    Ignored,
}

struct FragmentGroup<Observation> {
    pieces: BTreeMap<usize, Vec<u8>>,
    finalLength: Option<usize>,
    disposition: Option<UdpFragmentDisposition>,
    udpHeaderOffset: Option<usize>,
    packetCount: u64,
    packetBytes: u64,
    expiresAt: Instant,
    observations: VecDeque<Observation>,
    observationBytes: usize,
}

/// 完整数据报及其物理分片计数；指标按真实捕获包累计，事务正文只发布一次重组结果。
pub struct ReassembledUdpDatagram<Observation = ()> {
    pub event: Option<UdpDatagramEvent>,
    pub packetCount: u64,
    pub packetBytes: u64,
    /// 保留构成当前数据报的全部原始网络包；主动拦截模式据此执行唯一回注或整组丢弃。
    pub(crate) observations: VecDeque<Observation>,
}

/// 在单一 resolver 线程维护有界分片状态，不需要锁，也不会阻塞 WinDivert recv。
pub struct UdpFragmentAssembler<Observation = ()> {
    groups: HashMap<UdpFragmentKey, FragmentGroup<Observation>>,
    faultObservations: VecDeque<Observation>,
    bufferedBytes: usize,
    bufferedObservations: usize,
    bufferedObservationBytes: usize,
    fragmentLifetimeOverride: Option<Duration>,
}

impl<Observation> Default for UdpFragmentAssembler<Observation> {
    /// 创建生产重组器；双栈均不短于系统协议重组窗口，避免合法慢分片被提前判故障。
    fn default() -> Self {
        Self {
            groups: HashMap::new(),
            faultObservations: VecDeque::new(),
            bufferedBytes: 0,
            bufferedObservations: 0,
            bufferedObservationBytes: 0,
            fragmentLifetimeOverride: None,
        }
    }
}

impl UdpFragmentAssembler<()> {
    /// 创建使用显式超时的重组器；主要供确定性验证和特殊运行时约束复用同一状态机。
    pub fn withFragmentLifetime(lifetime: Duration) -> Self {
        Self {
            fragmentLifetimeOverride: Some(lifetime),
            ..Self::default()
        }
    }

    /// 接收不需要保留运行时副本的分片；协议单元测试与纯重组调用使用该入口。
    pub fn push(
        &mut self,
        part: UdpFragmentPart,
        disposition: Option<UdpFragmentDisposition>,
    ) -> Result<Option<ReassembledUdpDatagram<()>>, String> {
        let observationBytes = part.packetBytes;
        self.pushWithObservation(part, disposition, (), observationBytes)
    }
}

impl<Observation> UdpFragmentAssembler<Observation> {
    /// 接收一个分片及其唯一原始捕获副本，并在完整覆盖 `[0, finalLength)` 后返回原 UDP 正文。
    ///
    /// `disposition` 只允许首片提供；重叠不一致、长度溢出、容量耗尽或选中流超时均返回错误，
    /// 调用方必须把它升级为捕获完整性故障而不是继续生成截断事务。
    pub(crate) fn pushWithObservation(
        &mut self,
        part: UdpFragmentPart,
        disposition: Option<UdpFragmentDisposition>,
        observation: Observation,
        observationBytes: usize,
    ) -> Result<Option<ReassembledUdpDatagram<Observation>>, String> {
        self.pruneExpired()?;
        let selected = self.groups.get(&part.key).is_some_and(|group| {
            matches!(
                group.disposition,
                Some(UdpFragmentDisposition::Selected { .. })
            )
        }) || matches!(disposition, Some(UdpFragmentDisposition::Selected { .. }));
        let ignored = self.groups.get(&part.key).is_some_and(|group| {
            matches!(group.disposition, Some(UdpFragmentDisposition::Ignored))
        }) || matches!(disposition, Some(UdpFragmentDisposition::Ignored));
        if !self.groups.contains_key(&part.key)
            && self.groups.len() >= maximumFragmentGroups
            && !self.evictIgnoredGroup(None)
        {
            if ignored {
                eprintln!("UDP 分片组预算已满，忽略已确认未选流的新分片");
                return Ok(None);
            }
            return Err(format!(
                "选中或尚未归属的 UDP 分片组达到固定上限 {maximumFragmentGroups}"
            ));
        }
        let fragmentEnd = part
            .payloadOffset
            .checked_add(part.fragmentPayload.len())
            .ok_or_else(|| "UDP 分片偏移溢出".to_owned())?;
        let newBytes = if self
            .groups
            .get(&part.key)
            .and_then(|group| group.pieces.get(&part.payloadOffset))
            .is_some_and(|existing| existing == &part.fragmentPayload)
        {
            0
        } else {
            part.fragmentPayload.len()
        };
        while self.bufferedBytes.saturating_add(newBytes) > maximumBufferedFragmentBytes {
            if !self.evictIgnoredGroup(Some(part.key)) {
                if ignored {
                    eprintln!("UDP 分片正文预算已满，忽略已确认未选流的新分片");
                    return Ok(None);
                }
                return Err(format!(
                    "选中或尚未归属的 UDP 分片正文达到固定内存上限 {maximumBufferedFragmentBytes} 字节"
                ));
            }
        }
        while self.bufferedObservations >= maximumBufferedFragmentObservations
            || self
                .bufferedObservationBytes
                .saturating_add(observationBytes)
                > maximumBufferedObservationBytes
        {
            if !self.evictIgnoredGroup(Some(part.key)) {
                if ignored {
                    eprintln!("UDP 分片原始副本预算已满，忽略已确认未选流的新分片");
                    return Ok(None);
                }
                return Err("选中或尚未归属的 UDP 分片原始副本达到固定内存上限".to_owned());
            }
        }
        if let Some(group) = self.groups.get(&part.key)
            && let Err(detail) = validateNoConflictingOverlap(
                &group.pieces,
                part.payloadOffset,
                &part.fragmentPayload,
            )
        {
            if selected {
                return Err(detail);
            }
            self.removeGroup(part.key);
            eprintln!("忽略未选/未知 UDP 冲突分片：{detail}");
            return Ok(None);
        }
        if let Some(group) = self.groups.get(&part.key)
            && !part.moreFragments
            && group
                .finalLength
                .is_some_and(|length| length != fragmentEnd)
        {
            let detail = "同一 UDP 分片组出现冲突的末片长度".to_owned();
            if selected {
                return Err(detail);
            }
            self.removeGroup(part.key);
            eprintln!("忽略未选/未知 UDP 冲突末片：{detail}");
            return Ok(None);
        }
        let groupLifetime = self.fragmentLifetime(part.key);
        let group = self
            .groups
            .entry(part.key)
            .or_insert_with(|| FragmentGroup {
                pieces: BTreeMap::new(),
                finalLength: None,
                disposition: None,
                udpHeaderOffset: None,
                packetCount: 0,
                packetBytes: 0,
                expiresAt: Instant::now() + groupLifetime,
                observations: VecDeque::new(),
                observationBytes: 0,
            });
        if disposition.is_some() && part.payloadOffset != 0 {
            return Err("UDP 非首片携带了归属信息".to_owned());
        }
        if let Some(disposition) = disposition {
            group.disposition = Some(disposition);
            group.udpHeaderOffset = part.udpHeaderOffset;
        }
        if newBytes != 0 {
            self.bufferedBytes += newBytes;
            group
                .pieces
                .insert(part.payloadOffset, part.fragmentPayload);
        }
        group.packetCount += 1;
        group.packetBytes += part.packetBytes as u64;
        group.observations.push_back(observation);
        group.observationBytes += observationBytes;
        self.bufferedObservations += 1;
        self.bufferedObservationBytes += observationBytes;
        if !part.moreFragments {
            group.finalLength = Some(fragmentEnd);
        }
        if !isComplete(group) {
            return Ok(None);
        }
        let group = self.groups.remove(&part.key).expect("完整分片组必须仍存在");
        self.bufferedBytes -= group.pieces.values().map(Vec::len).sum::<usize>();
        self.bufferedObservations -= group.observations.len();
        self.bufferedObservationBytes -= group.observationBytes;
        match assembleGroup(group) {
            Ok(reassembled) => Ok(Some(reassembled)),
            Err((detail, mut observations)) => {
                self.faultObservations.append(&mut observations);
                Err(detail)
            }
        }
    }

    /// resolver 正常退出前验证需保留的流没有残留不完整正文；未知组也可能由迟到首片证明为选中。
    pub fn finish(&self) -> Result<(), String> {
        let incompleteRequired = self
            .groups
            .values()
            .filter(|group| !matches!(group.disposition, Some(UdpFragmentDisposition::Ignored)))
            .count();
        if incompleteRequired == 0 {
            Ok(())
        } else {
            Err(format!(
                "停止时仍有 {incompleteRequired} 个选中或尚未归属的 UDP 数据报缺少分片"
            ))
        }
    }

    /// 供 resolver 空闲 tick 主动检查选中分片超时；没有新包时也必须及时暴露缺片故障。
    pub fn pollExpired(&mut self) -> Result<(), String> {
        self.pruneExpired()
    }

    /// 导出故障前已经消费但尚未完成的全部原始分片，供跨服务代际按捕获序号重放。
    pub(crate) fn drainPendingObservations(&mut self) -> VecDeque<Observation> {
        let mut observations = std::mem::take(&mut self.faultObservations);
        for group in self.groups.values_mut() {
            observations.append(&mut group.observations);
        }
        observations
    }

    /// 返回当前地址族的协议重组窗口；测试覆盖可缩短时间，但生产值不得短于 IPv6 的 60 秒。
    fn fragmentLifetime(&self, key: UdpFragmentKey) -> Duration {
        self.fragmentLifetimeOverride
            .unwrap_or(match key.sourceAddress {
                IpAddr::V4(_) => ipv4FragmentLifetime,
                IpAddr::V6(_) => ipv6FragmentLifetime,
            })
    }

    /// 只丢弃过期的已确认未选分片；选中或未知组过期必须显式故障并跨代保留原始副本。
    fn pruneExpired(&mut self) -> Result<(), String> {
        let now = Instant::now();
        let expiredRequired = self.groups.values().any(|group| {
            group.expiresAt <= now
                && !matches!(group.disposition, Some(UdpFragmentDisposition::Ignored))
        });
        if expiredRequired {
            return Err("选中或尚未归属的 UDP 数据报在协议重组窗口内未收齐全部 IP 分片".to_owned());
        }
        let expiredKeys = self
            .groups
            .iter()
            .filter(|(_, group)| group.expiresAt <= now)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        for key in expiredKeys {
            self.removeGroup(key);
        }
        Ok(())
    }

    /// 只淘汰已由首片确认 Ignored 的最早组；身份未知组可能随后由乱序首片证明为选中，禁止丢弃。
    fn evictIgnoredGroup(&mut self, excludedKey: Option<UdpFragmentKey>) -> bool {
        let key = self
            .groups
            .iter()
            .filter(|(key, group)| {
                Some(**key) != excludedKey
                    && matches!(group.disposition, Some(UdpFragmentDisposition::Ignored))
            })
            .min_by_key(|(_, group)| group.expiresAt)
            .map(|(key, _)| *key);
        let Some(key) = key else {
            return false;
        };
        self.removeGroup(key);
        eprintln!("UDP 分片预算回收了已确认未选组");
        true
    }

    /// 删除单个组并准确归还正文预算。
    fn removeGroup(&mut self, key: UdpFragmentKey) {
        if let Some(group) = self.groups.remove(&key) {
            self.bufferedBytes -= group.pieces.values().map(Vec::len).sum::<usize>();
            self.bufferedObservations -= group.observations.len();
            self.bufferedObservationBytes -= group.observationBytes;
        }
    }
}

/// 解析 UDP IP 分片元数据；所有长度均以 IP 头声明边界为准，忽略捕获缓冲尾部填充。
pub fn inspectUdpFragment(packet: &[u8], outbound: bool) -> Result<UdpPacketFragment, String> {
    let version = packet
        .first()
        .ok_or_else(|| "数据包短于 IP 头".to_owned())?
        >> 4;
    match version {
        4 => inspectIpv4Fragment(packet, outbound),
        6 => inspectIpv6Fragment(packet, outbound),
        value => Err(format!("不支持的 IP 版本 {value}")),
    }
}

/// 解析 IPv4 fragment offset/MF 与 IP payload；未分片返回 Whole。
fn inspectIpv4Fragment(packet: &[u8], outbound: bool) -> Result<UdpPacketFragment, String> {
    if packet.len() < 20 {
        return Err("数据包短于 IPv4 头".to_owned());
    }
    let headerLength = usize::from(packet[0] & 0x0f) * 4;
    let totalLength = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if headerLength < 20 || totalLength < headerLength || totalLength > packet.len() {
        return Err("IPv4 长度字段超出捕获边界".to_owned());
    }
    if packet[9] != 17 {
        return Err("IP 分片不是 UDP".to_owned());
    }
    let fragment = u16::from_be_bytes([packet[6], packet[7]]);
    let offset = usize::from(fragment & 0x1fff) * 8;
    let moreFragments = fragment & 0x2000 != 0;
    if offset == 0 && !moreFragments {
        return Ok(UdpPacketFragment::Whole);
    }
    let sourceAddress = IpAddr::V4(Ipv4Addr::new(
        packet[12], packet[13], packet[14], packet[15],
    ));
    let destinationAddress = IpAddr::V4(Ipv4Addr::new(
        packet[16], packet[17], packet[18], packet[19],
    ));
    fragmentPart(
        UdpFragmentKey {
            sourceAddress,
            destinationAddress,
            identification: u32::from(u16::from_be_bytes([packet[4], packet[5]])),
            outbound,
        },
        offset,
        moreFragments,
        &packet[headerLength..totalLength],
        packet.len(),
        (offset == 0).then_some(0),
        headerLength,
    )
}

/// 解析 IPv6 扩展链中的 Fragment 头；没有 Fragment 头时返回 Whole。
fn inspectIpv6Fragment(packet: &[u8], outbound: bool) -> Result<UdpPacketFragment, String> {
    if packet.len() < 40 {
        return Err("数据包短于 IPv6 头".to_owned());
    }
    let payloadLength = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
    if payloadLength == 0 {
        return Err("IPv6 UDP jumbogram 缺少可验证的固定长度边界".to_owned());
    }
    let packetEnd = 40usize
        .checked_add(payloadLength)
        .filter(|end| *end <= packet.len())
        .ok_or_else(|| "IPv6 长度字段超出捕获边界".to_owned())?;
    let sourceAddress = IpAddr::V6(Ipv6Addr::from(
        <[u8; 16]>::try_from(&packet[8..24]).expect("IPv6 源地址长度已校验"),
    ));
    let destinationAddress = IpAddr::V6(Ipv6Addr::from(
        <[u8; 16]>::try_from(&packet[24..40]).expect("IPv6 目标地址长度已校验"),
    ));
    let mut nextHeader = packet[6];
    let mut offset = 40usize;
    loop {
        match nextHeader {
            17 => return Ok(UdpPacketFragment::Whole),
            0 | 43 | 60 => {
                ensureBytes(packetEnd, offset, 2)?;
                nextHeader = packet[offset];
                offset += (usize::from(packet[offset + 1]) + 1) * 8;
            }
            51 => {
                ensureBytes(packetEnd, offset, 2)?;
                nextHeader = packet[offset];
                offset += (usize::from(packet[offset + 1]) + 2) * 4;
            }
            44 => {
                ensureBytes(packetEnd, offset, 8)?;
                let fragment = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]);
                let payloadOffset = usize::from(fragment & 0xfff8);
                let fragmentPayload = &packet[offset + 8..packetEnd];
                let udpHeaderOffset = if payloadOffset == 0 {
                    Some(locatePostFragmentUdpHeader(
                        fragmentPayload,
                        packet[offset],
                    )?)
                } else {
                    None
                };
                return fragmentPart(
                    UdpFragmentKey {
                        sourceAddress,
                        destinationAddress,
                        identification: u32::from_be_bytes(
                            packet[offset + 4..offset + 8]
                                .try_into()
                                .expect("IPv6 分片标识长度已校验"),
                        ),
                        outbound,
                    },
                    payloadOffset,
                    fragment & 0x0001 != 0,
                    fragmentPayload,
                    packet.len(),
                    udpHeaderOffset,
                    offset + 8,
                );
            }
            _ => return Err("IPv6 扩展链不是 UDP".to_owned()),
        }
        if offset > packetEnd {
            return Err("IPv6 扩展头越过 payload 边界".to_owned());
        }
    }
}

/// 遍历 Fragment Header 之后属于 fragmentable part 的扩展链，定位真实 UDP 头。
/// RFC 7112 要求首片包含完整扩展链；首片内不足会作为损坏包显式报告。
fn locatePostFragmentUdpHeader(payload: &[u8], mut nextHeader: u8) -> Result<usize, String> {
    let mut offset = 0usize;
    loop {
        match nextHeader {
            17 => return Ok(offset),
            0 | 43 | 60 => {
                ensureBytes(payload.len(), offset, 2)?;
                nextHeader = payload[offset];
                offset += (usize::from(payload[offset + 1]) + 1) * 8;
            }
            51 => {
                ensureBytes(payload.len(), offset, 2)?;
                nextHeader = payload[offset];
                offset += (usize::from(payload[offset + 1]) + 2) * 4;
            }
            _ => return Err("IPv6 Fragment 后扩展链不是 UDP".to_owned()),
        }
        if offset > payload.len() {
            return Err("IPv6 Fragment 后扩展头越过首片边界".to_owned());
        }
    }
}

/// 构造已校验的分片；首片必须至少包含完整 UDP 头以提供端口和声明长度。
fn fragmentPart(
    key: UdpFragmentKey,
    payloadOffset: usize,
    moreFragments: bool,
    fragmentPayload: &[u8],
    packetBytes: usize,
    udpHeaderOffset: Option<usize>,
    packetPayloadOffset: usize,
) -> Result<UdpPacketFragment, String> {
    if moreFragments && !fragmentPayload.len().is_multiple_of(8) {
        return Err("非末尾 IP 分片长度不是 8 字节倍数".to_owned());
    }
    let (sourcePort, destinationPort) = if let Some(udpOffset) = udpHeaderOffset {
        if fragmentPayload.len() < udpOffset + 8 {
            return Err("UDP 首片短于 UDP 头".to_owned());
        }
        (
            Some(u16::from_be_bytes([
                fragmentPayload[udpOffset],
                fragmentPayload[udpOffset + 1],
            ])),
            Some(u16::from_be_bytes([
                fragmentPayload[udpOffset + 2],
                fragmentPayload[udpOffset + 3],
            ])),
        )
    } else {
        (None, None)
    };
    Ok(UdpPacketFragment::Fragment(UdpFragmentPart {
        key,
        payloadOffset,
        moreFragments,
        fragmentPayload: fragmentPayload.to_vec(),
        packetBytes,
        sourcePort,
        destinationPort,
        udpHeaderOffset,
        packetPayloadOffset,
    }))
}

/// 返回当前分片正文相对完整 IP fragmentable part 的偏移和原始包内偏移，供修改映射回物理包。
pub(crate) fn fragmentPayloadRange(part: &UdpFragmentPart) -> (usize, usize, usize) {
    (
        part.payloadOffset,
        part.packetPayloadOffset,
        part.fragmentPayload.len(),
    )
}

/// 返回首片内 UDP 头相对当前分片正文的偏移；非首片没有该字段。
pub(crate) fn fragmentUdpHeaderOffset(part: &UdpFragmentPart) -> Option<usize> {
    part.udpHeaderOffset
}

/// 返回零偏移首片中稳定的 fragmentable 前缀及源地址；长度不足或非首片时不暴露部分结果。
/// 运行上下文：变长 UDP 修改用它复制扩展头和传输层头，避免跨模块访问分片器私有状态。
pub(crate) fn fragmentPayloadPrefix(
    part: &UdpFragmentPart,
    prefixLength: usize,
) -> Option<(&[u8], IpAddr)> {
    (part.payloadOffset == 0 && part.fragmentPayload.len() >= prefixLength).then_some((
        &part.fragmentPayload[..prefixLength],
        part.key.sourceAddress,
    ))
}

/// 返回首片携带的五元组字段；非首片没有端口，调用方必须等待同组首片。
pub fn firstFragmentTuple(part: &UdpFragmentPart) -> Option<(IpAddr, u16, IpAddr, u16)> {
    Some((
        part.key.sourceAddress,
        part.sourcePort?,
        part.key.destinationAddress,
        part.destinationPort?,
    ))
}

/// 验证新片段不与既有范围发生不一致重叠；完全相同重传由调用方去重。
fn validateNoConflictingOverlap(
    pieces: &BTreeMap<usize, Vec<u8>>,
    offset: usize,
    bytes: &[u8],
) -> Result<(), String> {
    let end = offset + bytes.len();
    for (existingOffset, existing) in pieces {
        let existingEnd = *existingOffset + existing.len();
        if offset < existingEnd && *existingOffset < end {
            if *existingOffset == offset && existing.as_slice() == bytes {
                return Ok(());
            }
            return Err("UDP 分片存在重叠或冲突正文".to_owned());
        }
    }
    Ok(())
}

/// 判断片段是否无空洞覆盖完整长度且首片归属已经确定。
fn isComplete<Observation>(group: &FragmentGroup<Observation>) -> bool {
    let Some(finalLength) = group.finalLength else {
        return false;
    };
    if group.disposition.is_none() {
        return false;
    }
    let mut nextOffset = 0usize;
    for (offset, bytes) in &group.pieces {
        if *offset != nextOffset {
            return false;
        }
        nextOffset += bytes.len();
    }
    nextOffset == finalLength
}

/// 拼接完整 IP payload，按 UDP length 精确裁剪正文并构造一次录制事件。
fn assembleGroup<Observation>(
    group: FragmentGroup<Observation>,
) -> Result<ReassembledUdpDatagram<Observation>, (String, VecDeque<Observation>)> {
    let disposition = group.disposition.expect("完整分片组必须已有首片归属");
    if matches!(disposition, UdpFragmentDisposition::Ignored) {
        return Ok(ReassembledUdpDatagram {
            event: None,
            packetCount: group.packetCount,
            packetBytes: group.packetBytes,
            observations: group.observations,
        });
    }
    let mut udpBytes = Vec::with_capacity(group.finalLength.expect("完整分片组必须有末片"));
    for bytes in group.pieces.values() {
        udpBytes.extend_from_slice(bytes);
    }
    let Some(udpOffset) = group.udpHeaderOffset else {
        return Err((
            "完整 UDP 分片组缺少首片传输层偏移".to_owned(),
            group.observations,
        ));
    };
    if udpBytes.len() < udpOffset + 8 {
        return Err((
            "重组后的 UDP 数据报短于 UDP 头".to_owned(),
            group.observations,
        ));
    }
    let udpLength = usize::from(u16::from_be_bytes([
        udpBytes[udpOffset + 4],
        udpBytes[udpOffset + 5],
    ]));
    if udpLength < 8 || udpOffset + udpLength > udpBytes.len() {
        return Err((
            "重组后的 UDP length 超出完整 IP payload".to_owned(),
            group.observations,
        ));
    }
    let UdpFragmentDisposition::Selected {
        target,
        direction,
        clientAddress,
        capturedAtMilliseconds,
    } = disposition
    else {
        unreachable!("Ignored 已在前面返回")
    };
    Ok(ReassembledUdpDatagram {
        event: Some(UdpDatagramEvent {
            processId: target.processId,
            clientAddress,
            targetAddress: target.address,
            direction,
            payload: udpBytes[udpOffset + 8..udpOffset + udpLength].to_vec(),
            capturedAtMilliseconds,
        }),
        packetCount: group.packetCount,
        packetBytes: group.packetBytes,
        observations: group.observations,
    })
}

/// 校验扩展头固定字段仍位于 IPv6 payload 边界内。
fn ensureBytes(packetEnd: usize, offset: usize, length: usize) -> Result<(), String> {
    if offset
        .checked_add(length)
        .is_some_and(|end| end <= packetEnd)
    {
        Ok(())
    } else {
        Err("IPv6 扩展头越过 payload 边界".to_owned())
    }
}
