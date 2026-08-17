#![allow(non_snake_case, non_upper_case_globals)]

//! 从预编译 APK 模板生成独立客户端。
//!
//! 发布阶段已经完成 Kotlin、Compose、资源和 Native 核心编译。运行时重写二进制应用身份与图标，
//! 把连接资料封装为每包随机 XChaCha20-Poly1305 密文，并把密钥只写入两个 ABI 的 Native 固定槽，
//! 再使用本机持久签名身份生成 Android 安装签名；目标机器不需要 Gradle、JDK、Android SDK。

use std::{
    fs,
    io::{Cursor, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    thread,
    time::Duration as StandardDuration,
};

use apksig::{Algorithms, Apk, ValueSigningBlock};
use base64::{Engine as _, engine::general_purpose::STANDARD as base64Standard};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce, aead::AeadInPlace};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_RSA_SHA256};
use rsa::{RsaPrivateKey, pkcs8::DecodePrivateKey};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

mod packageCustomization;

use packageCustomization::{
    PackageCustomization, maximumIconInputBytes, rewriteCustomizedArchive,
    validateCustomizableTemplate, validatePackagedProfile,
};

pub use packageCustomization::{templateApplicationId, templateApplicationName};

const transientCleanupAttempts: usize = 5;
const transientCleanupDelay: StandardDuration = StandardDuration::from_millis(20);
/// 预编译模板只保留空的密文资产；正式下载包必须由独立打包器生成认证密文并补齐 Native 随机密钥槽。
pub const templateProfilePayload: &[u8] = b"";
const maximumNodeHostBytes: usize = 253;
const maximumCredentialBytes: usize = 255;
const minimumDistinctCredentialBytes: usize = 8;
const rulesUrlSlotBytes: usize = 2_048;
const rulesAuthorityHost: &str = "client-rules.internal.invalid";
const rulesPath: &str = "/api/v1/client/routing.txt";
const profileKeyBytes: usize = 32;
const profileNonceBytes: usize = 24;
const profileHeaderBytes: usize = 40;
const profileTagBytes: usize = 16;
const profilePlaintextVersion: u8 = 1;
const profileContainerMagic: &[u8; 8] = b"SPRKPF01";
const certificateValidityDays: i64 = 36_500;
const signingIdentityFileName: &str = "signingIdentity.json";
const manifestPath: &str = "AndroidManifest.xml";
const requiredNativeLibraries: [&str; 2] = [
    "lib/arm64-v8a/libroutesocks.so",
    "lib/armeabi-v7a/libroutesocks.so",
];
const nativeLibraryAlignment: u16 = 16_384;
const storedEntryAlignment: u16 = 4;

/// 描述一次模板装配所需的全部边界；路径和规则地址由控制层生成，凭据只在本次子进程内存中存活。
pub struct ClientTemplateRequest {
    pub templatePath: PathBuf,
    pub destinationPath: PathBuf,
    pub signingDirectory: PathBuf,
    pub applicationId: String,
    pub applicationName: String,
    pub nodeHost: String,
    pub nodePort: u16,
    pub username: String,
    pub password: String,
    pub rulesUrl: String,
    pub iconBytes: Option<Vec<u8>>,
}

/// 在打包请求离开作用域时覆盖连接资料；路径和应用身份不属于运行秘密。
///
/// 运行上下文：成功、校验失败与签名失败路径均由 Rust 自动调用；不执行 I/O，
/// 因此不会覆盖原始错误，只负责缩短节点、凭据与规则地址的堆内存寿命。
impl Drop for ClientTemplateRequest {
    fn drop(&mut self) {
        self.nodeHost.zeroize();
        self.username.zeroize();
        self.password.zeroize();
        self.rulesUrl.zeroize();
    }
}

/// 返回完成签名后的真实字节数；调用方据此生成下载元数据和摘要。
pub struct ClientTemplateResult {
    pub bytes: Vec<u8>,
}

