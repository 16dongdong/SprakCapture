#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{fs, path::Path};

const prohibitedSourceMarkers: [&str; 5] = [
    "#[cfg(test)]",
    "#[test]",
    "#[tokio::test]",
    "cfg!(test)",
    "cfg(test)",
];

/// 验证所有后端 crate 的业务源码树只包含可发布实现，测试属性、测试条件分支和测试目录必须位于同级 tests 树。
#[test]
fn backendProductionSourceContainsNoTestArtifacts() {
    let backendRoot = Path::new(env!("CARGO_MANIFEST_DIR"));
    for crateDirectory in [
        backendRoot.to_path_buf(),
        backendRoot.join("Capture"),
        backendRoot.join("HttpProxy"),
        backendRoot.join("Location"),
        backendRoot.join("Socks5"),
    ] {
        assertSourceTreeContainsNoTestArtifacts(&crateDirectory.join("src"));
    }
}

/// 递归检查单个业务源码树；发现测试文件、测试目录或测试条件标记即列出准确路径并使测试失败。
fn assertSourceTreeContainsNoTestArtifacts(sourceDirectory: &Path) {
    for entry in fs::read_dir(sourceDirectory).expect("读取后端业务源码目录失败") {
        let entry = entry.expect("读取后端业务源码条目失败");
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            assert!(
                !matches!(name.as_ref(), "test" | "tests"),
                "业务源码目录不得包含测试目录：{}",
                path.display()
            );
            assertSourceTreeContainsNoTestArtifacts(&path);
            continue;
        }
        let normalizedName = name.to_ascii_lowercase();
        assert!(
            !normalizedName.contains(".test.") && !normalizedName.contains(".spec."),
            "业务源码目录不得包含测试文件：{}",
            path.display()
        );
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("读取后端 Rust 业务源码失败");
        for marker in prohibitedSourceMarkers {
            assert!(
                !source.contains(marker),
                "后端业务源码 {} 包含测试标记 {marker}",
                path.display()
            );
        }
    }
}
