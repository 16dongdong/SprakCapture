#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{net::SocketAddr, time::Duration};

use account_service::{
    AccountDomainService, AccountPolicy, AccountServerConfig, AccountStore, CreateAccountRequest,
    startAccountService,
};
use socks5_core::{
    AccountServiceClientConfig, AuthenticationMode, FusedProxyDependencies, FusedProxyOptions,
    Socks5Config, startFusedProxyServer,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const internalToken: &str = "0123456789abcdef0123456789abcdef";

/// 创建真实 SQLite 账号服务和固定密码账号，供 SOCKS5 数据面集成测试复用。
///
/// 运行上下文：夹具只调用账号服务内部租约接口，不加载远程工作台，因此控制地址使用本地固定值，
/// Web 资源目录保持为空。函数没有输入参数，返回临时数据库、运行实例、观察服务和账号标识；
/// 数据库、账号或监听器初始化失败时直接终止当前测试，避免用不完整夹具产生伪通过。
async fn startAccountFixture() -> (
    TempDir,
    account_service::RunningAccountService,
    AccountDomainService,
    String,
) {
    let directory = tempfile::tempdir().expect("创建账号测试目录");
    let databasePath = directory.path().join("accounts.sqlite3");
    let store = AccountStore::open(&databasePath).expect("创建账号数据库");
    let account = store
        .createAccount(&CreateAccountRequest {
            username: "fixture-user".to_owned(),
            password: Some("fixture-password".to_owned()),
            policy: AccountPolicy {
                maxUploadBytesPerSecond: -1,
                maxDownloadBytesPerSecond: -1,
                maxConnections: 2,
                maxOnlineIps: 1,
                expiresAt: -1,
            },
            remark: None,
        })
        .expect("创建测试账号");
    drop(store);
    let running = startAccountService(AccountServerConfig {
        databasePath: databasePath.clone(),
        publicAddress: "127.0.0.1:0".parse().expect("解析公共地址"),
        internalAddress: "127.0.0.1:0".parse().expect("解析内部地址"),
        internalToken: internalToken.to_owned(),
        controlBaseUrl: "http://127.0.0.1:17890".to_owned(),
        webAssetsDirectory: None,
    })
    .await
    .expect("启动账号服务");
    let observer =
        AccountDomainService::new(AccountStore::open(&databasePath).expect("打开观察数据库"));
    (directory, running, observer, account.accountId)
}

/// 启动回显上游并返回地址；任务在接受一次连接并完成 EOF 后自然退出。
async fn startEchoServer() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定回显服务");
    let address = listener.local_addr().expect("读取回显地址");
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("接受回显连接");
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.expect("读取回显请求");
        stream.write_all(&bytes).await.expect("写入回显响应");
    });
    (address, task)
}

/// 完成 RFC1929 认证和 CONNECT，失败状态由调用测试直接断言。
async fn authenticateAndConnect(
    proxyAddress: SocketAddr,
    username: &str,
    password: &str,
    target: SocketAddr,
) -> (TcpStream, [u8; 2]) {
    let mut stream = TcpStream::connect(proxyAddress).await.expect("连接 SOCKS5");
    stream
        .write_all(&[0x05, 0x01, 0x02])
        .await
        .expect("发送方法");
    let mut method = [0_u8; 2];
    stream.read_exact(&mut method).await.expect("读取方法");
    assert_eq!(method, [0x05, 0x02]);
    let mut authentication = vec![0x01, username.len() as u8];
    authentication.extend_from_slice(username.as_bytes());
    authentication.push(password.len() as u8);
    authentication.extend_from_slice(password.as_bytes());
    stream
        .write_all(&authentication)
        .await
        .expect("发送账号密码");
    let mut status = [0_u8; 2];
    stream.read_exact(&mut status).await.expect("读取认证状态");
    if status == [0x01, 0x00] {
        let std::net::IpAddr::V4(targetIp) = target.ip() else {
            panic!("测试目标必须使用 IPv4");
        };
        let mut request = vec![0x05, 0x01, 0x00, 0x01];
        request.extend_from_slice(&targetIp.octets());
        request.extend_from_slice(&target.port().to_be_bytes());
        stream.write_all(&request).await.expect("发送 CONNECT");
        let mut reply = [0_u8; 10];
        stream
            .read_exact(&mut reply)
            .await
            .expect("读取 CONNECT 响应");
        assert_eq!(reply[1], 0x00);
    }
    (stream, status)
}