/// 把 Android Gradle 产物规范化为运行时可原位修改的模板；清单改为未压缩并保持 APK 条目对齐。
///
/// 运行上下文：仅桌面发布机器调用一次；`sourcePath` 是未签名 release APK，`destinationPath` 是安装
/// 资源模板。源 ZIP、压缩算法或固定槽位不符合契约时返回中文错误，目标文件只在完整验证后原子提交。
pub fn prepareClientTemplate(
    sourcePath: &Path,
    destinationPath: &Path,
) -> Result<ClientTemplateResult, String> {
    let parentDirectory = destinationPath
        .parent()
        .ok_or_else(|| "客户端模板目标路径缺少父目录".to_owned())?;
    fs::create_dir_all(parentDirectory)
        .map_err(|error| format!("创建客户端模板目录失败：{error}"))?;
    let stagingPath =
        parentDirectory.join(format!(".client-template-{}.apk", Uuid::new_v4().simple()));
    let result = (|| {
        rewriteTemplateArchive(sourcePath, &stagingPath)?;
        let bytes =
            fs::read(&stagingPath).map_err(|error| format!("读取规范化客户端模板失败：{error}"))?;
        validateCustomizableTemplate(&bytes)?;
        validateNativeLibraries(&bytes)?;
        replaceFile(&stagingPath, destinationPath)
            .map_err(|error| format!("发布预编译客户端模板失败：{error}"))?;
        Ok(ClientTemplateResult { bytes })
    })();
    completeWithCleanup(result, &[&stagingPath], "清理客户端模板暂存文件失败")
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SigningIdentity {
    privateKeyPem: String,
    certificateDerBase64: String,
}

/// 从预编译模板装配并签名一个客户端；该函数运行于阻塞线程，不得直接在 Tokio 执行器工作线程调用。
/// `request` 中的目标文件只在签名和完整验签成功后原子替换；模板槽位、节点或签名身份无效时返回中文错误。
pub fn packageClientTemplate(
    request: &ClientTemplateRequest,
) -> Result<ClientTemplateResult, String> {
    validateRequest(request)?;
    let identity = loadOrCreateSigningIdentity(&request.signingDirectory)?;
    let mut templateBytes = fs::read(&request.templatePath)
        .map_err(|error| format!("读取预编译客户端模板失败：{error}"))?;
    if !templateBytes.starts_with(b"PK\x03\x04") {
        return Err("预编译客户端模板不是有效 APK ZIP 文件".to_owned());
    }
    // 正式打包不能只信任历史 prepare 结果；模板可能被替换或污染。这里重新证明 profile 为空且两个 ABI 密钥槽均为零，
    // 避免旧密钥被静默覆盖后继续发布，也保证每个下载包都从无凭据模板独立生成。
    validateCustomizableTemplate(&templateBytes)?;
    validateNativeLibraries(&templateBytes)?;

    let encryptedProfile = encryptProfile(request)?;
    templateBytes = rewriteCustomizedArchive(
        &templateBytes,
        &PackageCustomization {
            applicationId: &request.applicationId,
            applicationName: &request.applicationName,
            iconBytes: request.iconBytes.as_deref(),
            encryptedProfile: Some(&encryptedProfile.container),
            profileKey: Some(&encryptedProfile.key),
        },
    )?;

    let parentDirectory = request
        .destinationPath
        .parent()
        .ok_or_else(|| "客户端目标路径缺少父目录".to_owned())?;
    fs::create_dir_all(parentDirectory)
        .map_err(|error| format!("创建客户端产物目录失败：{error}"))?;
    let (rawPath, signedPath) = transientPackagePaths(&request.destinationPath)?;
    let result = (|| {
        writeSynchronizedFile(&rawPath, &templateBytes, "写入客户端模板副本失败")?;
        signRawApk(&rawPath, &signedPath, &identity)?;
        verifySignedApk(&signedPath)?;
        let bytes =
            fs::read(&signedPath).map_err(|error| format!("读取已签名客户端失败：{error}"))?;
        validateNativeLibraries(&bytes)?;
        validatePackagedProfile(&bytes)?;
        verifyStaticConfidentiality(&bytes, request)?;
        // 原始 APK 已包含完整凭据，必须先确认它从磁盘消失，才允许发布可下载的签名产物。
        // 这样即使杀毒软件或其他进程阻止删除，也会让本次任务失败而不是留下未跟踪的秘密副本。
        removeTransientFiles(&[&rawPath])
            .map_err(|error| format!("清理含凭据客户端模板副本失败：{error}"))?;
        replaceFile(&signedPath, &request.destinationPath)
            .map_err(|error| format!("发布客户端 APK 失败：{error}"))?;
        Ok(ClientTemplateResult { bytes })
    })();
    completeWithCleanup(
        result,
        &[&rawPath, &signedPath],
        "清理客户端装配暂存文件失败",
    )
}

/// 扫描完整 APK 与每个解压条目，证明节点、规则地址和凭据没有以可搜索明文或 Base64 静态残留。
///
/// 运行上下文：签名验签完成后、产物原子发布前调用。扫描只使用请求现有内存并在返回前覆盖临时编码；
/// 命中任一 UTF-8、UTF-16LE 或标准 Base64 表示即返回固定错误，不回显命中值或条目名称。
fn verifyStaticConfidentiality(
    apkBytes: &[u8],
    request: &ClientTemplateRequest,
) -> Result<(), String> {
    let endpoint = format!("{}:{}", request.nodeHost, request.nodePort);
    let nodePort = request.nodePort.to_string();
    let base64Values = [
        request.nodeHost.as_str(),
        nodePort.as_str(),
        endpoint.as_str(),
        request.username.as_str(),
        request.password.as_str(),
        request.rulesUrl.as_str(),
    ]
    .map(|value| Zeroizing::new(base64Standard.encode(value.as_bytes())));
    let mut values = vec![
        request.nodeHost.as_str(),
        endpoint.as_str(),
        request.rulesUrl.as_str(),
    ];
    values.extend(base64Values.iter().map(|value| value.as_str()));
    // 很短的数字账号会自然出现在资源表、版本号或机器码里，直接按裸子串扫描会让合法用户永久无法下载。
    // 长凭据继续逐字段扫描；短凭据由标准 Base64 和完整带长度前缀的资料明文共同证明未静态落盘。
    for credential in [&request.username, &request.password] {
        if credential.len() >= minimumDistinctCredentialBytes {
            values.push(credential.as_str());
        }
    }
    let profilePlaintext = encodeProfilePlaintext(request)?;
    if containsStaticValue(apkBytes, &values)
        || containsBytes(apkBytes, &profilePlaintext)
        || containsPortToken(apkBytes, &nodePort)
    {
        return Err("客户端成品包含可搜索的连接资料".to_owned());
    }
    let mut archive = ZipArchive::new(Cursor::new(apkBytes))
        .map_err(|error| format!("读取客户端成品静态扫描条目失败：{error}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("读取客户端成品静态扫描条目失败：{error}"))?;
        let mut entryBytes = Vec::with_capacity(entry.size() as usize);
        std::io::copy(&mut entry, &mut entryBytes)
            .map_err(|error| format!("解压客户端成品静态扫描条目失败：{error}"))?;
        if containsStaticValue(&entryBytes, &values)
            || containsBytes(&entryBytes, &profilePlaintext)
            || containsPortToken(&entryBytes, &nodePort)
        {
            return Err("客户端成品解压条目包含可搜索的连接资料".to_owned());
        }
    }
    Ok(())
}

/// 判断完整二进制资料是否原样出现在产物中；长度前缀和字段顺序使短凭据也具有稳定辨识度。
fn containsBytes(contents: &[u8], value: &[u8]) -> bool {
    !value.is_empty() && contents.windows(value.len()).any(|window| window == value)
}

/// 检查一段字节是否包含任一敏感字符串的 UTF-8 或 UTF-16LE 表示；空值在上层已拒绝。
fn containsStaticValue(contents: &[u8], values: &[&str]) -> bool {
    values.iter().any(|value| {
        let utf8 = value.as_bytes();
        let utf16 = value
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        contents.windows(utf8.len()).any(|window| window == utf8)
            || contents.windows(utf16.len()).any(|window| window == utf16)
    })
}

/// 仅在端口作为连接字段出现时进行扫描，排除编译器符号表中的同名数字。
///
/// 运行上下文：profile 已使用 AEAD 加密，端口泄漏只能表现为 URI、JSON 或
/// endpoint 字段；裸数字会与 Android/NDK 的调试符号碰撞，因此只接受连接分隔符
/// 前缀，避免把合法模板误判为泄漏，同时仍覆盖可搜索的端口配置文本。
fn containsPortToken(contents: &[u8], port: &str) -> bool {
    let markers = [
        format!(":{port}"),
        format!("\"{port}\""),
        format!("'{port}'"),
        format!("={port}"),
    ];
    markers.iter().any(|marker| {
        contents
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
    })
}

/// 为单次任务生成只属于目标 APK 的 raw/signed 暂存路径，后端因此能够精确清理而不会遍历其他任务。
fn transientPackagePaths(destinationPath: &Path) -> Result<(PathBuf, PathBuf), String> {
    let parentDirectory = destinationPath
        .parent()
        .ok_or_else(|| "客户端产物路径缺少父目录".to_owned())?;
    let destinationName = destinationPath
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "客户端产物文件名不是 UTF-8".to_owned())?;
    Ok((
        parentDirectory.join(format!(".{destinationName}.raw.apk")),
        parentDirectory.join(format!(".{destinationName}.signed.apk")),
    ))
}

/// 合并主操作与暂存文件清理结果；主操作失败时保留原始诊断并追加清理错误，成功时任何残留都改判失败。
fn completeWithCleanup<T>(
    operationResult: Result<T, String>,
    paths: &[&Path],
    cleanupContext: &str,
) -> Result<T, String> {
    let cleanupResult = removeTransientFiles(paths);
    match (operationResult, cleanupResult) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanupError)) => Err(format!("{cleanupContext}：{cleanupError}")),
        (Err(operationError), Ok(())) => Err(operationError),
        (Err(operationError), Err(cleanupError)) => Err(format!(
            "{operationError}；{cleanupContext}：{cleanupError}"
        )),
    }
}

