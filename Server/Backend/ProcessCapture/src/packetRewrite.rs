use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use thiserror::Error;

use crate::{
    CaptureFlowTable, NetworkInterface, OriginalTarget, UdpDatagramDirection, UdpDatagramEvent,
    flowTable::{FragmentAction, FragmentKey, FragmentLookup, FragmentRecordResult},
};

/// 描述数据包是否绕过、被重定向到本地代理，或从代理回复恢复为原目标身份。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketDirection {
    Bypass,
    /// UDP 未命中选中进程时原样回注；命中时由统一封包数据面决定替换、丢弃或放行。
    ObservedUp(OriginalTarget),
    ObservedDown(OriginalTarget),
    Blocked(OriginalTarget),
    Redirected {
        original: OriginalTarget,
        proxyAddress: IpAddr,
        reflectedPort: u16,
    },
    Restored(OriginalTarget, Option<NetworkInterface>),
}

/// 数据包解析只拒绝无法安全定位 TCP 头的输入；运行时会原样重新注入这些数据包。
#[derive(Debug, Error, Eq, PartialEq)]
pub enum PacketRewriteError {
    #[error("数据包短于 IP 头")]
    TruncatedIpHeader,
    #[error("不支持的 IP 版本 {0}")]
    UnsupportedIpVersion(u8),
    #[error("数据包不是 TCP")]
    NotTcp,
    #[error("数据包不是 UDP")]
    NotUdp,
    #[error("IPv6 扩展头损坏或不支持")]
    InvalidIpv6Extension,
    #[error("数据包短于 TCP 头")]
    TruncatedTcpHeader,
    #[error("数据包短于 UDP 头")]
    TruncatedUdpHeader,
    #[error("IP 或 UDP 长度字段超出当前数据包边界")]
    InvalidPacketLength,
    #[error("IPv6 UDP jumbogram 尚未提供 Jumbo Payload 扩展头")]
    UnsupportedUdpJumbogram,
}

/// 保存 UDP 五元组与载荷偏移；数据报保持原字节回注，仅使用这些字段完成进程归属和指标记账。
#[derive(Clone, Copy)]
pub(crate) struct ObservedUdpPacket {
    pub sourceAddress: IpAddr,
    pub destinationAddress: IpAddr,
    pub sourcePort: u16,
    pub destinationPort: u16,
    pub payloadOffset: usize,
    pub payloadEnd: usize,
}

struct ParsedTcpPacket {
    sourceAddress: IpAddr,
    destinationAddress: IpAddr,
    sourceAddressOffset: usize,
    destinationAddressOffset: usize,
    tcpOffset: usize,
    fragmentKey: Option<FragmentKey>,
}

enum ParsedPacket {
    Tcp(ParsedTcpPacket),
    LaterFragment(FragmentKey),
}

