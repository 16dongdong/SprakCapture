//! 提供插件开发者从空目录创建、校验、生成 Schema 和重放阶段夹具所需的确定性工具。
//!
//! 命令行、CI 和图形化开发工具必须复用本模块，避免“本地能安装、CI 校验失败”或不同入口
//! 对 manifest、动作和文件边界作出不同解释。本模块只处理开发资产，不启动第三方运行时。

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use thiserror::Error;

use crate::{
    ActionKind, EventEnvelope, ExtensionAction, ExtensionManifest, ExtensionRuntimeKind,
    PluginExecutionOptions, PluginPlatformConfiguration, availableActions,
};

const PLUGIN_MANIFEST_FILE_NAME: &str = "plugin.json";
const MAXIMUM_DEVELOPER_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// 描述开发工具支持创建的运行时模板；作者可自由选择进程内或进程外实现。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScaffoldRuntime {
    Wasm,
    Sidecar,
    NativeWorker,
    Native,
}

impl ScaffoldRuntime {
    /// 把命令行名称解析为模板运行时；未知名称返回稳定错误，不生成半成品目录。
    pub fn parse(value: &str) -> Result<Self, DeveloperToolError> {
        match value {
            "wasm" => Ok(Self::Wasm),
            "sidecar" => Ok(Self::Sidecar),
            "nativeWorker" | "native-worker" => Ok(Self::NativeWorker),
            "native" => Ok(Self::Native),
            _ => Err(DeveloperToolError::UnsupportedRuntime),
        }
    }

    /// 返回 manifest 使用的运行时枚举和相对入口，模板与校验器共享同一稳定路径。
    fn manifestEntry(self) -> (ExtensionRuntimeKind, &'static str) {
        match self {
            Self::Wasm => (ExtensionRuntimeKind::Wasm, "dist/plugin.wasm"),
            Self::Sidecar => (ExtensionRuntimeKind::Sidecar, "dist/worker.js"),
            Self::NativeWorker => (ExtensionRuntimeKind::NativeWorker, "dist/worker.exe"),
            Self::Native => (ExtensionRuntimeKind::Native, "dist/plugin.dll"),
        }
    }
}

/// 描述脚手架输入；ID 同时是包身份和默认目录名，显示名称只进入用户界面。
pub struct ScaffoldOptions<'a> {
    pub destination: &'a Path,
    pub pluginId: &'a str,
    pub displayName: &'a str,
    pub runtime: ScaffoldRuntime,
}

