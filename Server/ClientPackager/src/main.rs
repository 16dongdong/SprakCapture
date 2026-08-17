#![allow(non_snake_case, non_upper_case_globals)]

//! 提供仅依赖预编译 APK 模板的客户端打包命令行。
//!
//! 主服务通过一次性子进程调用本程序；标准输出只承载严格 JSON 结果，标准错误输出中文失败原因，
//! 从而让进程边界同时成为依赖隔离和错误归属边界。

use std::{
    collections::HashMap,
    env,
    io::{self, Read},
    path::PathBuf,
    process::ExitCode,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as base64Standard};
use client_packager::{ClientTemplateRequest, packageClientTemplate, prepareClientTemplate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const packageCommand: &str = "package";
const prepareTemplateCommand: &str = "prepare-template";
const maximumPackageInputBytes: u64 = 2 * 1024 * 1024;

#[derive(Debug)]
struct PackageArguments {
    templatePath: PathBuf,
    outputPath: PathBuf,
    signingDirectory: PathBuf,
}

#[derive(Debug)]
struct PrepareTemplateArguments {
    sourcePath: PathBuf,
    outputPath: PathBuf,
}

/// 描述只允许经标准输入传递的运行期定制资料；进程列表只保留可信文件路径，不出现节点或用户输入。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageInput {
    applicationId: String,
    applicationName: String,
    nodeHost: String,
    nodePort: u16,
    username: String,
    password: String,
    rulesUrl: String,
    iconBase64: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackageOutput {
    sizeBytes: u64,
    sha256: String,
}

/// 解析并执行模板准备或运行时打包命令；全部运行期定制字段只从有界标准输入读取，错误不会回显正文。
fn run(
    mut arguments: impl Iterator<Item = String>,
    packageInputBytes: &[u8],
) -> Result<PackageOutput, String> {
    let command = arguments
        .next()
        .ok_or_else(|| "缺少客户端打包器子命令".to_owned())?;
    let result = match command.as_str() {
        packageCommand => {
            let packageArguments = parsePackageArguments(arguments)?;
            let packageInput = serde_json::from_slice::<PackageInput>(packageInputBytes)
                .map_err(|_| "客户端打包器标准输入不是有效装配协议".to_owned())?;
            let iconBytes = packageInput
                .iconBase64
                .map(|value| {
                    base64Standard
                        .decode(value)
                        .map_err(|_| "客户端自定义图标不是有效 Base64".to_owned())
                })
                .transpose()?;
            packageClientTemplate(&ClientTemplateRequest {
                templatePath: packageArguments.templatePath,
                destinationPath: packageArguments.outputPath,
                signingDirectory: packageArguments.signingDirectory,
                applicationId: packageInput.applicationId,
                applicationName: packageInput.applicationName,
                nodeHost: packageInput.nodeHost,
                nodePort: packageInput.nodePort,
                username: packageInput.username,
                password: packageInput.password,
                rulesUrl: packageInput.rulesUrl,
                iconBytes,
            })?
        }
        prepareTemplateCommand => {
            if !packageInputBytes.is_empty() {
                return Err("模板准备命令不接受标准输入".to_owned());
            }
            let prepareArguments = parsePrepareTemplateArguments(arguments)?;
            prepareClientTemplate(&prepareArguments.sourcePath, &prepareArguments.outputPath)?
        }
        _ => return Err(format!("未知客户端打包器子命令：{command}")),
    };
    Ok(PackageOutput {
        sizeBytes: result.bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&result.bytes)),
    })
}

/// 将 `package` 选项收拢为运行时领域参数；进程协议禁止忽略未知字段或重复选项。
fn parsePackageArguments(
    arguments: impl Iterator<Item = String>,
) -> Result<PackageArguments, String> {
    let mut options = collectOptions(
        arguments,
        &["--template", "--output", "--signing-directory"],
    )?;
    Ok(PackageArguments {
        templatePath: PathBuf::from(takeRequired(&mut options, "--template")?),
        outputPath: PathBuf::from(takeRequired(&mut options, "--output")?),
        signingDirectory: PathBuf::from(takeRequired(&mut options, "--signing-directory")?),
    })
}

/// 解析发布阶段模板准备参数；只接受源 APK 与目标模板，避免把运行时签名材料带入编译阶段。
fn parsePrepareTemplateArguments(
    arguments: impl Iterator<Item = String>,
) -> Result<PrepareTemplateArguments, String> {
    let mut options = collectOptions(arguments, &["--source", "--output"])?;
    Ok(PrepareTemplateArguments {
        sourcePath: PathBuf::from(takeRequired(&mut options, "--source")?),
        outputPath: PathBuf::from(takeRequired(&mut options, "--output")?),
    })
}

/// 收集成对 CLI 选项并按白名单校验；缺值、重复值和未知字段均在执行文件操作前失败。
fn collectOptions(
    mut arguments: impl Iterator<Item = String>,
    allowedNames: &[&str],
) -> Result<HashMap<String, String>, String> {
    let mut options = HashMap::new();
    while let Some(name) = arguments.next() {
        if !name.starts_with("--") {
            return Err(format!("客户端打包参数名称无效：{name}"));
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("客户端打包参数缺少取值：{name}"))?;
        if options.insert(name.clone(), value).is_some() {
            return Err(format!("客户端打包参数重复：{name}"));
        }
    }
    for name in options.keys() {
        if !allowedNames.contains(&name.as_str()) {
            return Err(format!("未知客户端打包参数：{name}"));
        }
    }
    Ok(options)
}

/// 取出必需选项并删除映射项；缺失值返回精确参数名，避免主服务只能看到模糊退出码。
fn takeRequired(options: &mut HashMap<String, String>, name: &str) -> Result<String, String> {
    options
        .remove(name)
        .ok_or_else(|| format!("缺少客户端打包参数：{name}"))
}

/// 执行命令并将成功结果写入标准输出；只有 package 读取秘密管道，prepare-template 继承终端时不得等待 EOF。
/// 失败只写标准错误且返回非零退出码，避免污染父进程消费的 JSON 协议。
fn main() -> ExitCode {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let packageInputBytes = if arguments
        .first()
        .is_some_and(|value| value == packageCommand)
    {
        let mut input = Zeroizing::new(Vec::new());
        if let Err(error) = io::stdin()
            .take(maximumPackageInputBytes + 1)
            .read_to_end(&mut input)
        {
            eprintln!("读取客户端打包器标准输入失败：{error}");
            return ExitCode::FAILURE;
        }
        if input.len() as u64 > maximumPackageInputBytes {
            eprintln!("客户端打包器标准输入超过协议上限");
            return ExitCode::FAILURE;
        }
        input
    } else {
        Zeroizing::new(Vec::new())
    };
    match run(arguments.into_iter(), &packageInputBytes) {
        Ok(output) => match serde_json::to_string(&output) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("序列化客户端打包结果失败：{error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
