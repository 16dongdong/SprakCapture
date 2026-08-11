use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::CONTENT_TYPE},
};
use proxy_backend::controlApi::{ControlState, createControlRouter};
use serde_json::Value;
use tempfile::tempdir;
use tower::ServiceExt;

const BOUNDARY: &str = "capture-map-local-import-boundary";

/// 向 multipart 正文追加文本字段；测试固定使用 CRLF，以覆盖浏览器 FormData 的标准线协议。
fn append_text_field(body: &mut Vec<u8>, name: &str, value: &str) {
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

/// 向 multipart 正文追加文件字段；filename 只承载显示名，实际落盘路径由前置 path 字段决定。
fn append_file_field(body: &mut Vec<u8>, filename: &str, bytes: &[u8]) {
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
}

/// 完成 multipart 正文并构造导入请求；失败语义由实际 Router 生成，测试不绕过来源与正文限制中间件。
fn import_request(body: Vec<u8>) -> Request<Body> {
    let mut completed_body = body;
    completed_body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri("/api/v1/tools/mapLocal/import")
        .header(
            CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(completed_body))
        .expect("测试请求必须可构造")
}

/// 目录导入必须保留浏览器相对层级，并返回能够直接写入 Map Local 规则的正斜杠路径。
#[tokio::test]
async fn imports_selected_directory_into_managed_mapping_root() {
    let data_directory = tempdir().expect("测试数据目录必须可创建");
    let state = ControlState::newWithDataDirectory(data_directory.path())
        .await
        .expect("控制状态必须可初始化");
    let router = createControlRouter(state);
    let mut body = Vec::new();
    append_text_field(&mut body, "directory", "true");
    append_text_field(&mut body, "path", "site/index.html");
    append_file_field(&mut body, "index.html", b"<h1>index</h1>");
    append_text_field(&mut body, "path", "site/assets/app.js");
    append_file_field(&mut body, "app.js", b"console.log('ok');");

    let response = router
        .oneshot(import_request(body))
        .await
        .expect("导入请求必须完成");
    assert_eq!(response.status(), StatusCode::OK);
    let response_body = to_bytes(response.into_body(), 16 * 1024)
        .await
        .expect("导入响应必须可读取");
    let payload: Value = serde_json::from_slice(&response_body).expect("导入响应必须是 JSON");
    let local_path = payload["localPath"].as_str().expect("响应必须包含相对路径");
    assert!(local_path.starts_with("imports/"));
    assert!(local_path.ends_with("/site"));
    assert_eq!(payload["fileCount"], 2);
    assert_eq!(payload["totalBytes"], 32);

    let imported_root = data_directory.path().join("mappings").join(local_path);
    assert_eq!(
        std::fs::read(imported_root.join("index.html")).expect("首页必须已导入"),
        b"<h1>index</h1>"
    );
    assert_eq!(
        std::fs::read(imported_root.join("assets/app.js")).expect("脚本必须保留目录层级"),
        b"console.log('ok');"
    );
}

/// 父级跳转必须在任何文件发布前返回 400，且不得在受管映射根之外产生文件。
#[tokio::test]
async fn rejects_traversal_before_publishing_imported_file() {
    let data_directory = tempdir().expect("测试数据目录必须可创建");
    let state = ControlState::newWithDataDirectory(data_directory.path())
        .await
        .expect("控制状态必须可初始化");
    let router = createControlRouter(state);
    let mut body = Vec::new();
    append_text_field(&mut body, "directory", "false");
    append_text_field(&mut body, "path", "../outside.txt");
    append_file_field(&mut body, "outside.txt", b"blocked");

    let response = router
        .oneshot(import_request(body))
        .await
        .expect("拒绝响应必须完成");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!data_directory.path().join("outside.txt").exists());
}
