#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{fs, path::Path};

const prohibitedTestMarkers: [&str; 5] = [
    "#[cfg(test)]",
    "#[test]",
    "#[tokio::test]",
    "cfg!(test)",
    "cfg(test)",
];

/// 递归检查 MCP 业务源码目录，确保测试属性和测试条件编译只存在于同级 tests 目录。
#[test]
fn productionSourceContainsNoTestOnlyMarkers() {
    let sourceDirectory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assertSourceTreeContainsNoTestMarkers(&sourceDirectory);
}

/// 遍历业务源码并报告违规文件，防止后续重构把测试夹具或测试分支混回运行时代码。
fn assertSourceTreeContainsNoTestMarkers(sourceDirectory: &Path) {
    for entry in fs::read_dir(sourceDirectory).expect("读取 MCP 业务源码目录失败") {
        let entry = entry.expect("读取 MCP 业务源码条目失败");
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            assert!(
                !matches!(name.as_ref(), "test" | "tests"),
                "MCP 业务源码目录不得包含测试目录：{}",
                path.display()
            );
            assertSourceTreeContainsNoTestMarkers(&path);
            continue;
        }
        let normalizedName = name.to_ascii_lowercase();
        assert!(
            !normalizedName.contains(".test.") && !normalizedName.contains(".spec."),
            "MCP 业务源码目录不得包含测试文件：{}",
            path.display()
        );
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("读取 MCP Rust 业务源码失败");
        for marker in prohibitedTestMarkers {
            assert!(
                !source.contains(marker),
                "MCP 业务源码 {} 包含测试标记 {marker}",
                path.display()
            );
        }
    }
}