/// 按流表双向改写 TCP 包；未命中目标进程五元组时保持字节完全不变。
pub fn rewriteTcpPacket(
    packet: &mut [u8],
    outbound: bool,
    proxyPort: u16,
    flowTable: &CaptureFlowTable,
) -> Result<PacketDirection, PacketRewriteError> {
    if !outbound {
        return Ok(PacketDirection::Bypass);
    }
    let parsed = match parsePacket(packet, outbound)? {
        ParsedPacket::Tcp(parsed) => parsed,
        ParsedPacket::LaterFragment(key) => {
            return Ok(match flowTable.fragmentLookup(key) {
                FragmentLookup::Block(target) => PacketDirection::Blocked(target),
                FragmentLookup::Allow | FragmentLookup::Unknown => PacketDirection::Bypass,
            });
        }
    };
    let sourcePort = readPort(packet, parsed.tcpOffset);
    let destinationPort = readPort(packet, parsed.tcpOffset + 2);
    if sourcePort == proxyPort
        && let Some(target) = flowTable.reflectedTarget(
            parsed.sourceAddress,
            destinationPort,
            parsed.destinationAddress,
        )
    {
        if let Some(fragmentKey) = parsed.fragmentKey {
            let result = flowTable.recordFragmentDecision(
                fragmentKey,
                FragmentAction::Block {
                    endpointId: target.endpointId,
                    target: target.original,
                },
            );
            return Ok(match result {
                FragmentRecordResult::Recorded => PacketDirection::Blocked(target.original),
                FragmentRecordResult::EndpointGone => PacketDirection::Blocked(target.original),
            });
        }
        writeIpAddress(
            packet,
            parsed.sourceAddressOffset,
            target.original.address.ip(),
        );
        writeIpAddress(
            packet,
            parsed.destinationAddressOffset,
            target.originalLocalAddress,
        );
        writePort(packet, parsed.tcpOffset, target.original.address.port());
        writePort(packet, parsed.tcpOffset + 2, target.originalLocalPort);
        return Ok(PacketDirection::Restored(
            target.original,
            target.originalInterface,
        ));
    }
    let Some(target) = flowTable.outboundTarget(
        parsed.sourceAddress,
        sourcePort,
        parsed.destinationAddress,
        destinationPort,
    ) else {
        if let Some(fragmentKey) = parsed.fragmentKey {
            match flowTable.recordFragmentDecision(fragmentKey, FragmentAction::Allow) {
                FragmentRecordResult::Recorded | FragmentRecordResult::EndpointGone => {}
            }
        }
        return Ok(PacketDirection::Bypass);
    };
    if let Some(fragmentKey) = parsed.fragmentKey {
        let result = flowTable.recordFragmentDecision(
            fragmentKey,
            FragmentAction::Block {
                endpointId: target.endpointId,
                target: target.original,
            },
        );
        return Ok(match result {
            FragmentRecordResult::Recorded => PacketDirection::Blocked(target.original),
            FragmentRecordResult::EndpointGone => PacketDirection::Blocked(target.original),
        });
    }
    writeIpAddress(packet, parsed.sourceAddressOffset, target.proxyAddress);
    writeIpAddress(packet, parsed.destinationAddressOffset, target.proxyAddress);
    writePort(packet, parsed.tcpOffset, target.reflectedPort);
    writePort(packet, parsed.tcpOffset + 2, proxyPort);
    Ok(PacketDirection::Redirected {
        original: target.original,
        proxyAddress: target.proxyAddress,
        reflectedPort: target.reflectedPort,
    })
}

/// 识别选中进程的双向 UDP 数据报，不修改 IP、UDP 头或正文任何字节。
///
/// 运行上下文：NETWORK 句柄必须观察系统双向 UDP，流表只会命中由 SOCKET/FLOW 层确认的目标 PID。
/// 未命中、分片或损坏数据报保持 `Bypass`；命中时返回方向和进程身份供计数与录制层消费。
#[cfg(test)]
pub(crate) fn observeUdpPacket(
    packet: &[u8],
    outbound: bool,
    configuredProxyAddress: IpAddr,
    proxyPort: u16,
    flowTable: &CaptureFlowTable,
) -> Result<(PacketDirection, Option<ObservedUdpPacket>), PacketRewriteError> {
    let Some(parsed) = parseUdpPacket(packet)? else {
        return Ok((PacketDirection::Bypass, None));
    };
    let direction = observeUdpTuple(
        &parsed,
        outbound,
        SocketAddr::new(configuredProxyAddress, proxyPort),
        flowTable,
    );
    Ok((direction, Some(parsed)))
}

/// 仅解析已拦截 UDP 包而不查询流表；resolver 随后按捕获 QPC 水位选择正确的端口复用代际。
pub(crate) fn parseObservedUdpPacket(
    packet: &[u8],
) -> Result<Option<ObservedUdpPacket>, PacketRewriteError> {
    parseUdpPacket(packet)
}