/// 有界重试删除所有暂存文件；每轮遍历全部路径，避免首个失败掩盖其他含凭据文件的清理结果。
fn removeTransientFiles(paths: &[&Path]) -> Result<(), String> {
    let mut remaining = paths
        .iter()
        .filter(|path| path.exists())
        .map(|path| (*path).to_path_buf())
        .collect::<Vec<_>>();
    let mut errors = Vec::new();
    for attempt in 0..transientCleanupAttempts {
        errors.clear();
        remaining.retain(|path| match fs::remove_file(path) {
            Ok(()) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                errors.push(format!("{}：{error}", path.display()));
                true
            }
        });
        if remaining.is_empty() {
            return Ok(());
        }
        if attempt + 1 < transientCleanupAttempts {
            thread::sleep(transientCleanupDelay);
        }
    }
    Err(errors.join("；"))
}

/// 校验模板装配请求的 Android 标识、显示文本、图标和认证边界。
///
/// 运行上下文：读取模板和签名身份前调用；字段非法时返回不含原始凭据或图片内容的中文错误。
fn validateRequest(request: &ClientTemplateRequest) -> Result<(), String> {
    if !isValidApplicationId(&request.applicationId) {
        return Err(
            "Android applicationId 必须是 2 到 8 段、总长不超过 127 字节的小写包名".to_owned(),
        );
    }
    let applicationNameCharacters = request.applicationName.chars().count();
    if request.applicationName.trim() != request.applicationName
        || !(1..=32).contains(&applicationNameCharacters)
        || request.applicationName.chars().any(char::is_control)
    {
        return Err("Android 软件名必须是 1 到 32 个非控制字符且首尾不能留空".to_owned());
    }
    if !request.templatePath.is_file() {
        return Err(format!(
            "预编译客户端模板不存在：{}",
            request.templatePath.display()
        ));
    }
    validateCredential("账号", &request.username)?;
    validateCredential("密码", &request.password)?;
    validateNodeHost(&request.nodeHost)?;
    validateRulesUrl(&request.rulesUrl)?;
    if request
        .iconBytes
        .as_ref()
        .is_some_and(|bytes| bytes.is_empty() || bytes.len() > maximumIconInputBytes)
    {
        return Err(format!(
            "自定义图标必须位于 1..={maximumIconInputBytes} 字节"
        ));
    }
    Ok(())
}