/// 描述开发工具的精确失败原因；所有写入在预检完成后执行，失败目录可直接删除重试。
#[derive(Debug, Error)]
pub enum DeveloperToolError {
    #[error("developerInvalidManifest")]
    InvalidManifest,
    #[error("developerInvalidFixture")]
    InvalidFixture,
    #[error("developerInvalidPath")]
    InvalidPath,
    #[error("developerMissingEntry")]
    MissingEntry,
    #[error("developerDestinationExists")]
    DestinationExists,
    #[error("developerUnsupportedRuntime")]
    UnsupportedRuntime,
    #[error("developerIo")]
    Io(#[source] std::io::Error),
    #[error("developerJson")]
    Json(#[source] serde_json::Error),
}

/// 保存可被模拟宿主重放的一次阶段输入与期望动作；该格式也是 SDK 的最小跨语言夹具。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageFixture {
    pub manifest: ExtensionManifest,
    pub options: PluginExecutionOptions,
    pub event: EventEnvelope,
    pub action: ExtensionAction,
}

/// 创建一个可由 CLI、CI 和模拟宿主共同使用的完整插件目录骨架。
///
/// 运行上下文：目标目录必须不存在；函数先校验生成的 manifest，再一次性创建全部父目录和文件。
/// 失败语义：任何既有目标、非法 ID、路径或磁盘错误都会中止，不覆盖开发者文件。
pub fn createPluginScaffold(options: ScaffoldOptions<'_>) -> Result<(), DeveloperToolError> {
    if options.destination.exists() {
        return Err(DeveloperToolError::DestinationExists);
    }
    let (runtimeKind, runtimeEntry) = options.runtime.manifestEntry();
    let manifest = scaffoldManifest(
        options.pluginId,
        options.displayName,
        runtimeKind,
        runtimeEntry,
    );
    let manifestBytes = serde_json::to_vec_pretty(&manifest).map_err(DeveloperToolError::Json)?;
    ExtensionManifest::parse(&manifestBytes).map_err(|_| DeveloperToolError::InvalidManifest)?;

    fs::create_dir_all(options.destination.join("schemas")).map_err(DeveloperToolError::Io)?;
    fs::create_dir_all(options.destination.join("fixtures")).map_err(DeveloperToolError::Io)?;
    fs::create_dir_all(options.destination.join("dist")).map_err(DeveloperToolError::Io)?;
    fs::write(
        options.destination.join(PLUGIN_MANIFEST_FILE_NAME),
        manifestBytes,
    )
    .map_err(DeveloperToolError::Io)?;
    fs::write(
        options
            .destination
            .join("schemas/configuration.schema.json"),
        configurationSchema(),
    )
    .map_err(DeveloperToolError::Io)?;
    fs::write(
        options.destination.join("fixtures/tcpChunk.json"),
        fixtureTemplate(options.pluginId),
    )
    .map_err(DeveloperToolError::Io)?;
    fs::write(
        options.destination.join("README.md"),
        readmeTemplate(options.pluginId, options.runtime),
    )
    .map_err(DeveloperToolError::Io)?;
    fs::write(
        options.destination.join(runtimeEntry),
        runtimePlaceholder(options.runtime),
    )
    .map_err(DeveloperToolError::Io)?;
    Ok(())
}

/// 校验一个已展开插件目录的 manifest、声明入口和可选 Schema 路径。
///
/// 运行上下文：安装器、CLI 与开发服务器在执行第三方代码前调用本函数。所有声明路径都必须是
/// 受控相对路径且落在插件目录内。失败语义：缺文件、超预算或语法错误均拒绝整个目录。
pub fn checkPluginDirectory(directory: &Path) -> Result<ExtensionManifest, DeveloperToolError> {
    let manifestPath = directory.join(PLUGIN_MANIFEST_FILE_NAME);
    let manifestBytes = readBoundedFile(&manifestPath)?;
    let manifest = ExtensionManifest::parse(&manifestBytes)
        .map_err(|_| DeveloperToolError::InvalidManifest)?;
    checkDeclaredFile(directory, &manifest.runtime.entry)?;
    if let Some(configurationSchema) = manifest.configurationSchema.as_deref() {
        let bytes = readDeclaredFile(directory, configurationSchema)?;
        let schema: JsonValue = serde_json::from_slice(&bytes).map_err(DeveloperToolError::Json)?;
        if !schema.is_object() {
            return Err(DeveloperToolError::InvalidManifest);
        }
    }
    if let Some(contributions) = manifest.contributes.as_deref() {
        let bytes = readDeclaredFile(directory, contributions)?;
        let value: JsonValue = serde_json::from_slice(&bytes).map_err(DeveloperToolError::Json)?;
        if !value.is_object() {
            return Err(DeveloperToolError::InvalidManifest);
        }
    }
    Ok(manifest)
}

/// 把全部公共交换类型生成到指定目录；不同语言 SDK 应从这些 Schema 生成类型而非手写枚举。
pub fn writeDeveloperSchemas(destination: &Path) -> Result<Vec<PathBuf>, DeveloperToolError> {
    fs::create_dir_all(destination).map_err(DeveloperToolError::Io)?;
    let schemas = [
        (
            "extensionManifest.schema.json",
            serde_json::to_value(schema_for!(ExtensionManifest))
                .map_err(DeveloperToolError::Json)?,
        ),
        (
            "eventEnvelope.schema.json",
            serde_json::to_value(schema_for!(EventEnvelope)).map_err(DeveloperToolError::Json)?,
        ),
        (
            "extensionAction.schema.json",
            serde_json::to_value(schema_for!(ExtensionAction)).map_err(DeveloperToolError::Json)?,
        ),
        (
            "stageFixture.schema.json",
            serde_json::to_value(schema_for!(StageFixture)).map_err(DeveloperToolError::Json)?,
        ),
        (
            "pluginPlatformConfiguration.schema.json",
            serde_json::to_value(schema_for!(PluginPlatformConfiguration))
                .map_err(DeveloperToolError::Json)?,
        ),
    ];
    schemas
        .into_iter()
        .map(|(fileName, schema)| {
            let path = destination.join(fileName);
            let bytes = serde_json::to_vec_pretty(&schema).map_err(DeveloperToolError::Json)?;
            fs::write(&path, bytes).map_err(DeveloperToolError::Io)?;
            Ok(path)
        })
        .collect()
}

/// 复验一个跨语言阶段夹具的身份、订阅和动作边界。
///
/// 该校验不执行插件代码，适合开发者在 CI 中验证运行时输出。线上宿主仍会在实际调用后重复相同
/// 阶段契约检查，不能把通过夹具校验视为跳过线上生命周期与协议一致性校验的凭据。
pub fn validateStageFixture(fixture: &StageFixture) -> Result<(), DeveloperToolError> {
    if fixture.event.eventId != fixture.action.eventId
        || !availableActions(fixture.event.stage).contains(&fixture.action.action)
        || !fixture.manifest.modules.iter().any(|module| {
            module
                .subscriptions
                .iter()
                .any(|subscription| subscription.stage == fixture.event.stage)
        })
    {
        return Err(DeveloperToolError::InvalidFixture);
    }
    if fixture.event.context.interceptionMode == crate::InterceptionMode::ObserveOnly
        && !matches!(
            fixture.action.action,
            ActionKind::Continue | ActionKind::Annotate
        )
    {
        return Err(DeveloperToolError::InvalidFixture);
    }
    Ok(())
}

/// 读取并校验一个夹具文件；大小边界先于 JSON 解析，避免 CI 误读大正文捕获文件。
pub fn readStageFixture(path: &Path) -> Result<StageFixture, DeveloperToolError> {
    let bytes = readBoundedFile(path)?;
    let fixture = serde_json::from_slice(&bytes).map_err(DeveloperToolError::Json)?;
    validateStageFixture(&fixture)?;
    Ok(fixture)
}

/// 构造默认 TCP 流转换 manifest；模板订阅双向块并显式请求观察和修改能力。
fn scaffoldManifest(
    pluginId: &str,
    displayName: &str,
    runtimeKind: ExtensionRuntimeKind,
    runtimeEntry: &str,
) -> JsonValue {
    json!({
        "manifestVersion": 2,
        "id": pluginId,
        "name": displayName,
        "description": "有状态二进制流分帧、解码、修改与重封包模块",
        "version": "1.0.0",
        "publisher": "local.developer",
        "engines": { "host": ">=2.0.0 <3.0.0", "api": "2.x" },
        "runtime": { "kind": runtimeKind, "entry": runtimeEntry, "protocolVersion": "2.0" },
        "modules": [{
            "id": "streamTransformer",
            "kind": "streamTransformer",
            "subscriptions": [{
                "stage": "tcpChunk",
                "order": 200,
                "match": { "transports": ["tcp"] }
            }]
        }],
        "capabilities": ["traffic.observe", "traffic.modify", "capture.annotate"],
        "dependencies": {},
        "limits": {
            "timeoutMs": 50,
            "maxPendingEvents": 128,
            "maxOutputBytes": 1048576,
            "maxStorageBytes": 67108864
        },
        "configurationSchema": "schemas/configuration.schema.json"
    })
}

/// 返回只允许明确字段的最小配置 Schema；开发者可在该基础上扩展协议密钥和分帧参数。
fn configurationSchema() -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {},
        "additionalProperties": false
    }))
    .expect("静态配置 Schema 必须可序列化")
}

