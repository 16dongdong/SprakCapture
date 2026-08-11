#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::path::Path;

use serde_json::Value;

use capture_core::{
    BeginTransaction, BodyWrite, HeaderField, MessageSide, RecordingConfiguration,
    TransactionCompletion, TransactionProtocol, currentTimeMilliseconds,
};

use capture_core::{HarExportRequest, buildHarExport};

/// 创建隔离的临时捕获会话，供 HAR 导出边界测试使用。
async fn session(directory: &Path) -> capture_core::RecordingSession {
    capture_core::RecordingSession::new(RecordingConfiguration {
        spillDirectory: directory.join("capture"),
        ..RecordingConfiguration::default()
    })
    .await
    .expect("session creates")
}

/// 写入包含头部、正文和终态的测试事务，覆盖文本与二进制导出路径。
async fn recordTransaction(
    session: &capture_core::RecordingSession,
    path: &str,
    requestBody: Vec<u8>,
    responseBody: Vec<u8>,
    contentType: &str,
) -> String {
    let transactionId = session
        .beginTransaction(BeginTransaction {
            protocol: TransactionProtocol::Http,
            method: "POST".to_owned(),
            location: location_core::ResolvedLocation {
                protocol: "http".to_owned(),
                host: "example.test".to_owned(),
                port: 80,
                path: path.to_owned(),
                query: "page=1&sort=asc".to_owned(),
                display: format!("http://example.test{path}?page=1&sort=asc"),
            },
            clientAddress: "127.0.0.1:7000".to_owned(),
            clientProcessName: None,
            clientProcessId: None,
            contentType: contentType.to_owned(),
            startAtMilliseconds: currentTimeMilliseconds(),
        })
        .await
        .expect("begin")
        .expect("recording active");
    session
        .storeHeaders(
            &transactionId,
            MessageSide::Request,
            vec![HeaderField {
                name: "content-type".to_owned(),
                value: contentType.to_owned(),
            }],
        )
        .await
        .expect("request headers");
    session
        .storeHeaders(
            &transactionId,
            MessageSide::Response,
            vec![
                HeaderField {
                    name: "content-type".to_owned(),
                    value: contentType.to_owned(),
                },
                HeaderField {
                    name: "location".to_owned(),
                    value: "/next".to_owned(),
                },
            ],
        )
        .await
        .expect("response headers");
    session
        .storeBody(
            &transactionId,
            MessageSide::Request,
            BodyWrite {
                originalBytes: requestBody.len() as u64,
                bytes: requestBody,
                contentType: contentType.to_owned(),
                encoding: "utf-8".to_owned(),
            },
        )
        .await
        .expect("request body");
    session
        .storeBody(
            &transactionId,
            MessageSide::Response,
            BodyWrite {
                originalBytes: responseBody.len() as u64,
                bytes: responseBody,
                contentType: contentType.to_owned(),
                encoding: "binary".to_owned(),
            },
        )
        .await
        .expect("response body");
    session
        .commit(
            &transactionId,
            TransactionCompletion {
                statusCode: 200,
                endAtMilliseconds: currentTimeMilliseconds(),
                contentType: contentType.to_owned(),
            },
        )
        .await
        .expect("commit");
    transactionId
}

#[tokio::test]
/// 验证指定事务选择、正文输出和 base64 二进制编码。
async fn exportsSelectedTransactionsWithBodies() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let recording = session(directory.path()).await;
    let first = recordTransaction(
        &recording,
        "/first",
        br#"{"request":true}"#.to_vec(),
        br#"{"response":true}"#.to_vec(),
        "application/json",
    )
    .await;
    let second = recordTransaction(
        &recording,
        "/second",
        vec![0, 1, 2],
        vec![0, 255, 1],
        "application/octet-stream",
    )
    .await;
    let archive = buildHarExport(
        &recording,
        &HarExportRequest {
            includeBodies: true,
            transactionIds: vec![second.clone()],
        },
    )
    .await
    .expect("export");
    let json = serde_json::to_value(&archive).expect("json");
    assert_eq!(json["log"]["version"], "1.2");
    assert_eq!(json["log"]["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(
        json["log"]["entries"][0]["_capture"]["transactionId"],
        second
    );
    assert_eq!(
        json["log"]["entries"][0]["request"]["postData"]["encoding"],
        "base64"
    );
    assert_eq!(
        json["log"]["entries"][0]["response"]["content"]["encoding"],
        "base64"
    );
    assert_ne!(
        first,
        json["log"]["entries"][0]["_capture"]["transactionId"]
    );
    let _: Value = json;
}

#[tokio::test]
/// 验证关闭正文导出时不写文本字段，空选择范围导出全部事务。
async fn omitsBodyTextWhenDisabledAndTreatsEmptySelectionAsAll() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let recording = session(directory.path()).await;
    recordTransaction(
        &recording,
        "/only",
        br#"{"request":true}"#.to_vec(),
        br#"{"response":true}"#.to_vec(),
        "application/json",
    )
    .await;
    let archive = recording
        .buildHarExport(HarExportRequest::default())
        .await
        .expect("export");
    let json = serde_json::to_value(&archive).expect("json");
    assert_eq!(json["log"]["entries"].as_array().expect("entries").len(), 1);
    assert!(
        json["log"]["entries"][0]["request"]
            .get("postData")
            .is_none()
    );
    assert!(
        json["log"]["entries"][0]["response"]["content"]
            .get("text")
            .is_none()
    );
}
