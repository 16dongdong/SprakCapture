#![allow(non_snake_case)]

use std::net::SocketAddr;
use std::time::Duration;

use process_capture_core::{
    OriginalTarget, UdpDatagramDirection,
    udpFragment::{
        UdpFragmentAssembler, UdpFragmentDisposition, UdpPacketFragment, inspectUdpFragment,
    },
};

/// 构造 IPv4 UDP 分片；offset 使用 IP 头规定的 8 字节单位。
fn ipv4Fragment(identification: u16, offset: usize, more: bool, bytes: &[u8]) -> Vec<u8> {
    let mut packet = vec![0_u8; 20 + bytes.len()];
    let packetLength = packet.len() as u16;
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&packetLength.to_be_bytes());
    packet[4..6].copy_from_slice(&identification.to_be_bytes());
    let fragment = (u16::try_from(offset / 8).unwrap()) | if more { 0x2000 } else { 0 };
    packet[6..8].copy_from_slice(&fragment.to_be_bytes());
    packet[8] = 64;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&[192, 0, 2, 40]);
    packet[16..20].copy_from_slice(&[198, 51, 100, 40]);
    packet[20..].copy_from_slice(bytes);
    packet
}

/// 构造 IPv6 Fragment→Destination Options→UDP 分片，验证 fragmentable 扩展链不是固定直连 UDP。
fn ipv6Fragment(identification: u32, offset: usize, more: bool, bytes: &[u8]) -> Vec<u8> {
    let mut packet = vec![0_u8; 48 + bytes.len()];
    packet[0] = 0x60;
    packet[4..6].copy_from_slice(&((8 + bytes.len()) as u16).to_be_bytes());
    packet[6] = 44;
    packet[7] = 64;
    packet[8..24].copy_from_slice(
        &"2001:db8::40"
            .parse::<std::net::Ipv6Addr>()
            .unwrap()
            .octets(),
    );
    packet[24..40].copy_from_slice(
        &"2001:db8::53"
            .parse::<std::net::Ipv6Addr>()
            .unwrap()
            .octets(),
    );
    packet[40] = 60;
    let fragment = u16::try_from(offset).unwrap() | u16::from(more);
    packet[42..44].copy_from_slice(&fragment.to_be_bytes());
    packet[44..48].copy_from_slice(&identification.to_be_bytes());
    packet[48..].copy_from_slice(bytes);
    packet
}

/// 构造 UDP header 与正文；length 字段是重组后正文的唯一裁剪边界。
fn udpBytes(sourcePort: u16, destinationPort: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0_u8; 8 + payload.len()];
    let udpLength = bytes.len() as u16;
    bytes[0..2].copy_from_slice(&sourcePort.to_be_bytes());
    bytes[2..4].copy_from_slice(&destinationPort.to_be_bytes());
    bytes[4..6].copy_from_slice(&udpLength.to_be_bytes());
    bytes[8..].copy_from_slice(payload);
    bytes
}

/// 验证 IPv4 后到首片的乱序交付仍只发布一次完整正文。
#[test]
fn reassemblesOutOfOrderIpv4UdpPayload() {
    let payload = b"fragmented-ipv4-media-body";
    let udp = udpBytes(53_040, 443, payload);
    let split = 16;
    let later = ipv4Fragment(0x4010, split, false, &udp[split..]);
    let first = ipv4Fragment(0x4010, 0, true, &udp[..split]);
    let mut assembler = UdpFragmentAssembler::default();
    let UdpPacketFragment::Fragment(later) = inspectUdpFragment(&later, true).unwrap() else {
        panic!("IPv4 末片必须进入重组")
    };
    assert!(assembler.push(later, None).unwrap().is_none());
    let UdpPacketFragment::Fragment(first) = inspectUdpFragment(&first, true).unwrap() else {
        panic!("IPv4 首片必须进入重组")
    };
    let reassembled = assembler
        .push(
            first,
            Some(UdpFragmentDisposition::Selected {
                target: OriginalTarget {
                    processId: 7_001,
                    address: "198.51.100.40:443".parse().unwrap(),
                },
                direction: UdpDatagramDirection::Up,
                clientAddress: "192.0.2.40:53040".parse().unwrap(),
                capturedAtMilliseconds: 1,
            }),
        )
        .unwrap()
        .expect("IPv4 分片必须完整");
    assert_eq!(reassembled.event.unwrap().payload, payload);
    assert_eq!(reassembled.packetCount, 2);
}