/// 按已解析的 UDP 五元组查询双向归属；IP 分片首片与普通数据报共享这一唯一决策路径。
#[cfg(test)]
pub(crate) fn observeUdpTuple(
    parsed: &ObservedUdpPacket,
    outbound: bool,
    proxyAddress: SocketAddr,
    flowTable: &CaptureFlowTable,
) -> PacketDirection {
    if outbound
        && let Some(target) = flowTable.associateUdpOutbound(
            parsed.sourceAddress,
            parsed.sourcePort,
            parsed.destinationAddress,
            parsed.destinationPort,
            proxyAddress.ip(),
            proxyAddress.port(),
        )
    {
        return PacketDirection::ObservedUp(target.original);
    }
    // Windows 会把同机 LAN 地址之间的报文标成 loopback 且两个方向都带 outbound；方向必须再用
    // 已确认五元组判定，不能把地址元数据当作唯一依据，否则本机测试服务的响应会漏记。
    let inboundTarget = flowTable.inboundTransportTarget(
        crate::flowTable::udpProtocol,
        parsed.sourceAddress,
        parsed.sourcePort,
        parsed.destinationAddress,
        parsed.destinationPort,
    );
    match inboundTarget {
        Some(target) => PacketDirection::ObservedDown(target.original),
        None => PacketDirection::Bypass,
    }
}

/// 从已命中流表的 UDP 包构造统一数据面事件；目标地址以 FLOW 权威值为准。
pub(crate) fn udpDatagramEvent(
    packet: &[u8],
    target: OriginalTarget,
    direction: UdpDatagramDirection,
    capturedAtMilliseconds: u64,
) -> Option<UdpDatagramEvent> {
    let parsed = parseUdpPacket(packet).ok()??;
    let clientAddress = match direction {
        UdpDatagramDirection::Up => SocketAddr::new(parsed.sourceAddress, parsed.sourcePort),
        UdpDatagramDirection::Down => {
            SocketAddr::new(parsed.destinationAddress, parsed.destinationPort)
        }
    };
    Some(UdpDatagramEvent {
        processId: target.processId,
        clientAddress,
        targetAddress: target.address,
        direction,
        payload: packet[parsed.payloadOffset..parsed.payloadEnd].to_vec(),
        capturedAtMilliseconds,
    })
}

