#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::{fs, path::Path};

const prohibitedSourceMarkers: [&str; 4] =
    ["#[test]", "#[tokio::test]", "cfg!(test)", "mod tests {"];

/// 验证所有后端 crate 的业务源码树只包含可发布实现。
///
/// 运行上下文：测试从 `proxy-backend` 清单目录遍历后端各 crate 的 `src`；业务文件只允许通过
/// `#[path]` 声明引用同级 `tests` 树，不得内嵌测试函数、条件分支或测试目录。该函数没有参数，
/// 任一路径读取失败或发现混入源码的测试实现时立即失败，并报告准确文件和标记。
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

/// 递归检查单个业务源码树，并验证测试模块声明确实指向外部 `tests` 目录。
///
/// `sourceDirectory` 是当前 crate 的源码根；递归只读取 Rust 文本，不跟随源码树外路径。
/// 目录、文件、UTF-8 文本或外部模块声明异常时直接终止测试，禁止将内嵌测试伪装成路径模块。
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
        // Git 与编辑器可能采用不同换行符；结构规则比较语义行，不把 CRLF 误判为内嵌测试。
        let normalizedSource = source.replace("\r\n", "\n");
        let sourceLines = normalizedSource.lines().collect::<Vec<_>>();
        let testModuleCount = normalizedSource.matches("#[cfg(test)]").count();
        let externalModuleCount = sourceLines
            .windows(3)
            .filter(|lines| {
                lines[0] == "#[cfg(test)]"
                    && lines[1].starts_with("#[path = \"")
                    && lines[1].contains("/tests/")
                    && lines[2] == "mod tests;"
            })
            .count();
        assert_eq!(
            testModuleCount,
            externalModuleCount,
            "后端业务源码 {} 的测试条件只能引用外部 tests 目录",
            path.display()
        );
        assert_eq!(
            testModuleCount,
            normalizedSource.matches("mod tests;").count(),
            "后端业务源码 {} 的外部测试模块声明不完整",
            path.display()
        );
        for marker in prohibitedSourceMarkers {
            assert!(
                !normalizedSource.contains(marker),
                "后端业务源码 {} 包含测试标记 {marker}",
                path.display()
            );
        }
    }
}