/// 返回可直接被模拟宿主校验的 TCP 阶段夹具；时间和连接标识均为固定测试值。
fn fixtureTemplate(pluginId: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({
        "manifest": scaffoldManifest(pluginId, "协议扩展", ExtensionRuntimeKind::Wasm, "dist/plugin.wasm"),
        "options": {
            "moduleOrder": ["streamTransformer"],
            "subscriptionOverrides": {},
            "failurePolicy": "failClosed",
            "limits": null
        },
        "event": {
            "apiVersion": "2.0",
            "eventId": "fixture_tcp_1",
            "stage": "tcpChunk",
            "serviceGeneration": 1,
            "recordingGeneration": 1,
            "pluginInstanceId": "fixture@1.0.0#1",
            "connectionId": "fixture_connection",
            "transactionId": null,
            "deadlineUnixMs": 4102444800000_u64,
            "context": {
                "entry": "proxy",
                "processName": "fixture.exe",
                "processPath": null,
                "transport": "tcp",
                "protocol": "binary",
                "direction": "clientToServer",
                "scheme": "tcp",
                "host": "127.0.0.1",
                "address": "127.0.0.1",
                "port": 9000,
                "method": null,
                "path": null,
                "statusCode": null,
                "mimeType": null,
                "labels": [],
                "interceptionMode": "intercept"
            },
            "payload": { "bytes": [0, 3, 1, 2, 3] }
        },
        "action": {
            "eventId": "fixture_tcp_1",
            "action": "continue",
            "patch": [],
            "annotations": [],
            "output": null
        }
    }))
    .expect("静态阶段夹具必须可序列化")
}

