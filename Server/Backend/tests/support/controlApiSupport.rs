#![allow(dead_code)]

use std::{
    net::{IpAddr, Ipv4Addr, TcpListener as StandardTcpListener},
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};
use tower::ServiceExt;

#[path = "controlState.rs"]
mod controlState;

pub(crate) use controlState::newControlState;

/// 预留一个当前可绑定端口；监听器立即释放，测试随后在同一进程使用该端口。
pub(crate) fn findAvailablePort() -> u16 {
    StandardTcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("预留控制测试端口")
        .local_addr()
        .expect("读取控制测试端口")
        .port()
}

/// 创建无认证的完整公开配置；调用方只覆盖与当前用例相关的字段。
pub(crate) fn configurationJson(listenPort: u16) -> Value {
    json!({
        "listenHost": "127.0.0.1",
        "listenPort": listenPort,
        "authenticationMode": "none",
        "maxConnections": 32,
        "connectTimeout": 2.0,
        "bindTimeout": 2.0,
        "idleTimeout": 10.0,
        "shutdownTimeout": 2.0,
        "readTimeout": 2.0,
        "relayBufferSize": 8192,
        "udpBindHost": "",
        "udpMaxPacketSize": 65507,
        "credentials": null,
        "httpProxy": {
            "enabled": true,
            "listenHost": "127.0.0.1",
            "listenPort": listenPort,
            "maxConnections": 16,
            "maxHeaderBytes": 16384,
            "maxCaptureBodyBytes": 65536,
            "connectTimeoutMilliseconds": 2000,
            "requestTimeoutMilliseconds": 5000,
            "headerReadTimeoutMilliseconds": 2000,
            "shutdownTimeoutMilliseconds": 2000
        },
        "upstreamProxy": {
            "enabled": false,
            "protocol": "socks5",
            "host": "127.0.0.1",
            "port": 1081,
            "username": "",
            "password": null
        },
        "processCapture": {
            "enabled": false,
            "processIds": [],
            "proxyPort": listenPort
        }
    })
}

/// 向内存 Router 发起 JSON 请求并返回状态码与解析后的响应对象。
pub(crate) async fn requestJson(
    router: axum::Router,
    method: Method,
    path: &str,
    body: Value,
) -> (axum::http::StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("构建控制请求");
    let response = router.oneshot(request).await.expect("执行控制请求");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("读取控制响应");
    let value = serde_json::from_slice(&bytes).expect("解析控制响应");
    (status, value)
}

/// 发送 listenPort=0 的本地化请求；测试只观察协商与结构化错误，不启动数据面。
pub(crate) async fn requestInvalidConfiguration(
    router: axum::Router,
    path: &str,
    acceptLanguage: &str,
) -> (axum::http::StatusCode, Value) {
    requestConfigurationBody(
        router,
        path,
        acceptLanguage,
        configurationJson(0).to_string(),
    )
    .await
}

/// 发送任意配置正文并读取结构化错误，覆盖 Serde 拒绝与数据面配置校验两类 detail 来源。
pub(crate) async fn requestConfigurationBody(
    router: axum::Router,
    path: &str,
    acceptLanguage: &str,
    body: String,
) -> (axum::http::StatusCode, Value) {
    let request = Request::builder()
        .method(Method::PUT)
        .uri(path)
        .header("content-type", "application/json")
        .header("accept-language", acceptLanguage)
        .body(Body::from(body))
        .expect("构建本地化控制请求");
    let response = router.oneshot(request).await.expect("执行本地化控制请求");
    parseJsonResponse(response).await
}

/// 读取一次 JSON 错误响应并保留状态码；测试通过该入口避免遗漏响应体解析失败。
pub(crate) async fn parseJsonResponse(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("读取本地化控制响应");
    let value = serde_json::from_slice(&bytes).expect("解析本地化控制响应");
    (status, value)
}

