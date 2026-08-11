#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread::JoinHandle,
    time::Duration,
};

use capture_mcp::controlClient::{ControlClient, ControlFailure, getMethod};
use serde_json::{Value, json};

/// 验证公开控制客户端仅将有效 JSON 成功响应和声明的结构化拒绝返回给调用方。
#[tokio::test]
async fn controlClientPreservesSuccessAndRejectionContracts() {
    let success = json!({ "serviceState": "running" });
    let (successBase, successFixture) =
        startResponseFixture(200, "application/json", success.clone());
    let client = ControlClient::new(&successBase).expect("创建控制客户端失败");
    assert_eq!(
        client
            .request(getMethod(), "api/v1/snapshot", None, "zh-Hans")
            .await
            .expect("成功响应应被解码"),
        success
    );
    successFixture.join().expect("成功 fixture 线程失败");

    let rejection = json!({
        "code": "serviceNotStoppable",
        "message": "服务当前不可停止",
        "messageKey": "error.serviceNotStoppable",
        "params": { "field": "listenPort" }
    });
    let (rejectionBase, rejectionFixture) =
        startResponseFixture(409, "application/json", rejection);
    let client = ControlClient::new(&rejectionBase).expect("创建控制客户端失败");
    match client
        .request(getMethod(), "api/v1/configuration", None, "en")
        .await
    {
        Err(ControlFailure::Rejected { statusCode, error }) => {
            assert_eq!(statusCode, 409);
            assert_eq!(error.code, "serviceNotStoppable");
            assert_eq!(error.messageKey, "error.serviceNotStoppable");
            assert_eq!(error.params["field"], "listenPort");
        }
        result => panic!("结构化拒绝必须保留允许字段：{result:?}"),
    }
    rejectionFixture.join().expect("拒绝 fixture 线程失败");
}

/// 验证非 JSON 成功正文不会被误解为业务成功，错误诊断只保留响应元数据。
#[tokio::test]
async fn controlClientRejectsInvalidSuccessBody() {
    let (controlBase, fixture) = startRawResponseFixture(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 8\r\nconnection: close\r\n\r\nnot-json"
            .to_owned(),
    );
    let client = ControlClient::new(&controlBase).expect("创建控制客户端失败");
    match client
        .request(getMethod(), "api/v1/snapshot", None, "en")
        .await
    {
        Err(ControlFailure::InvalidResponse {
            metadata: Some(metadata),
        }) => {
            assert_eq!(metadata.statusCode, 200);
            assert_eq!(metadata.contentType.as_deref(), Some("text/plain"));
            assert_eq!(metadata.contentLength, Some(8));
            assert!(metadata.contentDigest.is_some());
        }
        result => panic!("无效正文必须返回受限响应诊断：{result:?}"),
    }
    fixture.join().expect("无效正文 fixture 线程失败");
}

/// 验证声明超过 16 MiB 的响应在读取正文前被拒绝，避免 MCP 进程为异常控制页分配无界内存。
#[tokio::test]
async fn controlClientRejectsOversizedResponseBeforeBodyRead() {
    const oversizedLength: usize = 16 * 1024 * 1024 + 1;
    let (controlBase, fixture) = startRawResponseFixture(format!(
        "HTTP/1.1 502 Bad Gateway\r\ncontent-type: text/html\r\ncontent-length: {oversizedLength}\r\nconnection: close\r\n\r\n"
    ));
    let client = ControlClient::new(&controlBase).expect("创建控制客户端失败");
    match client
        .request(getMethod(), "api/v1/snapshot", None, "zh-Hans")
        .await
    {
        Err(ControlFailure::InvalidResponse {
            metadata: Some(metadata),
        }) => {
            assert_eq!(metadata.statusCode, 502);
            assert_eq!(metadata.contentLength, Some(oversizedLength as u64));
            assert_eq!(metadata.contentType.as_deref(), Some("text/html"));
            assert_eq!(metadata.contentDigest, None);
        }
        result => panic!("超限响应必须在读正文前被拒绝：{result:?}"),
    }
    fixture.join().expect("超限响应 fixture 线程失败");
}

/// 启动只响应一次的本地 HTTP fixture，并严格等待客户端至少发送完整请求头。
fn startResponseFixture(
    statusCode: u16,
    contentType: &str,
    body: Value,
) -> (String, JoinHandle<()>) {
    let reason = if statusCode == 200 { "OK" } else { "Conflict" };
    let serializedBody = body.to_string();
    startRawResponseFixture(format!(
        "HTTP/1.1 {statusCode} {reason}\r\ncontent-type: {contentType}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{serializedBody}",
        serializedBody.len()
    ))
}

/// 启动原始 HTTP 响应 fixture；用于验证客户端的协议解码和大小边界，而非模拟业务逻辑。
fn startRawResponseFixture(response: String) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定控制 fixture 失败");
    let address = listener.local_addr().expect("读取控制 fixture 地址失败");
    let task = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("接收控制客户端请求失败");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("设置控制 fixture 读取超时失败");
        let mut requestBytes = Vec::new();
        let mut buffer = [0_u8; 512];
        while !requestBytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let byteCount = stream.read(&mut buffer).expect("读取控制客户端请求失败");
            assert!(byteCount > 0, "控制客户端请求提前结束");
            requestBytes.extend_from_slice(&buffer[..byteCount]);
        }
        stream
            .write_all(response.as_bytes())
            .expect("写入控制 fixture 响应失败");
        stream.flush().expect("刷新控制 fixture 响应失败");
    });
    (format!("http://{address}"), task)
}
