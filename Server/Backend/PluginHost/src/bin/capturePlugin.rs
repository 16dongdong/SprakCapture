//! 提供插件开发者创建、校验、生成公共 Schema 和重放阶段夹具的统一命令行入口。

#![allow(non_snake_case)]

use std::{env, path::Path, process::ExitCode};

use plugin_host::{
    ScaffoldOptions, ScaffoldRuntime, checkPluginDirectory, createPluginScaffold, readStageFixture,
    writeDeveloperSchemas,
};

/// 解析命令并返回进程退出码；错误只输出稳定代码和简短上下文，避免把插件配置正文写入日志。
fn main() -> ExitCode {
    match execute(env::args().skip(1).collect()) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("capture-plugin: {error}");
            ExitCode::FAILURE
        }
    }
}

/// 执行一个开发者命令；参数数量在访问磁盘前严格校验，失败不会生成部分输出。
fn execute(arguments: Vec<String>) -> Result<String, String> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(usage());
    };
    match command {
        "new" => createProject(&arguments[1..]),
        "check" => checkProject(&arguments[1..]),
        "schema" => generateSchemas(&arguments[1..]),
        "fixture" => validateFixture(&arguments[1..]),
        _ => Err(usage()),
    }
}

/// 创建完整插件骨架；显示名称缺省为插件 ID，运行时必须显式选择以避免生成错误入口。
fn createProject(arguments: &[String]) -> Result<String, String> {
    if arguments.len() != 4 || arguments[2] != "--runtime" {
        return Err(
            "用法：capture-plugin new <目录> <插件ID> --runtime <wasm|sidecar|nativeWorker|native>"
                .to_owned(),
        );
    }
    let runtime = ScaffoldRuntime::parse(&arguments[3]).map_err(|error| error.to_string())?;
    createPluginScaffold(ScaffoldOptions {
        destination: Path::new(&arguments[0]),
        pluginId: &arguments[1],
        displayName: &arguments[1],
        runtime,
    })
    .map_err(|error| error.to_string())?;
    Ok(format!("已创建并校验插件目录：{}", arguments[0]))
}

/// 校验展开目录中的 manifest、入口、配置 Schema 和贡献声明。
fn checkProject(arguments: &[String]) -> Result<String, String> {
    if arguments.len() != 1 {
        return Err("用法：capture-plugin check <插件目录>".to_owned());
    }
    let manifest =
        checkPluginDirectory(Path::new(&arguments[0])).map_err(|error| error.to_string())?;
    Ok(format!(
        "插件校验通过：{} {}（{} 个模块）",
        manifest.id,
        manifest.version,
        manifest.modules.len()
    ))
}

/// 生成 manifest、事件、动作和夹具的权威 JSON Schema 文件集合。
fn generateSchemas(arguments: &[String]) -> Result<String, String> {
    if arguments.len() != 1 {
        return Err("用法：capture-plugin schema <输出目录>".to_owned());
    }
    let paths =
        writeDeveloperSchemas(Path::new(&arguments[0])).map_err(|error| error.to_string())?;
    Ok(format!(
        "已生成 {} 个公共 Schema：{}",
        paths.len(),
        arguments[0]
    ))
}

/// 校验阶段夹具的 manifest、订阅、执行参数和动作一致性，不执行第三方代码。
fn validateFixture(arguments: &[String]) -> Result<String, String> {
    if arguments.len() != 1 {
        return Err("用法：capture-plugin fixture <夹具.json>".to_owned());
    }
    let fixture = readStageFixture(Path::new(&arguments[0])).map_err(|error| error.to_string())?;
    Ok(format!(
        "阶段夹具校验通过：{} / {:?} / {:?}",
        fixture.manifest.id, fixture.event.stage, fixture.action.action
    ))
}

/// 返回紧凑总用法；子命令详情由错误消息直接给出，不引入额外解析依赖。
fn usage() -> String {
    "用法：capture-plugin <new|check|schema|fixture> ...".to_owned()
}