/// 轮询事务列表直到 HTTP 核心发布终态，避免测试用固定 sleep 掩盖慢机器调度差异。
pub(crate) async fn waitForCompletedTransaction(router: &axum::Router) -> Value {
    timeout(Duration::from_secs(5), async {
        loop {
            let (status, page) = requestJson(
                router.clone(),
                Method::GET,
                "/api/v1/transactions",
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            if let Some(transaction) = page["items"]
                .as_array()
                .and_then(|items| items.last())
                .filter(|transaction| transaction["status"] == "complete")
            {
                return transaction.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("HTTP 事务未在期限内完成")
}

/// 经真实 HTTP 代理发送一次关闭连接的 GET，并返回完整响应；连接、写入或读取失败直接终止验收。
pub(crate) async fn requestThroughHttpProxy(
    proxyAddress: std::net::SocketAddr,
    upstreamAddress: std::net::SocketAddr,
    path: &str,
) -> Vec<u8> {
    let mut client = TcpStream::connect(proxyAddress)
        .await
        .expect("连接 HTTP 代理");
    client
        .write_all(
            format!(
                "GET http://{upstreamAddress}/{path} HTTP/1.1\r\nHost: {upstreamAddress}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("发送代理请求");
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .expect("读取代理响应");
    response
}

/// 等待控制列表达到指定事务数；期限内未收敛说明录制事件或终态提交链路失效。
pub(crate) async fn waitForTransactionCount(router: &axum::Router, expectedCount: usize) -> Value {
    timeout(Duration::from_secs(2), async {
        loop {
            let (_, page) = requestJson(
                router.clone(),
                Method::GET,
                "/api/v1/transactions",
                json!({}),
            )
            .await;
            if page["total"] == expectedCount {
                return page;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("事务数量未在期限内收敛")
}

/// 通过真实 SOCKS5 CONNECT 回环一次负载，验证控制层归档使用实际会话和流量。
pub(crate) async fn relayTrafficThroughSocks(proxyAddress: std::net::SocketAddr) {
    let echoListener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("绑定历史测试回显端口");
    let echoAddress = echoListener.local_addr().expect("读取历史测试回显端口");
    let echoTask = tokio::spawn(async move {
        let (mut stream, _) = echoListener.accept().await.expect("接受历史测试连接");
        let mut payload = [0_u8; 7];
        stream
            .read_exact(&mut payload)
            .await
            .expect("读取历史测试负载");
        stream.write_all(&payload).await.expect("回写历史测试负载");
    });

    let mut client = TcpStream::connect(proxyAddress)
        .await
        .expect("连接历史测试 SOCKS5");
    client
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .expect("写入历史测试方法");
    let mut methodReply = [0_u8; 2];
    client
        .read_exact(&mut methodReply)
        .await
        .expect("读取历史测试方法");
    assert_eq!(methodReply, [0x05, 0x00]);
    let IpAddr::V4(targetIp) = echoAddress.ip() else {
        panic!("历史测试回显端点必须是 IPv4");
    };
    let mut request = vec![0x05, 0x01, 0x00, 0x01];
    request.extend_from_slice(&targetIp.octets());
    request.extend_from_slice(&echoAddress.port().to_be_bytes());
    client.write_all(&request).await.expect("写入历史测试请求");
    let mut connectReply = [0_u8; 10];
    client
        .read_exact(&mut connectReply)
        .await
        .expect("读取历史测试响应");
    assert_eq!(&connectReply[..4], &[0x05, 0x00, 0x00, 0x01]);
    client
        .write_all(b"history")
        .await
        .expect("写入历史测试代理负载");
    let mut echoed = [0_u8; 7];
    client
        .read_exact(&mut echoed)
        .await
        .expect("读取历史测试代理负载");
    assert_eq!(&echoed, b"history");
    drop(client);
    echoTask.await.expect("历史测试回显任务发生 panic");
}

const maximumWebSocketHandshakeBytes: usize = 16 * 1024;
const maximumWebSocketEventBytes: usize = 1024 * 1024;

/// 管理真实 TCP 控制面测试服务器；显式停止等待任务退出，析构路径仍会中止遗漏的监听任务以避免测试间端口泄漏。
pub(crate) struct ControlHttpTestServer {
    pub(crate) address: std::net::SocketAddr,
    task: Option<JoinHandle<()>>,
}

impl ControlHttpTestServer {
    /// 在临时回环端口运行完整 Axum 控制路由，使 WebSocket 验证经过真实 HTTP 升级而非内部事件通道。
    pub(crate) async fn start(router: axum::Router) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("绑定控制面 WebSocket 测试端口");
        let address = listener
            .local_addr()
            .expect("读取控制面 WebSocket 测试端口");
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("控制面 WebSocket 测试服务器不应异常退出");
        });
        Self {
            address,
            task: Some(task),
        }
    }

    /// 中止测试专用监听器并等待任务回收；业务服务由调用方先通过控制 API 正常停止。
    pub(crate) async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for ControlHttpTestServer {
    /// 断言失败时仍立即中止测试监听器，防止后续并行用例继承悬挂的本地端口。
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// 封装仅用于控制面事件验证的最小 WebSocket 客户端；客户端直接读取 Axum 的文本帧，覆盖实际升级和广播发送边界。
pub(crate) struct WebSocketEventClient {
    stream: TcpStream,
    bufferedBytes: Vec<u8>,
}

impl WebSocketEventClient {
    /// 完成 RFC 6455 HTTP 升级并保留握手后已经到达的帧字节，确保首个 snapshot 不会在头部读取阶段丢失。
    pub(crate) async fn connect(address: std::net::SocketAddr) -> Self {
        let mut client = Self {
            stream: TcpStream::connect(address)
                .await
                .expect("连接控制面 WebSocket 服务器"),
            bufferedBytes: Vec::new(),
        };
        client
            .stream
            .write_all(
                format!(
                    "GET /api/v1/events HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .expect("发送控制面 WebSocket 握手");
        let responseHeaders = client.readHandshakeHeaders().await;
        let responseText =
            String::from_utf8(responseHeaders).expect("控制面 WebSocket 握手头必须为 ASCII");
        assert!(
            responseText.starts_with("HTTP/1.1 101"),
            "控制面事件端点必须升级为 WebSocket：{responseText}"
        );
        assert!(
            responseText
                .to_ascii_lowercase()
                .contains("upgrade: websocket"),
            "升级响应必须声明 websocket 协议"
        );
        client
    }

    /// 等待指定判别字段的事件；无关事件持续消费以覆盖配置、录制和断点广播可以交错抵达的真实时序。
    pub(crate) async fn waitForEventType(&mut self, eventType: &str) -> Value {
        timeout(Duration::from_secs(2), async {
            loop {
                let event = self.readEvent().await;
                if event["type"] == eventType {
                    return event;
                }
            }
        })
        .await
        .expect("控制面 WebSocket 未在期限内收到目标事件")
    }

    /// 等待包含指定数量暂停项的断点事件；continue/abort 会同时触发显式与监视器广播，旧空队列事件不得覆盖后续新暂停验证。
    pub(crate) async fn waitForBreakpointEventCount(&mut self, expectedCount: usize) -> Value {
        timeout(Duration::from_secs(2), async {
            loop {
                let event = self.readEvent().await;
                let matchesCount = event["suspended"]
                    .as_array()
                    .is_some_and(|entries| entries.len() == expectedCount);
                if event["type"] == "breakpoints" && matchesCount {
                    return event;
                }
            }
        })
        .await
        .expect("控制面 WebSocket 未在期限内收到目标断点队列事件")
    }

    /// 读取并解析下一条控制面 JSON 文本事件；事件协议只使用文本消息，二进制消息必须在测试中立即暴露。
    pub(crate) async fn readEvent(&mut self) -> Value {
        let text = self.readTextFrame().await;
        serde_json::from_str(&text).expect("控制面 WebSocket 事件必须是 JSON")
    }

    /// 读取一个完整的服务端 WebSocket 文本帧，并正确跳过 Ping/Pong 控制帧以保持长连接符合协议。
    async fn readTextFrame(&mut self) -> String {
        loop {
            self.ensureBufferedBytes(2).await;
            let firstByte = self.bufferedBytes[0];
            let secondByte = self.bufferedBytes[1];
            assert_eq!(firstByte & 0x70, 0, "服务端 WebSocket 帧不应设置扩展保留位");
            assert_ne!(firstByte & 0x80, 0, "测试事件必须使用完整单帧消息");
            assert_eq!(
                secondByte & 0x80,
                0,
                "服务端 WebSocket 帧不得使用客户端掩码"
            );
            let (payloadLength, headerLength): (usize, usize) = match secondByte & 0x7f {
                length @ 0..=125 => (usize::from(length), 2_usize),
                126 => {
                    self.ensureBufferedBytes(4).await;
                    let length = u16::from_be_bytes([self.bufferedBytes[2], self.bufferedBytes[3]]);
                    (usize::from(length), 4_usize)
                }
                127 => {
                    self.ensureBufferedBytes(10).await;
                    let length = u64::from_be_bytes(
                        self.bufferedBytes[2..10]
                            .try_into()
                            .expect("WebSocket 扩展长度必须完整"),
                    );
                    (
                        usize::try_from(length).expect("WebSocket 事件长度超出本机地址空间"),
                        10_usize,
                    )
                }
                _ => unreachable!("七位 WebSocket 长度已在前序分支覆盖"),
            };
            assert!(
                payloadLength <= maximumWebSocketEventBytes,
                "控制面 WebSocket 单条事件超出测试上限"
            );
            let frameLength = headerLength
                .checked_add(payloadLength)
                .expect("WebSocket 帧长度计算溢出");
            self.ensureBufferedBytes(frameLength).await;
            let opcode = firstByte & 0x0f;
            let payload = self.bufferedBytes[headerLength..frameLength].to_vec();
            self.bufferedBytes.drain(..frameLength);
            match opcode {
                0x01 => {
                    return String::from_utf8(payload)
                        .expect("控制面 WebSocket 文本帧必须为 UTF-8");
                }
                0x08 => panic!("控制面 WebSocket 在事件抵达前关闭"),
                0x09 => self.sendPong(&payload).await,
                0x0a => {}
                _ => panic!("控制面 WebSocket 返回了非文本事件帧：{opcode}"),
            }
        }
    }

    /// 将指定数量的字节读入接收缓冲；每次 I/O 都带有限时，连接关闭或无事件时能给出明确测试失败。
    async fn ensureBufferedBytes(&mut self, requiredBytes: usize) {
        while self.bufferedBytes.len() < requiredBytes {
            let mut chunk = [0_u8; 4 * 1024];
            let count = timeout(Duration::from_secs(2), self.stream.read(&mut chunk))
                .await
                .expect("读取控制面 WebSocket 帧超时")
                .expect("读取控制面 WebSocket 帧失败");
            assert_ne!(count, 0, "控制面 WebSocket 在帧完成前关闭");
            self.bufferedBytes.extend_from_slice(&chunk[..count]);
        }
    }

    /// 读取 HTTP 升级响应头并将随后已经抵达的 WebSocket 帧留在内部缓冲，避免 TCP 合包丢失初始 snapshot。
    async fn readHandshakeHeaders(&mut self) -> Vec<u8> {
        loop {
            if let Some(headerEnd) = findHttpHeaderEnd(&self.bufferedBytes) {
                let remaining = self.bufferedBytes.split_off(headerEnd);
                return std::mem::replace(&mut self.bufferedBytes, remaining);
            }
            self.ensureBufferedBytes(self.bufferedBytes.len() + 1).await;
            assert!(
                self.bufferedBytes.len() <= maximumWebSocketHandshakeBytes,
                "控制面 WebSocket 握手头超出测试上限"
            );
        }
    }

    /// 按客户端掩码规则回复服务端 Ping，防止连接在等待后续工具或断点事件期间被协议层关闭。
    async fn sendPong(&mut self, payload: &[u8]) {
        assert!(payload.len() <= 125, "WebSocket Ping 负载超过协议上限");
        let mask = [0x1d_u8, 0x71, 0x56, 0x3a];
        let mut frame = Vec::with_capacity(payload.len() + 6);
        frame.push(0x8a);
        frame.push(0x80 | payload.len() as u8);
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % mask.len()]),
        );
        self.stream
            .write_all(&frame)
            .await
            .expect("回复控制面 WebSocket Ping");
    }
}

/// 查找 HTTP 响应头结束位置；返回值包含 `\r\n\r\n`，可直接作为 WebSocket 首帧的起始偏移。
fn findHttpHeaderEnd(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

/// 构建同时启用 SOCKS5 与 HTTP 正向代理的控制面配置；端口由调用方预留，避免与并行测试监听器冲突。
fn configurationWithHttpProxy(socksPort: u16) -> Value {
    let mut configuration = configurationJson(socksPort);
    configuration["httpProxy"] = json!({
        "enabled": true,
        "listenHost": "127.0.0.1",
        "listenPort": socksPort,
        "maxConnections": 16,
        "maxHeaderBytes": 16384,
        "maxCaptureBodyBytes": 65536,
        "connectTimeoutMilliseconds": 2000,
        "requestTimeoutMilliseconds": 5000,
        "headerReadTimeoutMilliseconds": 2000,
        "shutdownTimeoutMilliseconds": 2000
    });
    configuration
}

/// 通过公开控制 API 写入双监听配置并启动服务，返回真实 HTTP 代理绑定地址供断点与导出测试复用。
pub(crate) async fn startHttpProxyControlService(router: &axum::Router) -> std::net::SocketAddr {
    let socksPort = findAvailablePort();
    let (configurationStatus, _) = requestJson(
        router.clone(),
        Method::PUT,
        "/api/v1/configuration",
        configurationWithHttpProxy(socksPort),
    )
    .await;
    assert_eq!(configurationStatus, StatusCode::OK);
    let (startStatus, snapshot) = requestJson(
        router.clone(),
        Method::POST,
        "/api/v1/service/start",
        json!({}),
    )
    .await;
    assert_eq!(startStatus, StatusCode::OK);
    assert_eq!(snapshot["listeners"]["httpProxy"]["state"], "running");
    snapshot["listeners"]["httpProxy"]["boundEndpoint"]
        .as_str()
        .expect("启动快照必须包含 HTTP 代理地址")
        .parse()
        .expect("解析 HTTP 代理地址")
}

/// 按事务数量轮询断点队列端点；队列由真实代理工作任务写入，固定休眠会掩盖调度顺序问题。
pub(crate) async fn waitForBreakpointQueue(
    router: &axum::Router,
    expectedCount: usize,
) -> Vec<Value> {
    timeout(Duration::from_secs(2), async {
        loop {
            let (status, response) = requestJson(
                router.clone(),
                Method::GET,
                "/api/v1/breakpoints/suspended",
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let entries = response.as_array().expect("断点队列响应必须为数组").clone();
            if entries.len() == expectedCount {
                return entries;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("断点队列未在期限内达到目标数量")
}

/// 调用无正文的控制操作并验证端点没有意外写入 JSON 或文本负载，适用于断点 continue/abort 的 204 契约。
pub(crate) async fn requestNoContent(
    router: axum::Router,
    method: Method,
    path: &str,
    body: Value,
) -> StatusCode {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("构建无正文控制请求");
    let response = router.oneshot(request).await.expect("执行无正文控制请求");
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("读取无正文控制响应");
    assert!(body.is_empty(), "204 控制响应不得携带正文");
    status
}