/// 解析 IPv4/IPv6 UDP 头；分片数据报不具备稳定五元组时返回 `None` 并保持系统原路径。
fn parseUdpPacket(packet: &[u8]) -> Result<Option<ObservedUdpPacket>, PacketRewriteError> {
    let version = packet
        .first()
        .ok_or(PacketRewriteError::TruncatedIpHeader)?
        >> 4;
    let (sourceAddress, destinationAddress, udpOffset, ipEnd) = match version {
        4 => {
            if packet.len() < 20 {
                return Err(PacketRewriteError::TruncatedIpHeader);
            }
            let headerLength = usize::from(packet[0] & 0x0f) * 4;
            if headerLength < 20 || packet.len() < headerLength {
                return Err(PacketRewriteError::TruncatedIpHeader);
            }
            if packet[9] != crate::flowTable::udpProtocol {
                return Err(PacketRewriteError::NotUdp);
            }
            let totalLength = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
            if totalLength < headerLength + 8 || totalLength > packet.len() {
                return Err(PacketRewriteError::InvalidPacketLength);
            }
            let fragment = u16::from_be_bytes([packet[6], packet[7]]);
            if fragment & 0x3fff != 0 {
                return Ok(None);
            }
            (
                IpAddr::V4(Ipv4Addr::new(
                    packet[12], packet[13], packet[14], packet[15],
                )),
                IpAddr::V4(Ipv4Addr::new(
                    packet[16], packet[17], packet[18], packet[19],
                )),
                headerLength,
                totalLength,
            )
        }
        6 => {
            if packet.len() < 40 {
                return Err(PacketRewriteError::TruncatedIpHeader);
            }
            let payloadLength = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
            if payloadLength == 0 {
                return Err(PacketRewriteError::UnsupportedUdpJumbogram);
            }
            let ipEnd = 40usize
                .checked_add(payloadLength)
                .filter(|end| *end <= packet.len())
                .ok_or(PacketRewriteError::InvalidPacketLength)?;
            let mut nextHeader = packet[6];
            let mut offset = 40usize;
            while nextHeader != crate::flowTable::udpProtocol {
                match nextHeader {
                    0 | 43 | 60 => {
                        ensureExtensionBytes(packet, offset, 2)?;
                        nextHeader = packet[offset];
                        offset += (usize::from(packet[offset + 1]) + 1) * 8;
                    }
                    51 => {
                        ensureExtensionBytes(packet, offset, 2)?;
                        nextHeader = packet[offset];
                        offset += (usize::from(packet[offset + 1]) + 2) * 4;
                    }
                    44 => return Ok(None),
                    _ => return Err(PacketRewriteError::NotUdp),
                }
                if offset > ipEnd {
                    return Err(PacketRewriteError::InvalidIpv6Extension);
                }
            }
            (
                IpAddr::V6(Ipv6Addr::from(
                    <[u8; 16]>::try_from(&packet[8..24]).expect("IPv6 源地址长度已校验"),
                )),
                IpAddr::V6(Ipv6Addr::from(
                    <[u8; 16]>::try_from(&packet[24..40]).expect("IPv6 目标地址长度已校验"),
                )),
                offset,
                ipEnd,
            )
        }
        value => return Err(PacketRewriteError::UnsupportedIpVersion(value)),
    };
    if ipEnd < udpOffset + 8 {
        return Err(PacketRewriteError::TruncatedUdpHeader);
    }
    let udpLength = usize::from(u16::from_be_bytes([
        packet[udpOffset + 4],
        packet[udpOffset + 5],
    ]));
    if udpLength < 8
        || udpOffset
            .checked_add(udpLength)
            .is_none_or(|end| end > ipEnd)
    {
        return Err(PacketRewriteError::InvalidPacketLength);
    }
    Ok(Some(ObservedUdpPacket {
        sourceAddress,
        destinationAddress,
        sourcePort: readPort(packet, udpOffset),
        destinationPort: readPort(packet, udpOffset + 2),
        payloadOffset: udpOffset + 8,
        payloadEnd: udpOffset + udpLength,
    }))
}

/// 判断数据包是否为新 TCP 连接的首个 SYN；用于等待 SOCKET 事件线程完成 PID 五元组登记。
pub(crate) fn isTcpStartPacket(packet: &[u8]) -> bool {
    let Ok(ParsedPacket::Tcp(parsed)) = parsePacket(packet, true) else {
        return false;
    };
    let flags = packet[parsed.tcpOffset + 13];
    flags & 0x02 != 0 && flags & 0x10 == 0
}

/// 解析 IPv4/IPv6 及常见 IPv6 扩展头，只返回原地改写所需的固定偏移。
fn parsePacket(packet: &[u8], outbound: bool) -> Result<ParsedPacket, PacketRewriteError> {
    let version = packet
        .first()
        .ok_or(PacketRewriteError::TruncatedIpHeader)?
        >> 4;
    match version {
        4 => parseIpv4TcpPacket(packet, outbound),
        6 => parseIpv6TcpPacket(packet, outbound),
        value => Err(PacketRewriteError::UnsupportedIpVersion(value)),
    }
}

/// 解析 IPv4 TCP 头；非首片返回分片标识，首片则保留该标识供命中流表后建立阻断组。
fn parseIpv4TcpPacket(packet: &[u8], outbound: bool) -> Result<ParsedPacket, PacketRewriteError> {
    if packet.len() < 20 {
        return Err(PacketRewriteError::TruncatedIpHeader);
    }
    let headerLength = usize::from(packet[0] & 0x0f) * 4;
    if headerLength < 20 || packet.len() < headerLength {
        return Err(PacketRewriteError::TruncatedIpHeader);
    }
    if packet[9] != 6 {
        return Err(PacketRewriteError::NotTcp);
    }
    let fragment = u16::from_be_bytes([packet[6], packet[7]]);
    let sourceAddress = IpAddr::V4(Ipv4Addr::new(
        packet[12], packet[13], packet[14], packet[15],
    ));
    let destinationAddress = IpAddr::V4(Ipv4Addr::new(
        packet[16], packet[17], packet[18], packet[19],
    ));
    let fragmentKey = FragmentKey {
        sourceAddress,
        destinationAddress,
        identification: u32::from(u16::from_be_bytes([packet[4], packet[5]])),
        protocol: packet[9],
        outbound,
    };
    if fragment & 0x1fff != 0 {
        return Ok(ParsedPacket::LaterFragment(fragmentKey));
    }
    if packet.len() < headerLength + 20 {
        return Err(PacketRewriteError::TruncatedTcpHeader);
    }
    Ok(ParsedPacket::Tcp(ParsedTcpPacket {
        sourceAddress,
        destinationAddress,
        sourceAddressOffset: 12,
        destinationAddressOffset: 16,
        tcpOffset: headerLength,
        fragmentKey: (fragment & 0x2000 != 0).then_some(fragmentKey),
    }))
}

