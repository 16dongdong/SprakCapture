#![allow(non_snake_case, non_upper_case_globals)]

use std::{env, fs, path::PathBuf};

const distributionFileNames: [&str; 3] =
    ["WinDivert.dll", "WinDivert64.sys", "LICENSE.WinDivert.txt"];

/// 把官方 WinDivert 动态库、签名驱动与许可证部署到普通二进制和集成测试目录。
///
/// Cargo 将应用二进制放在 `target/{profile}`，集成测试放在其 `deps` 子目录；
/// WinDivert 必须从当前可执行文件同目录加载 DLL 和驱动，分发物还必须携带许可证文本。
/// 任一文件复制失败都会终止构建，避免安装包或测试在运行期才以缺少组件的模糊错误失败。
fn main() {
    for distributionFileName in distributionFileNames {
        println!("cargo:rerun-if-changed=vendor/{distributionFileName}");
    }

    let architecture = env::var("CARGO_CFG_TARGET_ARCH").expect("缺少目标架构");
    assert_eq!(
        architecture, "x86_64",
        "当前仅随包提供 WinDivert x64 运行库"
    );

    let manifestDirectory = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("缺少清单目录"));
    let outputDirectory = PathBuf::from(env::var_os("OUT_DIR").expect("缺少 OUT_DIR"));
    let executableDirectory = outputDirectory
        .ancestors()
        .nth(3)
        .expect("Cargo 输出目录层级异常");
    let testExecutableDirectory = executableDirectory.join("deps");
    fs::create_dir_all(&testExecutableDirectory).expect("创建集成测试输出目录失败");

    for destinationDirectory in [executableDirectory.to_path_buf(), testExecutableDirectory] {
        for distributionFileName in distributionFileNames {
            let distributionSource = manifestDirectory.join("vendor").join(distributionFileName);
            fs::copy(
                &distributionSource,
                destinationDirectory.join(distributionFileName),
            )
            .unwrap_or_else(|error| panic!("复制 {distributionFileName} 失败：{error}"));
        }
    }
}