/// 校验 APK 内部节点只使用规范 IP 字面量；独立 CLI 不能生成客户端安装后才因主机格式失败的产物。
///
/// 运行上下文：控制服务与独立打包命令共用本校验。域名、空白及非规范 IPv6 均返回静态中文错误，
/// 不在诊断中回显真实地址；调用方必须传递 `IpAddr::to_string()` 的结果。
fn validateNodeHost(nodeHost: &str) -> Result<(), String> {
    let address = nodeHost
        .parse::<IpAddr>()
        .map_err(|_| "客户端节点主机必须是规范 IP 字面量".to_owned())?;
    if address.to_string() != nodeHost {
        return Err("客户端节点主机必须是规范 IP 字面量".to_owned());
    }
    Ok(())
}

/// 校验 Android applicationId；只接受小写 ASCII 段，避免大小写文件系统与清单解析产生差异。
fn isValidApplicationId(applicationId: &str) -> bool {
    let segments = applicationId.split('.').collect::<Vec<_>>();
    applicationId.len() <= 127
        && (2..=8).contains(&segments.len())
        && segments.iter().all(|segment| {
            segment
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_lowercase())
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

/// 重写 APK ZIP 并保持未压缩条目对齐；清单强制 Stored 后，运行时才能在不改变偏移的前提下修改包名。
fn rewriteTemplateArchive(sourcePath: &Path, destinationPath: &Path) -> Result<(), String> {
    let sourceFile = fs::File::open(sourcePath)
        .map_err(|error| format!("打开 Android 客户端编译产物失败：{error}"))?;
    let mut sourceArchive = ZipArchive::new(sourceFile)
        .map_err(|error| format!("读取 Android 客户端 ZIP 失败：{error}"))?;
    let destinationFile = fs::File::create(destinationPath)
        .map_err(|error| format!("创建预编译客户端模板失败：{error}"))?;
    let mut destinationArchive = ZipWriter::new(destinationFile);
    for index in 0..sourceArchive.len() {
        let mut entry = sourceArchive
            .by_index(index)
            .map_err(|error| format!("读取 Android 客户端 ZIP 条目失败：{error}"))?;
        let name = entry.name().to_owned();
        if entry.is_dir() {
            destinationArchive
                .add_directory(name, SimpleFileOptions::default())
                .map_err(|error| format!("写入客户端目录条目失败：{error}"))?;
            continue;
        }
        let compression = if name == manifestPath {
            CompressionMethod::Stored
        } else {
            match entry.compression() {
                CompressionMethod::Stored | CompressionMethod::Deflated => entry.compression(),
                method => return Err(format!("客户端模板包含不支持的 ZIP 压缩算法：{method:?}")),
            }
        };
        let alignment = if compression == CompressionMethod::Stored && name.ends_with(".so") {
            nativeLibraryAlignment
        } else if compression == CompressionMethod::Stored {
            storedEntryAlignment
        } else {
            1
        };
        let options = SimpleFileOptions::default()
            .compression_method(compression)
            .with_alignment(alignment);
        destinationArchive
            .start_file(name, options)
            .map_err(|error| format!("创建客户端模板条目失败：{error}"))?;
        std::io::copy(&mut entry, &mut destinationArchive)
            .map_err(|error| format!("复制客户端模板条目失败：{error}"))?;
    }
    let mut file = destinationArchive
        .finish()
        .map_err(|error| format!("完成客户端模板 ZIP 失败：{error}"))?;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("同步客户端模板 ZIP 失败：{error}"))
}

/// 校验发布 APK 的 Native 边界；两个受支持 ABI 必须各自只包含统一核心 `libroutesocks.so`。
///
/// 运行上下文：模板准备和每次装配都会调用本函数，防止旧 HEV 动态库或第三方 SO 被模板原样带入。
/// `contents` 是完整 APK 字节；ZIP 损坏、ABI 缺失、重复或出现额外 SO 时返回中文错误并阻止发布。
fn validateNativeLibraries(contents: &[u8]) -> Result<(), String> {
    let mut archive = ZipArchive::new(Cursor::new(contents))
        .map_err(|error| format!("读取客户端模板 Native 条目失败：{error}"))?;
    let mut nativeLibraries = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("读取客户端模板 Native 条目失败：{error}"))?;
        let name = entry.name();
        if name.starts_with("lib/") && name.ends_with(".so") {
            nativeLibraries.push(name.to_owned());
        }
    }
    nativeLibraries.sort_unstable();
    let mut expectedLibraries = requiredNativeLibraries.map(str::to_owned).to_vec();
    expectedLibraries.sort_unstable();
    if nativeLibraries != expectedLibraries {
        return Err(format!(
            "客户端模板必须仅包含双 ABI 统一 Native 核心，实际条目为：{}",
            nativeLibraries.join(", ")
        ));
    }
    Ok(())
}