/// 穿过 IPv6 扩展头定位 TCP；非首片直接返回分片标识，首片保留标识供阻断后续片。
fn parseIpv6TcpPacket(packet: &[u8], outbound: bool) -> Result<ParsedPacket, PacketRewriteError> {
    if packet.len() < 40 {
        return Err(PacketRewriteError::TruncatedIpHeader);
    }
    let mut nextHeader = packet[6];
    let mut offset = 40usize;
    let sourceBytes: [u8; 16] = packet[8..24].try_into().expect("IPv6 源地址长度已校验");
    let destinationBytes: [u8; 16] = packet[24..40].try_into().expect("IPv6 目标地址长度已校验");
    let sourceAddress = IpAddr::V6(Ipv6Addr::from(sourceBytes));
    let destinationAddress = IpAddr::V6(Ipv6Addr::from(destinationBytes));
    let mut fragmentKey = None;
    while nextHeader != 6 {
        match nextHeader {
            0 | 43 | 60 => {
                ensureExtensionBytes(packet, offset, 2)?;
                nextHeader = packet[offset];
                offset += (usize::from(packet[offset + 1]) + 1) * 8;
            }
            44 => {
                ensureExtensionBytes(packet, offset, 8)?;
                let fragment = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]);
                let key = FragmentKey {
                    sourceAddress,
                    destinationAddress,
                    identification: u32::from_be_bytes(
                        packet[offset + 4..offset + 8]
                            .try_into()
                            .expect("IPv6 分片标识长度已校验"),
                    ),
                    protocol: packet[offset],
                    outbound,
                };
                if fragment & 0xfff8 != 0 {
                    return Ok(ParsedPacket::LaterFragment(key));
                }
                fragmentKey = (fragment & 0x0001 != 0).then_some(key);
                nextHeader = packet[offset];
                offset += 8;
            }
            51 => {
                ensureExtensionBytes(packet, offset, 2)?;
                nextHeader = packet[offset];
                offset += (usize::from(packet[offset + 1]) + 2) * 4;
            }
            _ => return Err(PacketRewriteError::NotTcp),
        }
        if offset > packet.len() {
            return Err(PacketRewriteError::InvalidIpv6Extension);
        }
    }
    if packet.len() < offset + 20 {
        return Err(PacketRewriteError::TruncatedTcpHeader);
    }
    Ok(ParsedPacket::Tcp(ParsedTcpPacket {
        sourceAddress,
        destinationAddress,
        sourceAddressOffset: 8,
        destinationAddressOffset: 24,
        tcpOffset: offset,
        fragmentKey,
    }))
}

/// 校验扩展头最小长度，统一截断错误语义并阻止偏移溢出。
fn ensureExtensionBytes(
    packet: &[u8],
    offset: usize,
    length: usize,
) -> Result<(), PacketRewriteError> {
    if offset
        .checked_add(length)
        .is_none_or(|end| end > packet.len())
    {
        return Err(PacketRewriteError::InvalidIpv6Extension);
    }
    Ok(())
}

