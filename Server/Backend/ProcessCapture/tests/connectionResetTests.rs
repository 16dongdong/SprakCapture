#![allow(non_snake_case, non_upper_case_globals)]
#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    net::Ipv6Addr,
    time::{Duration, Instant},
};

use windows_sys::Win32::NetworkManagement::IpHelper::{
    MIB_TCP_STATE_LISTEN, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
};

/// 为按源码复用的连接重置模块提供与生产 crate 相同的错误契约；测试不执行错误格式化。
#[derive(Debug)]
pub enum ProcessCaptureError {
    EnumerateConnections {
        addressFamily: &'static str,
        status: u32,
    },
    InvalidConnectionTable(&'static str),
    ResetConnection {
        addressFamily: &'static str,
        processId: u32,
        status: u32,
    },
}

#[path = "../src/connectionReset.rs"]
mod connectionReset;

use connectionReset::{ConnectionResetState, shouldQueueIpv6Connection, shouldResetIpv4Connection};

/// 按 Windows owner table 的网络字节序构造 IPv4 记录；用于验证热更新不会重置控制面回环连接。
fn ipv4OwnerRow(localAddress: [u8; 4], remoteAddress: [u8; 4]) -> MIB_TCPROW_OWNER_PID {
    MIB_TCPROW_OWNER_PID {
        dwState: 5,
        dwLocalAddr: u32::from_ne_bytes(localAddress),
        dwLocalPort: u32::from(52_001_u16.to_be()),
        dwRemoteAddr: u32::from_ne_bytes(remoteAddress),
        dwRemotePort: u32::from(443_u16.to_be()),
        dwOwningPid: 42,
    }
}

/// 构造 MIB IPv6 记录，验证状态、作用域和 PID 过滤，不依赖机器当前连接表。
fn ownerRow(processId: u32, state: u32) -> MIB_TCP6ROW_OWNER_PID {
    MIB_TCP6ROW_OWNER_PID {
        ucLocalAddr: "2001:db8::10".parse::<Ipv6Addr>().unwrap().octets(),
        dwLocalScopeId: 0,
        dwLocalPort: u32::from(52_001_u16.to_be()),
        ucRemoteAddr: "2001:db8::20".parse::<Ipv6Addr>().unwrap().octets(),
        dwRemoteScopeId: 0,
        dwRemotePort: u32::from(443_u16.to_be()),
        dwState: state,
        dwOwningPid: processId,
    }
}

/// 按本地 TCP 六元组构造带 ACK 的最小 IPv6 报文；校验和不参与纯构造测试。
fn acknowledgementPacket(
    sourceAddress: Ipv6Addr,
    destinationAddress: Ipv6Addr,
    sourcePort: u16,
    destinationPort: u16,
) -> Vec<u8> {
    let mut packet = vec![0_u8; 60];
    packet[0] = 0x60;
    packet[4..6].copy_from_slice(&20_u16.to_be_bytes());
    packet[6] = 6;
    packet[7] = 64;
    packet[8..24].copy_from_slice(&sourceAddress.octets());
    packet[24..40].copy_from_slice(&destinationAddress.octets());
    packet[40..42].copy_from_slice(&sourcePort.to_be_bytes());
    packet[42..44].copy_from_slice(&destinationPort.to_be_bytes());
    packet[44..48].copy_from_slice(&0x1020_3040_u32.to_be_bytes());
    packet[48..52].copy_from_slice(&0x5060_7080_u32.to_be_bytes());
    packet[52] = 0x50;
    packet[53] = 0x10;
    packet
}

#[test]
fn rejectsInactiveAndScopedIpv6OwnerRows() {
    let selectedProcessIds = BTreeSet::from([42]);
    assert!(shouldQueueIpv6Connection(
        &ownerRow(42, 5),
        &selectedProcessIds
    ));
    assert!(!shouldQueueIpv6Connection(
        &ownerRow(7, 5),
        &selectedProcessIds
    ));
    assert!(!shouldQueueIpv6Connection(
        &ownerRow(42, MIB_TCP_STATE_LISTEN as u32),
        &selectedProcessIds
    ));
    let mut scoped = ownerRow(42, 5);
    scoped.dwLocalScopeId = 7;
    assert!(!shouldQueueIpv6Connection(&scoped, &selectedProcessIds));
}

/// 验证 PID 热更新只关闭外部 TCP，会保留承载当前控制请求和代理入口的 IPv4/IPv6 回环 TCB。
#[test]
fn preservesLoopbackConnectionsDuringProcessSelectionUpdate() {
    let selectedProcessIds = BTreeSet::from([42]);
    assert!(shouldResetIpv4Connection(
        &ipv4OwnerRow([192, 0, 2, 10], [198, 51, 100, 20]),
        &selectedProcessIds,
    ));
    assert!(!shouldResetIpv4Connection(
        &ipv4OwnerRow([127, 0, 0, 1], [127, 0, 0, 1]),
        &selectedProcessIds,
    ));

    let mut loopbackIpv6 = ownerRow(42, 5);
    loopbackIpv6.ucLocalAddr = Ipv6Addr::LOCALHOST.octets();
    loopbackIpv6.ucRemoteAddr = Ipv6Addr::LOCALHOST.octets();
    assert!(!shouldQueueIpv6Connection(
        &loopbackIpv6,
        &selectedProcessIds,
    ));
}

#[test]
fn queuesIpv6OwnerRowsAndBuildsBidirectionalResets() {
    let resetState = ConnectionResetState::default();
    let row = ownerRow(42, 5);
    assert_eq!(
        resetState.queueIpv6Connections(&[row], &BTreeSet::from([42])),
        1
    );
    let packet = acknowledgementPacket(
        "2001:db8::10".parse().unwrap(),
        "2001:db8::20".parse().unwrap(),
        52_001,
        443,
    );
    let resets = resetState
        .takeIpv6ResetPackets(&packet, true)
        .expect("真实 ACK 应命中 owner table 中的 IPv6 四元组");
    assert_eq!(&resets.forward[44..48], &0x1020_3040_u32.to_be_bytes());
    assert_eq!(&resets.reverse[44..48], &0x5060_7080_u32.to_be_bytes());
    assert_eq!(resets.forward[53], 0x04);
    assert_eq!(resets.reverse[53], 0x04);
    assert_eq!(&resets.forward[8..24], &resets.reverse[24..40]);
    assert_eq!(&resets.forward[24..40], &resets.reverse[8..24]);
    resets.restoreAfterFailure(&resetState);
    assert!(
        resetState.pruneExpired(Instant::now()) >= 1,
        "RST 注入失败后必须保留目标四元组；同一测试进程的其他 IPv6 连接不影响该契约"
    );
    assert!(
        resetState.takeIpv6ResetPackets(&packet, true).is_some(),
        "RST 注入失败后必须保留同一四元组等待下一份真实 ACK 重试"
    );
    resetState.clear();
}

#[test]
fn expiresClosedConnectionBeforeTupleCanBeReused() {
    let resetState = ConnectionResetState::default();
    assert_eq!(
        resetState.queueIpv6Connections(&[ownerRow(42, 5)], &BTreeSet::from([42])),
        1
    );

    assert_eq!(
        resetState.pruneExpired(Instant::now() + Duration::from_secs(31)),
        0
    );
    let reusedTuplePacket = acknowledgementPacket(
        "2001:db8::10".parse().unwrap(),
        "2001:db8::20".parse().unwrap(),
        52_001,
        443,
    );
    assert!(
        resetState
            .takeIpv6ResetPackets(&reusedTuplePacket, true)
            .is_none(),
        "过期四元组不得重置后续复用同一端口的新连接"
    );
}
