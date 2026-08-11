#![allow(non_snake_case, non_upper_case_globals)]

//! 无界面控制入口只经 HTTP 调用 proxyService，与 Web、Desktop 和 MCP 共享同一控制契约。

use std::{env, path::Path};

use reqwest::{Client, Method, Response};
use serde_json::{Value, json};

const defaultControlBase: &str = "http://127.0.0.1:17890";

/// CLI 入口解析命令并返回进程状态；所有业务错误均写入 stderr 且以非零状态结束。
#[tokio::main]
async fn main() {
    if let Err(message) = run().await {
        eprintln!("错误：{message}");
        std::process::exit(1);
    }
}

/// 通过唯一控制 API 实现 headless 子命令，禁止本地复制服务配置或直接写用户数据目录。
async fn run() -> Result<(), String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let arguments = arguments
        .into_iter()
        .filter(|argument| argument != "--json")
        .collect::<Vec<_>>();
    let base = env::var("CAPTURE_CONTROL_BASE").unwrap_or_else(|_| defaultControlBase.to_owned());
    let client = Client::new();
    let result = match arguments.as_slice() {
        [command, action] if command == "service" && action == "start" => {
            requestJson(&client, &base, Method::POST, "/api/v1/service/start", None).await
        }
        [command, action] if command == "service" && action == "stop" => {
            requestJson(&client, &base, Method::POST, "/api/v1/service/stop", None).await
        }
        [command] if command == "snapshot" => {
            requestJson(&client, &base, Method::GET, "/api/v1/snapshot", None).await
        }
        [command] if command == "health" => {
            requestJson(&client, &base, Method::GET, "/api/v1/health", None).await
        }
        [command] if command == "version" => {
            requestJson(&client, &base, Method::GET, "/api/v1/version", None).await
        }
        [command, action] if command == "record" && matches!(action.as_str(), "start" | "stop") => {
            updateRecordingState(&client, &base, action == "start").await
        }
        [command, tool, state]
            if command == "tools" && matches!(state.as_str(), "--enable" | "--disable") =>
        {
            updateToolEnabled(&client, &base, tool, state == "--enable").await
        }
        [command, formatFlag, format, outFlag, output]
            if command == "export" && formatFlag == "--format" && outFlag == "--out" =>
        {
            exportRecording(&client, &base, format, output).await
        }
        [command, action, formatFlag, format, outFlag, output]
            if command == "ssl"
                && action == "export"
                && formatFlag == "--format"
                && outFlag == "--out" =>
        {
            exportCertificate(&client, &base, format, output).await
        }
        _ => return Err(helpText()),
    }?;
    printResult(&result);
    Ok(())
}

/// 向控制面发送 JSON 请求；错误响应优先读取已按 Accept-Language 本地化的 message 字段。
async fn requestJson(
    client: &Client,
    base: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let request = client
        .request(method, format!("{}{}", base.trim_end_matches('/'), path))
        .header("Accept-Language", "zh-Hans");
    let response = match body {
        Some(body) => request.json(&body).send().await,
        None => request.send().await,
    }
    .map_err(|error| format!("控制接口不可用：{error}"))?;
    parseJsonResponse(response).await
}

/// 获取完整工具配置后只更新 enabled 字段，再提交回同一工具端点以保持人工与 CLI 语义一致。
async fn updateToolEnabled(
    client: &Client,
    base: &str,
    tool: &str,
    enabled: bool,
) -> Result<Value, String> {
    let path = format!("/api/v1/tools/{tool}");
    let mut configuration = requestJson(client, base, Method::GET, &path, None).await?;
    let object = configuration
        .as_object_mut()
        .ok_or_else(|| "工具配置响应不是对象".to_owned())?;
    object.insert("enabled".to_owned(), Value::Bool(enabled));
    requestJson(client, base, Method::PUT, &path, Some(configuration)).await
}

/// 切换录制会话状态；控制 API 的部分更新保留现有预算和忽略规则，不复制任何运行时状态。
async fn updateRecordingState(
    client: &Client,
    base: &str,
    recording: bool,
) -> Result<Value, String> {
    let state = if recording { "recording" } else { "paused" };
    requestJson(
        client,
        base,
        Method::PUT,
        "/api/v1/recording",
        Some(json!({ "state": state })),
    )
    .await
}