/// 写入已确认同地址族的 IP 地址；流表在登记阶段已排除跨地址族监听配置。
fn writeIpAddress(packet: &mut [u8], offset: usize, address: IpAddr) {
    match address {
        IpAddr::V4(address) => packet[offset..offset + 4].copy_from_slice(&address.octets()),
        IpAddr::V6(address) => packet[offset..offset + 16].copy_from_slice(&address.octets()),
    }
}

/// 从 TCP 头读取网络字节序端口；偏移已由解析器验证。
fn readPort(packet: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([packet[offset], packet[offset + 1]])
}

/// 写入网络字节序 TCP 端口；修改后由 WinDivert 统一重算 IP/TCP 校验和。
fn writePort(packet: &mut [u8], offset: usize, port: u16) {
    packet[offset..offset + 2].copy_from_slice(&port.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptureFlow;

    /// 构造无选项 IPv4 TCP 头，载荷不参与地址反射测试。
    fn ipv4Packet(
        source: [u8; 4],
        destination: [u8; 4],
        sourcePort: u16,
        destinationPort: u16,
    ) -> Vec<u8> {
        let mut packet = vec![0u8; 40];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&(40u16).to_be_bytes());
        packet[8] = 64;
        packet[9] = 6;
        packet[12..16].copy_from_slice(&source);
        packet[16..20].copy_from_slice(&destination);
        packet[20..22].copy_from_slice(&sourcePort.to_be_bytes());
        packet[22..24].copy_from_slice(&destinationPort.to_be_bytes());
        packet[32] = 0x50;
        packet
    }

    /// 创建与测试包一致的目标流，保证只验证改写而不依赖驱动。
    fn flowTable() -> CaptureFlowTable {
        let table = CaptureFlowTable::default();
        table.insert(
            CaptureFlow::tcp(
                700,
                8,
                "192.0.2.10".parse().unwrap(),
                52000,
                "198.51.100.20".parse().unwrap(),
                443,
            )
            .unwrap(),
            "127.0.0.1".parse().unwrap(),
            1080,
        );
        table
    }

    #[test]
    /// 验证出站反射和代理回复恢复会同时改写地址与两侧端口。
    fn redirectsAndRestoresIpv4Packet() {
        let table = flowTable();
        let mut outbound = ipv4Packet([192, 0, 2, 10], [198, 51, 100, 20], 52000, 443);
        assert!(matches!(
            rewriteTcpPacket(&mut outbound, true, 1080, &table).unwrap(),
            PacketDirection::Redirected { .. }
        ));
        assert_eq!(&outbound[12..16], &[127, 0, 0, 1]);
        assert_eq!(&outbound[16..20], &[127, 0, 0, 1]);
        let reflectedPort = readPort(&outbound, 20);
        assert_eq!(readPort(&outbound, 22), 1080);

        let mut reply = ipv4Packet([127, 0, 0, 1], [127, 0, 0, 1], 1080, reflectedPort);
        assert!(matches!(
            rewriteTcpPacket(&mut reply, true, 1080, &table).unwrap(),
            PacketDirection::Restored(_, _)
        ));
        assert_eq!(&reply[12..16], &[198, 51, 100, 20]);
        assert_eq!(&reply[16..20], &[192, 0, 2, 10]);
        assert_eq!(readPort(&reply, 20), 443);
        assert_eq!(readPort(&reply, 22), 52000);
    }

    #[test]
    /// 验证未选进程的五元组保持逐字节不变。
    fn leavesUntrackedPacketUnchanged() {
        let table = flowTable();
        let mut packet = ipv4Packet([192, 0, 2, 11], [203, 0, 113, 9], 53000, 80);
        let baseline = packet.clone();
        assert_eq!(
            rewriteTcpPacket(&mut packet, true, 1080, &table).unwrap(),
            PacketDirection::Bypass
        );
        assert_eq!(packet, baseline);
    }

    #[test]
    /// 验证带更多分片标志的 IPv4 首分片不会只改写首片而破坏后续分片路由。
    fn rejectsIpv4FirstFragment() {
        let table = flowTable();
        let mut packet = ipv4Packet([192, 0, 2, 10], [198, 51, 100, 20], 52000, 443);
        packet[4..6].copy_from_slice(&0x1234u16.to_be_bytes());
        packet[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
        assert_eq!(
            rewriteTcpPacket(&mut packet, true, 1080, &table),
            Ok(PacketDirection::Blocked(OriginalTarget {
                processId: 700,
                address: std::net::SocketAddr::new("198.51.100.20".parse().unwrap(), 443),
            }))
        );

        let mut laterFragment = packet.clone();
        laterFragment[6..8].copy_from_slice(&0x0001u16.to_be_bytes());
        assert!(matches!(
            rewriteTcpPacket(&mut laterFragment, true, 1080, &table),
            Ok(PacketDirection::Blocked(_))
        ));

        let mut outOfOrderFragment = laterFragment.clone();
        outOfOrderFragment[4..6].copy_from_slice(&0x4321u16.to_be_bytes());
        assert_eq!(
            rewriteTcpPacket(&mut outOfOrderFragment, true, 1080, &table),
            Ok(PacketDirection::Bypass)
        );

        let mut unselectedFirst = ipv4Packet([192, 0, 2, 11], [203, 0, 113, 9], 53000, 443);
        unselectedFirst[4..6].copy_from_slice(&0x5678u16.to_be_bytes());
        unselectedFirst[6..8].copy_from_slice(&0x2000u16.to_be_bytes());
        assert_eq!(
            rewriteTcpPacket(&mut unselectedFirst, true, 1080, &table),
            Ok(PacketDirection::Bypass)
        );
        unselectedFirst[6..8].copy_from_slice(&0x0001u16.to_be_bytes());
        assert_eq!(
            rewriteTcpPacket(&mut unselectedFirst, true, 1080, &table),
            Ok(PacketDirection::Bypass)
        );
    }

    #[test]
    /// 验证仍有后续分片的 IPv6 首分片不会参与仅能覆盖单包的透明地址改写。
    fn rejectsIpv6FirstFragment() {
        let table = CaptureFlowTable::default();
        table.insert(
            CaptureFlow::tcp(
                701,
                9,
                "2001:db8::10".parse().unwrap(),
                52001,
                "2001:db8::20".parse().unwrap(),
                443,
            )
            .unwrap(),
            "::1".parse().unwrap(),
            1080,
        );
        let mut packet = vec![0u8; 68];
        packet[0] = 0x60;
        packet[6] = 44;
        packet[8..24].copy_from_slice(&"2001:db8::10".parse::<Ipv6Addr>().unwrap().octets());
        packet[24..40].copy_from_slice(&"2001:db8::20".parse::<Ipv6Addr>().unwrap().octets());
        packet[40] = 6;
        packet[42..44].copy_from_slice(&1u16.to_be_bytes());
        packet[44..48].copy_from_slice(&0x01020304u32.to_be_bytes());
        packet[48..50].copy_from_slice(&52001u16.to_be_bytes());
        packet[50..52].copy_from_slice(&443u16.to_be_bytes());
        assert!(matches!(
            rewriteTcpPacket(&mut packet, true, 1080, &table),
            Ok(PacketDirection::Blocked(_))
        ));

        let mut laterFragment = packet.clone();
        laterFragment[42..44].copy_from_slice(&0x0008u16.to_be_bytes());
        assert!(matches!(
            rewriteTcpPacket(&mut laterFragment, true, 1080, &table),
            Ok(PacketDirection::Blocked(_))
        ));

        let mut differentIdentification = laterFragment.clone();
        differentIdentification[44..48].copy_from_slice(&0x05060708u32.to_be_bytes());
        assert_eq!(
            rewriteTcpPacket(&mut differentIdentification, true, 1080, &table),
            Ok(PacketDirection::Bypass)
        );

        table.removeEndpoint(9);
        assert_eq!(
            rewriteTcpPacket(&mut laterFragment, true, 1080, &table),
            Ok(PacketDirection::Bypass)
        );
    }

    /// 构造未分片 IPv4 UDP 数据报；校验和留空，因为观察路径承诺不改写任何字节。
    fn ipv4UdpPacket(
        source: [u8; 4],
        destination: [u8; 4],
        sourcePort: u16,
        destinationPort: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let packetLength = 28 + payload.len();
        let mut packet = vec![0u8; packetLength];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&(packetLength as u16).to_be_bytes());
        packet[8] = 64;
        packet[9] = crate::flowTable::udpProtocol;
        packet[12..16].copy_from_slice(&source);
        packet[16..20].copy_from_slice(&destination);
        packet[20..22].copy_from_slice(&sourcePort.to_be_bytes());
        packet[22..24].copy_from_slice(&destinationPort.to_be_bytes());
        packet[24..26].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        packet[28..].copy_from_slice(payload);
        packet
    }

    /// 验证选中进程的 IPv4 UDP 双向数据报保持原字节，并把完整 payload 与权威目标投影到录制事件。
    #[test]
    fn observesIpv4UdpWithoutModifyingDatagram() {
        let table = CaptureFlowTable::default();
        table.insert(
            CaptureFlow::udp(
                900,
                17,
                "192.0.2.30".parse().unwrap(),
                53000,
                "198.51.100.53".parse().unwrap(),
                443,
            )
            .unwrap(),
            "127.0.0.1".parse().unwrap(),
            1080,
        );
        let up = ipv4UdpPacket(
            [192, 0, 2, 30],
            [198, 51, 100, 53],
            53000,
            443,
            b"quic-request",
        );
        let baseline = up.clone();
        let (direction, _) =
            observeUdpPacket(&up, true, "127.0.0.1".parse().unwrap(), 1080, &table).unwrap();
        let PacketDirection::ObservedUp(target) = direction else {
            panic!("选中进程 IPv4 UDP 上行必须命中")
        };
        assert_eq!(up, baseline);
        let event = udpDatagramEvent(&up, target, UdpDatagramDirection::Up, 123).unwrap();
        assert_eq!(event.processId, 900);
        assert_eq!(event.clientAddress, "192.0.2.30:53000".parse().unwrap());
        assert_eq!(event.targetAddress, "198.51.100.53:443".parse().unwrap());
        assert_eq!(event.payload, b"quic-request");

        let down = ipv4UdpPacket(
            [198, 51, 100, 53],
            [192, 0, 2, 30],
            443,
            53000,
            b"quic-response",
        );
        let (direction, _) =
            observeUdpPacket(&down, false, "127.0.0.1".parse().unwrap(), 1080, &table).unwrap();
        assert!(matches!(direction, PacketDirection::ObservedDown(_)));
    }

    /// 验证 IPv6 UDP 五元组使用 128 位地址匹配，不会退化为 IPv4 或仅按端口误认。
    #[test]
    fn observesIpv6UdpFlow() {
        let local: Ipv6Addr = "2001:db8::30".parse().unwrap();
        let remote: Ipv6Addr = "2001:db8::53".parse().unwrap();
        let table = CaptureFlowTable::default();
        table.insert(
            CaptureFlow::udp(901, 18, local.into(), 53001, remote.into(), 53).unwrap(),
            "::1".parse().unwrap(),
            1080,
        );
        let payload = b"dns-over-udp-v6";
        let mut packet = vec![0u8; 48 + payload.len()];
        packet[0] = 0x60;
        packet[4..6].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        packet[6] = crate::flowTable::udpProtocol;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&local.octets());
        packet[24..40].copy_from_slice(&remote.octets());
        packet[40..42].copy_from_slice(&53001u16.to_be_bytes());
        packet[42..44].copy_from_slice(&53u16.to_be_bytes());
        packet[44..46].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        packet[48..].copy_from_slice(payload);
        let baseline = packet.clone();
        let (direction, _) =
            observeUdpPacket(&packet, true, "::1".parse().unwrap(), 1080, &table).unwrap();
        assert!(matches!(direction, PacketDirection::ObservedUp(_)));
        assert_eq!(packet, baseline);
    }
}
