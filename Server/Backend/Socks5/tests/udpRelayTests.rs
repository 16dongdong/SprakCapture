#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::net::{Ipv4Addr, SocketAddr};

use tokio::sync::mpsc;

use socks5_core::{
    registry::SessionRegistry,
    udpRelay::{RemoteDatagram, queueRemoteDatagram, remoteResponseQueueCapacity},
};

/// 验证 UDP 响应队列固定为小容量，满队列时不等待并把被丢弃数据报计入指标。
#[test]
fn fullRemoteResponseQueueDropsAndCountsDatagram() {
    let registry = SessionRegistry::new(1);
    let (sender, _receiver) = mpsc::channel::<RemoteDatagram>(remoteResponseQueueCapacity);
    let source = SocketAddr::from((Ipv4Addr::LOCALHOST, 53));
    for _ in 0..remoteResponseQueueCapacity {
        assert!(queueRemoteDatagram(
            &sender,
            &registry,
            (vec![0_u8; 1], source)
        ));
    }
    assert!(queueRemoteDatagram(
        &sender,
        &registry,
        (vec![0_u8; 1], source)
    ));
    assert_eq!(registry.metrics().droppedUdpPackets, 1);
}