/// 持有单次 APK 的认证密文与 Native 密钥；密钥由 `Zeroizing` 在成功和失败路径自动覆盖。
struct EncryptedProfile {
    container: Vec<u8>,
    key: Zeroizing<[u8; profileKeyBytes]>,
}

/// 把节点、凭据和规则地址编码为严格二进制明文，再用每包随机 XChaCha20-Poly1305 密钥认证加密。
///
/// 运行上下文：独立打包器只在内存中持有明文，密文写入 `profile.bin`，密钥写入两个 ABI 的 Native 固定槽。
/// 任何随机源、字段边界或认证加密失败都中止打包；错误只包含字段角色，不回显节点和凭据。
fn encryptProfile(request: &ClientTemplateRequest) -> Result<EncryptedProfile, String> {
    let hostBytes = request.nodeHost.as_bytes();
    if hostBytes.is_empty()
        || hostBytes.len() > maximumNodeHostBytes
        || request.nodeHost.chars().any(char::is_control)
    {
        return Err(format!(
            "客户端节点主机必须是 1 到 {maximumNodeHostBytes} 字节且不含控制字符"
        ));
    }
    if request.nodePort == 0 {
        return Err("客户端节点端口不能为 0".to_owned());
    }
    let plaintext = encodeProfilePlaintext(request)?;

    let mut key = Zeroizing::new([0_u8; profileKeyBytes]);
    let mut nonce = [0_u8; profileNonceBytes];
    getrandom::fill(&mut *key).map_err(|_| "生成客户端静态资料密钥失败".to_owned())?;
    getrandom::fill(&mut nonce).map_err(|_| "生成客户端静态资料随机数失败".to_owned())?;
    let mut header = Vec::with_capacity(profileHeaderBytes);
    header.extend_from_slice(profileContainerMagic);
    header.extend_from_slice(&[1, 1, 0, 0]);
    header.extend_from_slice(&nonce);
    header.extend_from_slice(
        &u32::try_from(plaintext.len())
            .map_err(|_| "客户端静态资料明文过长".to_owned())?
            .to_be_bytes(),
    );
    let mut cipherText = plaintext.to_vec();
    let cipher = XChaCha20Poly1305::new_from_slice(&*key)
        .map_err(|_| "初始化客户端静态资料加密器失败".to_owned())?;
    let tag = cipher
        .encrypt_in_place_detached(XNonce::from_slice(&nonce), &header, &mut cipherText)
        .map_err(|_| "加密客户端静态资料失败".to_owned())?;
    nonce.zeroize();
    let mut container = Vec::with_capacity(profileHeaderBytes + cipherText.len() + profileTagBytes);
    container.extend_from_slice(&header);
    container.append(&mut cipherText);
    container.extend_from_slice(tag.as_slice());
    Ok(EncryptedProfile { container, key })
}