/// 返回运行时入口占位内容；入口存在使目录校验可立即运行，但明确阻止把模板误当生产插件。
fn runtimePlaceholder(runtime: ScaffoldRuntime) -> &'static [u8] {
    match runtime {
        ScaffoldRuntime::Wasm => b"SPRak plugin template: build a WebAssembly component here.\n",
        ScaffoldRuntime::Sidecar => {
            b"throw new Error('Implement the framed sidecar protocol before packaging');\n"
        }
        ScaffoldRuntime::NativeWorker => {
            b"Build the isolated native worker executable into this path.\n"
        }
        ScaffoldRuntime::Native => b"Build the trusted in-process native module into this path.\n",
    }
}

/// 返回脚手架说明，明确双向状态、半包和重封包的实现边界。
fn readmeTemplate(pluginId: &str, runtime: ScaffoldRuntime) -> String {
    format!(
        "# {pluginId}\n\n运行时：{runtime:?}\n\n实现 `tcpChunk` 时必须为两个方向维护独立缓冲；半包返回 hold，完整帧可输出零到多帧，修改后由插件重算长度、校验和或认证标签。先运行 `capture-plugin check .` 与 `capture-plugin fixture fixtures/tcpChunk.json`。\n"
    )
}

/// 读取受预算的小型开发文件；正文捕获与大媒体不能误进入清单和夹具路径。
fn readBoundedFile(path: &Path) -> Result<Vec<u8>, DeveloperToolError> {
    let metadata = fs::metadata(path).map_err(DeveloperToolError::Io)?;
    if !metadata.is_file() || metadata.len() > MAXIMUM_DEVELOPER_FILE_BYTES {
        return Err(DeveloperToolError::InvalidPath);
    }
    fs::read(path).map_err(DeveloperToolError::Io)
}

/// 校验并读取 manifest 声明文件；路径导航和绝对路径在访问磁盘前拒绝。
fn readDeclaredFile(directory: &Path, relativePath: &str) -> Result<Vec<u8>, DeveloperToolError> {
    let path = checkedDeclaredPath(directory, relativePath)?;
    readBoundedFile(&path)
}

/// 校验声明入口存在且为普通文件；平台架构与 ABI 由实际运行时适配器继续验证。
fn checkDeclaredFile(directory: &Path, relativePath: &str) -> Result<(), DeveloperToolError> {
    let path = checkedDeclaredPath(directory, relativePath)?;
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            DeveloperToolError::MissingEntry
        } else {
            DeveloperToolError::Io(error)
        }
    })?;
    metadata
        .is_file()
        .then_some(())
        .ok_or(DeveloperToolError::MissingEntry)
}

/// 将声明相对路径绑定到插件根目录；拒绝空路径、绝对路径、根前缀和任何导航分量。
fn checkedDeclaredPath(
    directory: &Path,
    relativePath: &str,
) -> Result<PathBuf, DeveloperToolError> {
    let path = Path::new(relativePath);
    let valid = !relativePath.is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    valid
        .then(|| directory.join(path))
        .ok_or(DeveloperToolError::InvalidPath)
}
