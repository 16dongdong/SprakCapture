#![allow(non_snake_case)]

use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use transport_core::{OutboundConnector, UpstreamProxyConfiguration, UpstreamProxyProtocol};

/// 构造关闭二级代理的直连配置；目标域名应由统一连接器在本机并行解析。
fn directConfiguration() -> UpstreamProxyConfiguration {
    UpstreamProxyConfiguration::default()
}

/// 构造启用状态的二级代理配置，测试只覆盖线协议，不在夹具中复用生产凭据。
fn upstreamConfiguration(protocol: UpstreamProxyProtocol, port: u16) -> UpstreamProxyConfiguration {
    UpstreamProxyConfiguration {
        enabled: true,
        protocol,
        host: "127.0.0.1".to_owned(),
        port,
        username: "fixtureUser".to_owned(),
        password: "fixturePassword".to_owned(),
    }
}

/// 验证 HTTP 二级代理收到 CONNECT authority 与 Basic 认证，并在成功头后留下纯净隧道。
#[tokio::test]
async fn httpConnectForwardsTargetAndAuthentication() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定夹具");
    let port = listener.local_addr().expect("读取地址").port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("接受连接");
        let mut request = Vec::new();
        loop {
            let mut next = [0_u8; 1];
            stream.read_exact(&mut next).await.expect("读取请求头");
            request.push(next[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8(request).expect("请求头编码");
        assert!(request.starts_with("CONNECT example.test:443 HTTP/1.1\r\n"));
        assert!(
            request.contains("Proxy-Authorization: Basic Zml4dHVyZVVzZXI6Zml4dHVyZVBhc3N3b3Jk\r\n")
        );
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nready")
            .await
            .expect("发送响应");
    });
    let connector = OutboundConnector::new(
        upstreamConfiguration(UpstreamProxyProtocol::Http, port),
        Duration::from_secs(2),
    )
    .expect("构建连接器");
    let mut stream = connector
        .connect("example.test", 443)
        .await
        .expect("建立 CONNECT 隧道");
    let mut ready = [0_u8; 5];
    stream.read_exact(&mut ready).await.expect("读取隧道数据");
    assert_eq!(&ready, b"ready");
    server.await.expect("等待夹具");
}

/// 验证 SOCKS5 二级代理完成方法协商、RFC 1929 认证和域名型 CONNECT，且完整消费绑定地址。
#[tokio::test]
async fn socks5ConnectUsesRemoteDomainResolution() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定夹具");
    let port = listener.local_addr().expect("读取地址").port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("接受连接");
        let mut methods = [0_u8; 4];
        stream.read_exact(&mut methods).await.expect("读取方法");
        assert_eq!(methods, [0x05, 0x02, 0x00, 0x02]);
        stream.write_all(&[0x05, 0x02]).await.expect("选择认证");
        let mut authHeader = [0_u8; 2];
        stream
            .read_exact(&mut authHeader)
            .await
            .expect("读取认证头");
        let mut username = vec![0_u8; authHeader[1] as usize];
        stream.read_exact(&mut username).await.expect("读取用户名");
        let mut passwordLength = [0_u8; 1];
        stream
            .read_exact(&mut passwordLength)
            .await
            .expect("读取口令长度");
        let mut password = vec![0_u8; passwordLength[0] as usize];
        stream.read_exact(&mut password).await.expect("读取口令");
        assert_eq!(&username, b"fixtureUser");
        assert_eq!(&password, b"fixturePassword");
        stream.write_all(&[0x01, 0x00]).await.expect("认证成功");
        let mut requestHeader = [0_u8; 5];
        stream
            .read_exact(&mut requestHeader)
            .await
            .expect("读取请求头");
        assert_eq!(&requestHeader[..4], &[0x05, 0x01, 0x00, 0x03]);
        let mut host = vec![0_u8; requestHeader[4] as usize];
        stream.read_exact(&mut host).await.expect("读取目标主机");
        let mut targetPort = [0_u8; 2];
        stream
            .read_exact(&mut targetPort)
            .await
            .expect("读取目标端口");
        assert_eq!(&host, b"example.test");
        assert_eq!(u16::from_be_bytes(targetPort), 8443);
        stream
            .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x1f, 0x90])
            .await
            .expect("发送 CONNECT 响应");
        stream.write_all(b"ready").await.expect("发送隧道数据");
    });
    let connector = OutboundConnector::new(
        upstreamConfiguration(UpstreamProxyProtocol::Socks5, port),
        Duration::from_secs(2),
    )
    .expect("构建连接器");
    let mut stream = connector
        .connect("example.test", 8443)
        .await
        .expect("建立 SOCKS5 隧道");
    let mut ready = [0_u8; 5];
    stream.read_exact(&mut ready).await.expect("读取隧道数据");
    assert_eq!(&ready, b"ready");
    server.await.expect("等待夹具");
}

/// 验证直连域名存在 IPv4/IPv6 等多个候选时仍能选中实际可达监听器，不被首个失败地址阻塞。
#[tokio::test]
async fn directConnectUsesReachableResolvedAddress() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定 IPv4 夹具");
    let port = listener.local_addr().expect("读取夹具地址").port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("接受直连连接");
        stream.write_all(b"ready").await.expect("发送直连响应");
    });
    let connector = OutboundConnector::new(directConfiguration(), Duration::from_secs(2))
        .expect("构建直连连接器");
    let mut stream = connector
        .connect("localhost", port)
        .await
        .expect("应选中 localhost 的可达 IPv4 地址");
    let mut ready = [0_u8; 5];
    stream.read_exact(&mut ready).await.expect("读取直连响应");
    assert_eq!(&ready, b"ready");
    server.await.expect("等待直连夹具");
}