/// 验证真实外部认证、租约释放和流量持久化贯穿 SOCKS5 TCP 数据面。
#[tokio::test]
async fn authenticatesRelaysAndFinalizesExternalLease() {
    let (_directory, accountService, observer, accountId) = startAccountFixture().await;
    let (echoAddress, echoTask) = startEchoServer().await;
    let mut config = Socks5Config {
        listenPort: 0,
        authenticationMode: AuthenticationMode::AccountService,
        readTimeoutMilliseconds: 2_000,
        idleTimeoutMilliseconds: 2_000,
        ..Socks5Config::default()
    };
    config.users.clear();
    let server = startFusedProxyServer(
        config,
        FusedProxyDependencies {
            pluginHost: plugin_host::PluginHost::disabled(),
            tunnelInterceptor: None,
            addressOverride: None,
            protocolHandler: None,
            outboundConnector: None,
        },
        FusedProxyOptions {
            enableInternalCaptureListener: false,
            accountServiceConfig: Some(AccountServiceClientConfig {
                endpoint: format!("http://{}", accountService.internalAddress),
                internalToken: internalToken.to_owned(),
                synchronizationIntervalMilliseconds: 250,
                requestTimeoutMilliseconds: 2_000,
            }),
        },
    )
    .await
    .expect("启动多账号 SOCKS5");
    let (mut stream, status) = authenticateAndConnect(
        server.boundAddress(),
        "fixture-user",
        "fixture-password",
        echoAddress,
    )
    .await;
    assert_eq!(status, [0x01, 0x00]);
    stream
        .write_all(b"account-traffic")
        .await
        .expect("写入代理流量");
    stream.shutdown().await.expect("结束上传");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("读取代理回显");
    assert_eq!(response, b"account-traffic");
    echoTask.await.expect("等待回显服务");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let usage = observer.account(&accountId).expect("读取账号用量");
    assert_eq!(usage.uploadedBytes, 15);
    assert_eq!(usage.downloadedBytes, 15);
    server.stop().await;
    accountService.stop().await.expect("停止账号服务");
}

/// 验证密码错误时只返回 RFC1929 失败，并且不会创建连接计数。
#[tokio::test]
async fn rejectsInvalidExternalPassword() {
    let (_directory, accountService, observer, accountId) = startAccountFixture().await;
    let mut config = Socks5Config {
        listenPort: 0,
        authenticationMode: AuthenticationMode::AccountService,
        ..Socks5Config::default()
    };
    config.users.clear();
    let server = startFusedProxyServer(
        config,
        FusedProxyDependencies {
            pluginHost: plugin_host::PluginHost::disabled(),
            tunnelInterceptor: None,
            addressOverride: None,
            protocolHandler: None,
            outboundConnector: None,
        },
        FusedProxyOptions {
            enableInternalCaptureListener: false,
            accountServiceConfig: Some(AccountServiceClientConfig {
                endpoint: format!("http://{}", accountService.internalAddress),
                internalToken: internalToken.to_owned(),
                synchronizationIntervalMilliseconds: 250,
                requestTimeoutMilliseconds: 2_000,
            }),
        },
    )
    .await
    .expect("启动多账号 SOCKS5");
    let (_stream, status) = authenticateAndConnect(
        server.boundAddress(),
        "fixture-user",
        "wrong-password",
        "127.0.0.1:9".parse().expect("解析占位目标"),
    )
    .await;
    assert_eq!(status, [0x01, 0x01]);
    let usage = observer.account(&accountId).expect("读取账号用量");
    assert_eq!(usage.uploadedBytes, 0);
    assert_eq!(usage.downloadedBytes, 0);
    server.stop().await;
    accountService.stop().await.expect("停止账号服务");
}
