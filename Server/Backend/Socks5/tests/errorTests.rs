#![allow(non_snake_case)]

use std::io;

use socks5_core::Socks5Error;

/// 已建立转发后的常见 TCP 关闭必须归为正常终止，避免有有效上下行字节的事务被误标失败。
#[test]
fn normalRelayTerminationRecognizesTransportClosure() {
    assert!(Socks5Error::RelayIdleTimeout.isNormalRelayTermination());
    for kind in [
        io::ErrorKind::BrokenPipe,
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::NotConnected,
        io::ErrorKind::UnexpectedEof,
    ] {
        let error = Socks5Error::Io(io::Error::new(kind, "连接已结束"));
        assert!(error.isNormalRelayTermination());
    }
}

/// 协议读取超时、认证失败和普通 I/O 故障仍需进入失败状态，不能被空闲回收规则扩大吞掉。
#[test]
fn normalRelayTerminationRejectsOperationalFailures() {
    assert!(!Socks5Error::Timeout("认证读取").isNormalRelayTermination());
    assert!(!Socks5Error::AuthenticationFailed.isNormalRelayTermination());
    assert!(
        !Socks5Error::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "访问被拒绝"
        ))
        .isNormalRelayTermination()
    );
}