/// 导出 HAR 录制并写入显式新路径；已存在文件直接拒绝，避免覆盖用户导出物。
async fn exportRecording(
    client: &Client,
    base: &str,
    format: &str,
    output: &str,
) -> Result<Value, String> {
    if format != "har" {
        return Err("仅支持 --format har".to_owned());
    }
    let response = client
        .post(format!(
            "{}/api/v1/recording/export",
            base.trim_end_matches('/')
        ))
        .header("Accept-Language", "zh-Hans")
        .json(&json!({"format":"har","includeBodies":true}))
        .send()
        .await
        .map_err(|error| format!("导出请求失败：{error}"))?;
    let bytes = parseBytesResponse(response).await?;
    writeOutput(output, &bytes)?;
    Ok(json!({"format":"har","path":output,"bytesWritten":bytes.len()}))
}

/// 导出公开 SSL 根证书并写入用户指定新路径，返回值不含私钥材料。
async fn exportCertificate(
    client: &Client,
    base: &str,
    format: &str,
    output: &str,
) -> Result<Value, String> {
    if !matches!(format, "pem" | "cer") {
        return Err("证书格式必须为 pem 或 cer".to_owned());
    }
    let response = client
        .get(format!(
            "{}/api/v1/ssl/ca/export?format={format}",
            base.trim_end_matches('/')
        ))
        .header("Accept-Language", "zh-Hans")
        .send()
        .await
        .map_err(|error| format!("证书导出请求失败：{error}"))?;
    let bytes = parseBytesResponse(response).await?;
    writeOutput(output, &bytes)?;
    Ok(json!({"format":format,"path":output,"bytesWritten":bytes.len()}))
}

/// 将控制响应解码为 JSON；非成功状态一律优先输出结构化 message，避免 CLI 泄露内部响应文本。
async fn parseJsonResponse(response: Response) -> Result<Value, String> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取控制响应失败：{error}"))?;
    if status.is_success() {
        return serde_json::from_slice(&bytes).map_err(|_| "控制响应不是 JSON".to_owned());
    }
    Err(controlError(&bytes, status.as_u16()))
}

/// 读取二进制下载响应；失败时依旧按控制错误对象渲染，成功时不尝试解析正文。
async fn parseBytesResponse(response: Response) -> Result<Vec<u8>, String> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取控制响应失败：{error}"))?;
    if status.is_success() {
        return Ok(bytes.to_vec());
    }
    Err(controlError(&bytes, status.as_u16()))
}

/// 从结构化错误中提取已本地化 message；损坏响应只保留 HTTP 状态，不回显可能含敏感内容的正文。
fn controlError(bytes: &[u8], status: u16) -> String {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| format!("控制接口返回 HTTP {status}"))
}

/// 使用同目录临时文件提交全新输出；目标已存在时拒绝覆盖，写入失败时清理临时文件。
fn writeOutput(output: &str, bytes: &[u8]) -> Result<(), String> {
    let path = Path::new(output);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "输出路径缺少父目录".to_owned())?;
    if !parent.is_dir() {
        return Err("输出目录不存在".to_owned());
    }
    if path.exists() {
        return Err("输出文件已存在".to_owned());
    }
    let temporary = parent.join(format!(
        ".{}.pending",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "输出文件名无效".to_owned())?
    ));
    std::fs::write(&temporary, bytes).map_err(|error| format!("写入输出失败：{error}"))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("提交输出失败：{error}"));
    }
    Ok(())
}

/// 输出供脚本消费的稳定 JSON，普通模式仅输出紧凑 JSON 以避免不同命令出现不一致的人类格式。
fn printResult(result: &Value) {
    println!(
        "{}",
        serde_json::to_string(result).expect("CLI 结果序列化失败")
    );
}

/// 返回统一中文帮助，所有未识别命令都以错误退出而非静默成功。
fn helpText() -> String {
    "用法：capture service <start|stop> [--json]；capture record <start|stop>；capture snapshot|health|version [--json]；capture tools <toolId> <--enable|--disable>；capture export --format har --out 文件；capture ssl export --format <pem|cer> --out 文件".to_owned()
}
