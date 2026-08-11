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

/// 递归检查业务源码目录，确保测试属性和测试条件编译只存在于同级 tests 目录。
#[test]
fn productionSourceContainsNoTestOnlyMarkers() {
    assertSourceTreeContainsNoTestMarkers(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
    );
}

/// 遍历一个业务源码目录并报告包含测试标记的具体文件与标记，避免后续重构把测试代码混回 src。
fn assertSourceTreeContainsNoTestMarkers(sourceDirectory: &Path) {
    for entry in fs::read_dir(sourceDirectory).expect("读取业务源码目录失败") {
        let entry = entry.expect("读取业务源码条目失败");
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            assert!(
                !matches!(name.as_ref(), "test" | "tests"),
                "业务源码目录不得包含测试目录：{}",
                path.display()
            );
            assertSourceTreeContainsNoTestMarkers(&path);
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
        let source = fs::read_to_string(&path).expect("读取 Rust 业务源码失败");
        for marker in prohibitedTestMarkers {
            assert!(
                !source.contains(marker),
                "业务源码 {} 包含测试标记 {marker}",
                path.display()
            );
        }
    }
}
