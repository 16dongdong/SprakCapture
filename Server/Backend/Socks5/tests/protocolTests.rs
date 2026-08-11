#![allow(non_snake_case)]

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use socks5_core::{
    AuthenticationMode, Socks5Config, Socks5Error,
    address::{TargetAddress, TargetHost},
    config::{
        maximumConnections, maximumRelayBufferSize, maximumSessionHistoryLimit,
        maximumShutdownTimeoutMilliseconds, maximumTotalRelayBufferSize, maximumUdpRemoteLimit,
    },
    protocol::{
        decodeUdpPacket, encodeUdpPacket, negotiateAuthentication, negotiatePluginAuthentication,
        usernamePasswordVersion,
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 验证 IPv4 UDP 负载可无损往返且不会把 SOCKS5 头计入 payload。
#[test]
fn udpPacketRoundTripPreservesPayload() {
    let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 53);
    let payload = b"\x00dns-payload\xff";
    let encoded = encodeUdpPacket(source, payload).expect("UDP 编码应成功");
    let decoded = decodeUdpPacket(&encoded).expect("UDP 解码应成功");
    assert_eq!(
        decoded.destination,
        TargetAddress::fromSocketAddress(source)
    );
    assert_eq!(decoded.payload, payload);
}

/// 验证非零 FRAG 被明确拒绝，避免把不完整数据静默转发到目标。
#[test]
fn udpPacketRejectsUnsupportedFragment() {
    let packet = [0x00, 0x00, 0x01, 0x01, 127, 0, 0, 1, 0, 53];
    let error = decodeUdpPacket(&packet).expect_err("非零分片必须失败");
    assert!(error.to_string().contains("分片"));
}

/// 验证 RFC1929 成功响应与用户名结果，密码不会进入返回值或错误文本。
#[tokio::test]
async fn usernamePasswordAuthenticationSucceeds() {
    let (mut client, mut server) = tokio::io::duplex(256);
    let users = HashMap::from([("alice".to_owned(), "secret".to_owned())]);
    let authentication = tokio::spawn(async move {
        negotiateAuthentication(
            &mut server,
            &AuthenticationMode::UsernamePassword,
            &users,
            Duration::from_secs(1),
        )
        .await
    });

    client
        .write_all(&[0x05, 0x01, 0x02])
        .await
        .expect("写入方法协商");
    let mut methodReply = [0_u8; 2];
    client
        .read_exact(&mut methodReply)
        .await
        .expect("读取方法响应");
    assert_eq!(methodReply, [0x05, 0x02]);

    client
        .write_all(&[
            usernamePasswordVersion,
            5,
            b'a',
            b'l',
            b'i',
            b'c',
            b'e',
            6,
            b's',
            b'e',
            b'c',
            b'r',
            b'e',
            b't',
        ])
        .await
        .expect("写入认证字段");
    let mut authReply = [0_u8; 2];
    client
        .read_exact(&mut authReply)
        .await
        .expect("读取认证响应");
    assert_eq!(authReply, [usernamePasswordVersion, 0x00]);
    assert_eq!(
        authentication
            .await
            .expect("认证任务不应崩溃")
            .expect("认证应成功"),
        "alice"
    );
}

/// 验证插件认证回调收到完整凭据、可返回独立主体 ID，并由协议层生成标准 RFC1929 成功响应。
#[tokio::test]
async fn pluginAuthenticationControlsRfc1929Decision() {
    let (mut client, mut server) = tokio::io::duplex(256);
    let authentication = tokio::spawn(async move {
        negotiatePluginAuthentication(
            &mut server,
            Duration::from_secs(1),
            |username, password| async move {
                assert_eq!(username, "alice");
                assert_eq!(password, "plugin-secret");
                Some("principal-from-plugin".to_owned())
            },
        )
        .await
    });
    client
        .write_all(&[0x05, 0x01, 0x02])
        .await
        .expect("写入插件认证方法");
    let mut methodReply = [0_u8; 2];
    client
        .read_exact(&mut methodReply)
        .await
        .expect("读取插件认证方法响应");
    assert_eq!(methodReply, [0x05, 0x02]);
    client
        .write_all(&[
            usernamePasswordVersion,
            5,
            b'a',
            b'l',
            b'i',
            b'c',
            b'e',
            13,
            b'p',
            b'l',
            b'u',
            b'g',
            b'i',
            b'n',
            b'-',
            b's',
            b'e',
            b'c',
            b'r',
            b'e',
            b't',
        ])
        .await
        .expect("写入插件认证凭据");
    let mut authenticationReply = [0_u8; 2];
    client
        .read_exact(&mut authenticationReply)
        .await
        .expect("读取插件认证响应");
    assert_eq!(authenticationReply, [usernamePasswordVersion, 0x00]);
    assert_eq!(
        authentication
            .await
            .expect("插件认证任务不应崩溃")
            .expect("插件认证应成功"),
        "principal-from-plugin"
    );
}

/// 验证非法 UTF-8 用户名会先收到 RFC1929 失败状态，再由服务端终止认证流程。
#[tokio::test]
async fn usernamePasswordAuthenticationRejectsInvalidUsernameEncoding() {
    assertInvalidCredentialEncoding(&[usernamePasswordVersion, 1, 0xff, 1, b'x']).await;
}

/// 验证非法 UTF-8 口令同样先收到 RFC1929 失败状态，不以连接截断替代协议响应。
#[tokio::test]
async fn usernamePasswordAuthenticationRejectsInvalidPasswordEncoding() {
    assertInvalidCredentialEncoding(&[usernamePasswordVersion, 1, b'a', 1, 0xff]).await;
}

/// 驱动一次非法凭据字节串协商并验证失败响应顺序；输入必须包含完整 RFC1929 消息。
async fn assertInvalidCredentialEncoding(credentials: &[u8]) {
    let (mut client, mut server) = tokio::io::duplex(256);
    let users = HashMap::from([("a".to_owned(), "x".to_owned())]);
    let authentication = tokio::spawn(async move {
        negotiateAuthentication(
            &mut server,
            &AuthenticationMode::UsernamePassword,
            &users,
            Duration::from_secs(1),
        )
        .await
    });
    client
        .write_all(&[0x05, 0x01, 0x02])
        .await
        .expect("写入非法凭据测试方法");
    let mut methodReply = [0_u8; 2];
    client
        .read_exact(&mut methodReply)
        .await
        .expect("读取非法凭据测试方法响应");
    assert_eq!(methodReply, [0x05, 0x02]);
    client
        .write_all(credentials)
        .await
        .expect("写入非法 UTF-8 凭据");
    let mut authenticationReply = [0_u8; 2];
    client
        .read_exact(&mut authenticationReply)
        .await
        .expect("非法 UTF-8 凭据必须收到认证失败响应");
    assert_eq!(
        authenticationReply,
        [usernamePasswordVersion, 0x01],
        "服务端必须先回写 RFC1929 失败状态"
    );
    let error = authentication
        .await
        .expect("非法凭据认证任务不应 panic")
        .expect_err("非法 UTF-8 凭据必须失败");
    assert!(matches!(error, Socks5Error::AuthenticationFailed));
}

/// 验证 Socks5Config 调试输出只包含排序用户名，不包含任何原始口令。
#[test]
fn configurationDebugOutputRedactsPasswords() {
    let configuration = Socks5Config {
        authenticationMode: AuthenticationMode::UsernamePassword,
        users: HashMap::from([
            ("bob".to_owned(), "secondSecret".to_owned()),
            ("alice".to_owned(), "firstSecret".to_owned()),
        ]),
        ..Socks5Config::default()
    };
    let debugOutput = format!("{configuration:?}");
    assert!(debugOutput.contains("authenticationUsernames"));
    assert!(debugOutput.contains("alice"));
    assert!(debugOutput.contains("bob"));
    assert!(!debugOutput.contains("firstSecret"));
    assert!(!debugOutput.contains("secondSecret"));
    assert!(!debugOutput.contains("users:"));
}

/// 验证域名模型保持原始主机文本与端口，供协议测试共享。
#[test]
fn domainTargetKeepsWireIdentity() {
    let target = TargetAddress {
        host: TargetHost::Domain("localhost".to_owned()),
        port: 8080,
    };
    assert_eq!(target.toString(), "localhost:8080");
}

/// 验证插件连接元数据使用纯主机名而非会话展示地址，端口仍由独立字段表达，防止 `streamMatch.hosts` 与 SOCKS5 目标失配。
#[test]
fn targetHostSeparatesPortForPluginMatching() {
    let domainTarget = TargetAddress {
        host: TargetHost::Domain("gateway.example.test".to_owned()),
        port: 19_081,
    };
    let ipTarget = TargetAddress::fromSocketAddress(
        "127.0.0.1:19081".parse().expect("本地测试地址必须可解析"),
    );

    assert_eq!(domainTarget.hostString(), "gateway.example.test");
    assert_eq!(domainTarget.port, 19_081);
    assert_eq!(ipTarget.hostString(), "127.0.0.1");
    assert_eq!(ipTarget.port, 19_081);
}

/// 验证密码模式缺少账户时在绑定监听器前失败。
#[test]
fn configurationRejectsPasswordModeWithoutUsers() {
    let configuration = Socks5Config {
        authenticationMode: AuthenticationMode::UsernamePassword,
        ..Socks5Config::default()
    };
    let error = configuration.validate().expect_err("空账户配置必须失败");
    assert!(error.to_string().contains("至少需要一个账户"));
}

/// 验证 UDP 和转发资源下界由配置模型统一拒绝。
#[test]
fn configurationRejectsInvalidResourceLimits() {
    let configuration = Socks5Config {
        relayBufferSize: 512,
        udpMaxPacketSize: 128,
        ..Socks5Config::default()
    };
    assert!(configuration.validate().is_err());
}

/// 验证所有按配置扩张的资源都有明确上界，极端 usize 不会进入 Semaphore 或缓冲区构造。
#[test]
fn configurationRejectsResourceValuesAboveLimits() {
    for configuration in [
        Socks5Config {
            maxConnections: maximumConnections + 1,
            ..Socks5Config::default()
        },
        Socks5Config {
            relayBufferSize: maximumRelayBufferSize + 1,
            ..Socks5Config::default()
        },
        Socks5Config {
            udpRemoteLimit: maximumUdpRemoteLimit + 1,
            ..Socks5Config::default()
        },
        Socks5Config {
            sessionHistoryLimit: maximumSessionHistoryLimit + 1,
            ..Socks5Config::default()
        },
        Socks5Config {
            shutdownTimeoutMilliseconds: maximumShutdownTimeoutMilliseconds + 1,
            ..Socks5Config::default()
        },
    ] {
        assert!(configuration.validate().is_err());
    }
}

/// 验证 TCP 双向转发工作缓冲超过 448 MiB 时在启动前被拒绝，完整录制存储不参与该预算。
#[test]
fn configurationRejectsExcessiveTotalRelayMemory() {
    let configuration = Socks5Config {
        maxConnections: maximumTotalRelayBufferSize / maximumRelayBufferSize / 2 + 1,
        relayBufferSize: maximumRelayBufferSize,
        ..Socks5Config::default()
    };
    let error = configuration
        .validate()
        .expect_err("数据面转发缓冲区总预算超限必须失败");
    assert!(error.to_string().contains("总预算"));
}

/// 验证 TCP 监听与显式 UDP 绑定必须使用同一地址族，防止响应宣告未监听的中继地址。
#[test]
fn configurationRejectsMismatchedUdpAddressFamily() {
    let configuration = Socks5Config {
        listenHost: IpAddr::V4(Ipv4Addr::LOCALHOST),
        udpBindHost: "::1".to_owned(),
        ..Socks5Config::default()
    };
    let error = configuration
        .validate()
        .expect_err("跨地址族 UDP 绑定必须失败");
    assert!(error.to_string().contains("相同地址族"));
}
