#![allow(non_snake_case, non_upper_case_globals)]

use std::{
    io::Write,
    process::{Command, Stdio},
};

use tempfile::TempDir;

const generatedApplicationId: &str = "q01234567.r89abcdef.s01234567.t89abcdef";

/// CLI 必须只从标准输入读取秘密，失败输出不得回显账号、密码或完整秘密协议。
#[test]
fn secretInputNeverAppearsInPackagerDiagnostics() {
    let directory = TempDir::new().expect("创建 CLI 测试目录");
    let templatePath = directory.path().join("template.apk");
    std::fs::write(&templatePath, b"not-used").expect("写入占位模板");
    let mut child = Command::new(env!("CARGO_BIN_EXE_clientPackager"))
        .arg("package")
        .arg("--template")
        .arg(&templatePath)
        .arg("--output")
        .arg(directory.path().join("output.apk"))
        .arg("--signing-directory")
        .arg(directory.path().join("signing"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动独立打包器");
    let secretUsername = "U".repeat(256);
    let secretPassword = "never-print-this-password";
    let input = serde_json::json!({
        "applicationId": generatedApplicationId,
        "applicationName": "5A17C290E4B86D31",
        "nodeHost": "192.168.1.10",
        "nodePort": 1080,
        "username": secretUsername,
        "password": secretPassword,
        "rulesUrl": "http://192.168.1.10:19090/api/v1/client/routing.txt"
    })
    .to_string();
    child
        .stdin
        .take()
        .expect("取得打包器标准输入")
        .write_all(input.as_bytes())
        .expect("写入秘密协议");
    let output = child.wait_with_output().expect("等待独立打包器退出");
    assert!(!output.status.success());
    let diagnostic = String::from_utf8(output.stderr).expect("错误输出必须是 UTF-8");
    assert!(diagnostic.contains("账号长度"));
    assert!(!diagnostic.contains(secretPassword));
    assert!(!diagnostic.contains(&secretUsername));
    assert!(output.stdout.is_empty());
}