/// 验证 IPv6 Fragment 后的 Destination Options 能被遍历并按 UDP length 精确裁剪。
#[test]
fn reassemblesIpv6PostFragmentExtensionChain() {
    let payload = b"fragmented-ipv6-dns-body";
    let udp = udpBytes(53_041, 53, payload);
    let mut fragmentable = vec![17, 0, 0, 0, 0, 0, 0, 0];
    fragmentable.extend_from_slice(&udp);
    let split = 24;
    let firstPacket = ipv6Fragment(0x0102_0304, 0, true, &fragmentable[..split]);
    let laterPacket = ipv6Fragment(0x0102_0304, split, false, &fragmentable[split..]);
    let mut assembler = UdpFragmentAssembler::default();
    let UdpPacketFragment::Fragment(first) = inspectUdpFragment(&firstPacket, true).unwrap() else {
        panic!("IPv6 首片必须进入重组")
    };
    assert!(
        assembler
            .push(
                first,
                Some(UdpFragmentDisposition::Selected {
                    target: OriginalTarget {
                        processId: 7_002,
                        address: "[2001:db8::53]:53".parse().unwrap(),
                    },
                    direction: UdpDatagramDirection::Up,
                    clientAddress: "[2001:db8::40]:53041".parse::<SocketAddr>().unwrap(),
                    capturedAtMilliseconds: 2,
                }),
            )
            .unwrap()
            .is_none()
    );
    let UdpPacketFragment::Fragment(later) = inspectUdpFragment(&laterPacket, true).unwrap() else {
        panic!("IPv6 末片必须进入重组")
    };
    let reassembled = assembler
        .push(later, None)
        .unwrap()
        .expect("IPv6 扩展链分片必须完整");
    assert_eq!(reassembled.event.unwrap().payload, payload);
}

/// 验证没有后续网络包时，resolver 的空闲 tick 仍能发现选中首片缺少末片。
#[test]
fn reportsSelectedFragmentTimeoutWithoutAnotherPacket() {
    let udp = udpBytes(53_042, 53, b"selected-timeout-body");
    let firstPacket = ipv4Fragment(0x4020, 0, true, &udp[..16]);
    let UdpPacketFragment::Fragment(first) = inspectUdpFragment(&firstPacket, true).unwrap() else {
        panic!("超时用首片必须进入重组")
    };
    let mut assembler = UdpFragmentAssembler::withFragmentLifetime(Duration::from_millis(5));
    assert!(
        assembler
            .push(
                first,
                Some(UdpFragmentDisposition::Selected {
                    target: OriginalTarget {
                        processId: 7_003,
                        address: "198.51.100.42:53".parse().unwrap(),
                    },
                    direction: UdpDatagramDirection::Up,
                    clientAddress: "192.0.2.40:53042".parse().unwrap(),
                    capturedAtMilliseconds: 3,
                }),
            )
            .unwrap()
            .is_none()
    );
    std::thread::sleep(Duration::from_millis(15));
    assert!(assembler.pollExpired().is_err());
}

/// 验证 1,024 个乱序未知组占满预算后，第 1,025 个组会显式故障而不是淘汰潜在选中正文。
#[test]
fn preservesUnknownGroupsAndFaultsAtFixedBudget() {
    let payload = b"late-selected-after-unknown-flood";
    let udp = udpBytes(53_043, 443, payload);
    let split = 8;
    let mut assembler = UdpFragmentAssembler::default();
    for identification in 1..=1_024_u16 {
        let laterPacket = ipv4Fragment(identification, split, false, &udp[split..]);
        let UdpPacketFragment::Fragment(later) = inspectUdpFragment(&laterPacket, true).unwrap()
        else {
            panic!("乱序末片必须进入有界重组")
        };
        assert!(assembler.push(later, None).unwrap().is_none());
    }

    let overflowPacket = ipv4Fragment(1_025, split, false, &udp[split..]);
    let UdpPacketFragment::Fragment(overflow) = inspectUdpFragment(&overflowPacket, true).unwrap()
    else {
        panic!("预算边界末片必须进入重组")
    };
    assert!(assembler.push(overflow, None).is_err());

    // 首个未知组没有为容纳洪泛而被静默淘汰；迟到首片仍能证明归属并恢复完整正文。
    let firstPacket = ipv4Fragment(1, 0, true, &udp[..split]);
    let UdpPacketFragment::Fragment(first) = inspectUdpFragment(&firstPacket, true).unwrap() else {
        panic!("迟到首片必须进入重组")
    };
    let reassembled = assembler
        .push(
            first,
            Some(UdpFragmentDisposition::Selected {
                target: OriginalTarget {
                    processId: 7_004,
                    address: "198.51.100.40:443".parse().unwrap(),
                },
                direction: UdpDatagramDirection::Up,
                clientAddress: "192.0.2.40:53043".parse().unwrap(),
                capturedAtMilliseconds: 4,
            }),
        )
        .unwrap()
        .expect("未知末片与迟到选中首片必须完整重组");
    assert_eq!(reassembled.event.unwrap().payload, payload);
}