/// 按客户端解密器的稳定字段顺序构造资料明文；调用方必须用 `Zeroizing` 缩短凭据在堆内存中的寿命。
fn encodeProfilePlaintext(request: &ClientTemplateRequest) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut plaintext = Zeroizing::new(Vec::with_capacity(
        11 + request.nodeHost.len()
            + request.username.len()
            + request.password.len()
            + request.rulesUrl.len(),
    ));
    plaintext.push(profilePlaintextVersion);
    appendProfileField(&mut plaintext, &request.nodeHost, "节点主机")?;
    plaintext.extend_from_slice(&request.nodePort.to_be_bytes());
    appendProfileField(&mut plaintext, &request.username, "账号")?;
    appendProfileField(&mut plaintext, &request.password, "密码")?;
    appendProfileField(&mut plaintext, &request.rulesUrl, "规则地址")?;
    Ok(plaintext)
}

/// 追加大端 u16 长度前缀的 UTF-8 字段；超界时只返回字段角色，不回显秘密内容。
fn appendProfileField(output: &mut Vec<u8>, value: &str, fieldName: &str) -> Result<(), String> {
    let length =
        u16::try_from(value.len()).map_err(|_| format!("客户端{fieldName}超过静态资料字段上限"))?;
    if length == 0 {
        return Err(format!("客户端{fieldName}不能为空"));
    }
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

/// 校验 RFC 1929 凭据的 UTF-8 字节边界；任意密码账号也必须内置下载时提交的非空密码，确保 SOCKS 与规则认证一致。
fn validateCredential(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > maximumCredentialBytes {
        return Err(format!(
            "客户端{name}长度必须位于 1..={maximumCredentialBytes} 个 UTF-8 字节"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("客户端{name}不能包含控制字符"));
    }
    Ok(())
}

/// 校验云规则地址的固定协议边界；地址禁止携带用户信息，认证只能来自独立凭据字段。
///
/// 运行上下文：独立 CLI 也必须遵循服务端保留域映射合同。协议、保留域、端口、路径或 URI 附加字段
/// 不符合约定时返回静态错误，避免签名后由 Android 解析器才拒绝安装包。
fn validateRulesUrl(rulesUrl: &str) -> Result<(), String> {
    let parsed = url::Url::parse(rulesUrl).map_err(|_| "客户端规则地址格式无效".to_owned())?;
    let valid = rulesUrl.len() <= rulesUrlSlotBytes
        && parsed.scheme() == "http"
        && parsed.host_str() == Some(rulesAuthorityHost)
        && parsed.port().is_some_and(|port| port != 0)
        && parsed.path() == rulesPath
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.as_str() == rulesUrl;
    if !valid {
        return Err("客户端规则地址必须是保留域上的规范绝对 HTTP 地址".to_owned());
    }
    Ok(())
}

/// 读取或创建单文件持久签名身份；私钥和证书同文件提交，进程中断不会留下半初始化状态。
fn loadOrCreateSigningIdentity(signingDirectory: &Path) -> Result<SigningIdentity, String> {
    fs::create_dir_all(signingDirectory)
        .map_err(|error| format!("创建客户端签名目录失败：{error}"))?;
    let identityPath = signingDirectory.join(signingIdentityFileName);
    if identityPath.is_file() {
        let bytes =
            fs::read(&identityPath).map_err(|error| format!("读取客户端签名身份失败：{error}"))?;
        let identity: SigningIdentity = serde_json::from_slice(&bytes)
            .map_err(|error| format!("客户端签名身份格式无效：{error}"))?;
        parsePrivateKey(&identity)?;
        decodeCertificate(&identity)?;
        return Ok(identity);
    }

    let identity = createSigningIdentity()?;
    let bytes = serde_json::to_vec_pretty(&identity)
        .map_err(|error| format!("序列化客户端签名身份失败：{error}"))?;
    let stagingPath =
        signingDirectory.join(format!(".signingIdentity-{}.json", Uuid::new_v4().simple()));
    let result = (|| {
        writeSynchronizedFile(&stagingPath, &bytes, "写入客户端签名身份失败")?;
        replaceFile(&stagingPath, &identityPath)
            .map_err(|error| format!("提交客户端签名身份失败：{error}"))?;
        Ok(identity)
    })();
    completeWithCleanup(result, &[&stagingPath], "清理客户端签名身份暂存文件失败")
}

/// 使用 AWS-LC 生成 RSA-2048 PKCS#8 私钥与百年自签证书；该算法满足 Android 的 SHA-256/RSA 安装契约。
fn createSigningIdentity() -> Result<SigningIdentity, String> {
    let keyPair = KeyPair::generate_for(&PKCS_RSA_SHA256)
        .map_err(|error| format!("生成客户端 RSA 私钥失败：{error}"))?;
    let mut parameters = CertificateParams::new(Vec::<String>::new())
        .map_err(|error| format!("创建客户端签名证书参数失败：{error}"))?;
    let now = OffsetDateTime::now_utc();
    parameters.not_before = now - Duration::days(1);
    parameters.not_after = now + Duration::days(certificateValidityDays);
    let mut distinguishedName = DistinguishedName::new();
    distinguishedName.push(DnType::CommonName, "Generated Android Client");
    distinguishedName.push(DnType::OrganizationName, "Local");
    parameters.distinguished_name = distinguishedName;
    let certificate = parameters
        .self_signed(&keyPair)
        .map_err(|error| format!("签发客户端签名证书失败：{error}"))?;
    Ok(SigningIdentity {
        privateKeyPem: keyPair.serialize_pem(),
        certificateDerBase64: base64Standard.encode(certificate.der()),
    })
}

/// 解析持久 PKCS#8 私钥；损坏身份必须显式失败，禁止静默换钥导致后续 APK 无法覆盖安装。
fn parsePrivateKey(identity: &SigningIdentity) -> Result<RsaPrivateKey, String> {
    RsaPrivateKey::from_pkcs8_pem(&identity.privateKeyPem)
        .map_err(|error| format!("客户端签名私钥无效：{error}"))
}

/// 解码持久 X.509 证书；空证书和非法 Base64 都会阻止签名。
fn decodeCertificate(identity: &SigningIdentity) -> Result<Vec<u8>, String> {
    let certificate = base64Standard
        .decode(&identity.certificateDerBase64)
        .map_err(|error| format!("客户端签名证书编码无效：{error}"))?;
    if certificate.is_empty() {
        return Err("客户端签名证书不能为空".to_owned());
    }
    Ok(certificate)
}

/// 为未签名模板写入 Android 安装签名；输出写入独立文件，失败不触碰最终下载路径。
fn signRawApk(rawPath: &Path, signedPath: &Path, identity: &SigningIdentity) -> Result<(), String> {
    let algorithm = Algorithms::RSASSA_PKCS1_v1_5_256;
    let mut apk = Apk::new_raw(rawPath.to_path_buf())
        .map_err(|error| format!("打开客户端模板 APK 失败：{error}"))?;
    apk.sign_v2(
        &algorithm,
        &decodeCertificate(identity)?,
        parsePrivateKey(identity)?,
    )
    .map_err(|error| format!("生成客户端 APK 安装签名失败：{error}"))?;
    let mut signedFile = fs::File::create(signedPath)
        .map_err(|error| format!("创建已签名客户端文件失败：{error}"))?;
    apk.write_with_signature(&mut signedFile)
        .map_err(|error| format!("写入客户端 APK 安装签名失败：{error}"))?;
    signedFile
        .flush()
        .and_then(|()| signedFile.sync_all())
        .map_err(|error| format!("同步已签名客户端文件失败：{error}"))
}

/// 独立重算 APK 内容摘要并校验签名；仅验证 RSA 签名而不比对内容摘要不足以证明产物可安装。
fn verifySignedApk(signedPath: &Path) -> Result<(), String> {
    let algorithm = Algorithms::RSASSA_PKCS1_v1_5_256;
    let apk = Apk::new(signedPath.to_path_buf())
        .map_err(|error| format!("重新打开已签名客户端失败：{error}"))?;
    apk.verify()
        .map_err(|error| format!("客户端 APK 安装签名验证失败：{error}"))?;
    let calculatedDigest = apk
        .digest(&algorithm)
        .map_err(|error| format!("计算客户端 APK 内容摘要失败：{error}"))?;
    let signingBlock = apk
        .get_signing_block()
        .map_err(|error| format!("读取客户端 APK 签名块失败：{error}"))?;
    let embeddedDigests = signingBlock
        .content
        .iter()
        .filter_map(|block| match block {
            ValueSigningBlock::SignatureSchemeV2Block(scheme) => Some(&scheme.signers),
            _ => None,
        })
        .flat_map(|signers| &signers.signers_data)
        .flat_map(|signer| &signer.signed_data.digests.digests_data)
        .filter(|digest| digest.signature_algorithm_id == algorithm)
        .collect::<Vec<_>>();
    if embeddedDigests.len() != 1 || embeddedDigests[0].digest != calculatedDigest {
        return Err("客户端 APK 签名块中的内容摘要与文件不一致".to_owned());
    }
    Ok(())
}

/// 写入并同步关键文件，确保后续原子替换不会提交仍停留在进程缓冲区的数据。
fn writeSynchronizedFile(path: &Path, bytes: &[u8], operation: &str) -> Result<(), String> {
    let mut file = fs::File::create(path).map_err(|error| format!("{operation}：{error}"))?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("{operation}：{error}"))
}

/// 用同卷原子替换提交关键文件；调用方先完成内容同步，读者因此只能观察旧版或完整新版。
///
/// 运行上下文：模板、签名身份与最终 APK 共用该提交原语。Windows 使用带写穿透的替换重命名，
/// 其他平台使用同卷 `rename`；跨卷、权限或目标占用导致的错误原样返回，禁止先删目标制造空窗。
fn replaceFile(source: &Path, destination: &Path) -> std::io::Result<()> {
    replaceFileAtomically(source, destination)
}

#[cfg(windows)]
/// 在 Windows 上用 `MoveFileExW` 原子覆盖同卷目标；写穿透保证返回前目录项已提交到存储设备。
fn replaceFileAtomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let sourceWide = source
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    let destinationWide = destination
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    // SAFETY：两个 UTF-16 缓冲区都以 NUL 结尾，并在系统调用返回前保持有效。
    let moved = unsafe {
        MoveFileExW(
            sourceWide.as_ptr(),
            destinationWide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
/// 在支持覆盖语义的平台上使用标准同卷重命名；跨卷错误交由上层转成领域错误。
fn replaceFileAtomically(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}
