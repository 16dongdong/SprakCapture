//! 管理上游双向 TLS 客户端身份。
//!
//! 身份按 Location 规则选择，导入阶段把 PKCS#12/PFX、PEM 或 DER 统一成 rustls 可直接使用的
//! X.509 链和私钥。公开状态只保留证书元数据；口令从不落盘，私钥写入用户证书目录并沿用该目录 ACL。

use std::{
    collections::HashMap,
    fs,
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD as base64Standard};
use location_core::{
    LocationMatchOptions, LocationPattern, ResolvedLocation, locationMatches,
    validateLocationPattern,
};
use p12_keystore::{KeyStore, Pkcs12ImportPolicy};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x509_parser::parse_x509_certificate;

use crate::ssl::SslMitmError;

const manifestFileName: &str = "clientCertificates.json";
const identityDirectoryName: &str = "clientCertificates";
const maximumClientCertificates: usize = 64;

/// 标识导入来源；运行期始终使用统一 DER 材料，枚举只用于 UI 展示和审计。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientCertificateFormat {
    Pkcs12,
    Pem,
    Der,
}

/// 接收一次客户端身份导入；PKCS#12 只使用 certificateBytes，PEM/DER 必须同时提供 keyBytes。
pub struct ClientCertificateImport {
    pub name: String,
    pub format: ClientCertificateFormat,
    pub enabled: bool,
    pub locations: Vec<LocationPattern>,
    pub certificateBytes: Vec<u8>,
    pub keyBytes: Option<Vec<u8>>,
    pub password: String,
}

/// 向控制面公开客户端证书的非敏感信息和匹配规则。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientCertificateInfo {
    pub id: String,
    pub name: String,
    pub format: ClientCertificateFormat,
    pub enabled: bool,
    pub locations: Vec<LocationPattern>,
    pub subject: String,
    pub issuer: String,
    pub validFromMilliseconds: u64,
    pub validToMilliseconds: u64,
    pub fingerprintSha256: String,
}

/// 更新客户端证书启用状态、名称和主机范围；证书与私钥材料不可通过更新接口替换。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientCertificateUpdate {
    pub name: String,
    pub enabled: bool,
    pub locations: Vec<LocationPattern>,
}

/// 保存在内存中的完整身份；Clone 必须显式复制私钥包装，避免 Debug 或序列化泄露密钥。
pub(crate) struct ClientCertificateIdentity {
    pub info: ClientCertificateInfo,
    pub chain: Vec<CertificateDer<'static>>,
    pub privateKey: PrivateKeyDer<'static>,
}

impl Clone for ClientCertificateIdentity {
    /// 克隆连接池构建所需证书材料；私钥仅停留在进程内存，不产生可打印表示。
    fn clone(&self) -> Self {
        Self {
            info: self.info.clone(),
            chain: self.chain.clone(),
            privateKey: self.privateKey.clone_key(),
        }
    }
}

/// 维护身份清单和密钥文件；调用方在 SslMitmManager 的锁内执行变更以保证原子可见。
pub(crate) struct ClientCertificateStore {
    root: PathBuf,
    identities: HashMap<String, ClientCertificateIdentity>,
    order: Vec<String>,
}

impl ClientCertificateStore {
    /// 从用户证书目录加载清单及每个规范化身份；任何缺失或损坏材料都明确阻止启动。
    pub(crate) fn load(certificateDirectory: &Path) -> Result<Self, SslMitmError> {
        let root = certificateDirectory.join(identityDirectoryName);
        fs::create_dir_all(&root).map_err(|_| SslMitmError::CertificateStorage)?;
        let manifestPath = certificateDirectory.join(manifestFileName);
        let infos = if manifestPath.exists() {
            serde_json::from_slice::<Vec<ClientCertificateInfo>>(
                &fs::read(&manifestPath).map_err(|_| SslMitmError::CertificateStorage)?,
            )
            .map_err(|_| SslMitmError::InvalidClientCertificate)?
        } else {
            Vec::new()
        };
        let mut identities = HashMap::with_capacity(infos.len());
        let mut order = Vec::with_capacity(infos.len());
        for info in infos {
            validateInfo(&info)?;
            let identity = loadIdentity(&root, info)?;
            if identities.contains_key(&identity.info.id) {
                return Err(SslMitmError::InvalidClientCertificate);
            }
            order.push(identity.info.id.clone());
            identities.insert(identity.info.id.clone(), identity);
        }
        Ok(Self {
            root,
            identities,
            order,
        })
    }

    /// 返回按导入顺序排列的公开快照，禁止把链和私钥带入控制响应。
    pub(crate) fn publicState(&self) -> Vec<ClientCertificateInfo> {
        self.order
            .iter()
            .filter_map(|id| self.identities.get(id))
            .map(|identity| identity.info.clone())
            .collect()
    }

    /// 解析并持久化新身份；先写材料再原子更新清单，失败时删除本次材料且不改内存。
    pub(crate) fn import(
        &mut self,
        certificateDirectory: &Path,
        input: ClientCertificateImport,
    ) -> Result<ClientCertificateInfo, SslMitmError> {
        if self.identities.len() >= maximumClientCertificates {
            return Err(SslMitmError::ClientCertificateLimit);
        }
        validateNameAndLocations(&input.name, &input.locations)?;
        let (chain, privateKey) = parseImport(&input)?;
        let info = buildInfo(&input, &chain[0])?;
        if self.identities.contains_key(&info.id) {
            return Err(SslMitmError::DuplicateClientCertificate);
        }
        persistIdentity(&self.root, &info.id, &chain, &privateKey)?;
        let identity = ClientCertificateIdentity {
            info: info.clone(),
            chain,
            privateKey,
        };
        self.order.push(info.id.clone());
        self.identities.insert(info.id.clone(), identity);
        if let Err(error) = self.persistManifest(certificateDirectory) {
            self.identities.remove(&info.id);
            self.order.retain(|id| id != &info.id);
            removeIdentityFiles(&self.root, &info.id);
            return Err(error);
        }
        Ok(info)
    }

    /// 原子更新非密钥元数据和匹配规则；ID 不存在时返回明确错误。
    pub(crate) fn update(
        &mut self,
        certificateDirectory: &Path,
        id: &str,
        update: ClientCertificateUpdate,
    ) -> Result<ClientCertificateInfo, SslMitmError> {
        validateNameAndLocations(&update.name, &update.locations)?;
        let identity = self
            .identities
            .get_mut(id)
            .ok_or(SslMitmError::ClientCertificateNotFound)?;
        let previous = identity.info.clone();
        identity.info.name = update.name;
        identity.info.enabled = update.enabled;
        identity.info.locations = update.locations;
        let current = identity.info.clone();
        if let Err(error) = self.persistManifest(certificateDirectory) {
            self.identities
                .get_mut(id)
                .expect("已验证身份必须仍存在")
                .info = previous;
            return Err(error);
        }
        Ok(current)
    }

    /// 删除身份材料和清单项；清单先提交，确保崩溃后已删除身份不会再次启用。
    pub(crate) fn remove(
        &mut self,
        certificateDirectory: &Path,
        id: &str,
    ) -> Result<(), SslMitmError> {
        let orderIndex = self
            .order
            .iter()
            .position(|current| current == id)
            .ok_or(SslMitmError::ClientCertificateNotFound)?;
        let removed = self
            .identities
            .remove(id)
            .ok_or(SslMitmError::InvalidClientCertificate)?;
        self.order.remove(orderIndex);
        if let Err(error) = self.persistManifest(certificateDirectory) {
            self.identities.insert(id.to_owned(), removed);
            self.order.insert(orderIndex, id.to_owned());
            return Err(error);
        }
        removeIdentityFiles(&self.root, id);
        Ok(())
    }

    /// 按导入顺序选择首个启用且命中 Location 的身份；热路径不排序、不复制未命中身份。
    pub(crate) fn resolve(&self, location: &ResolvedLocation) -> Option<ClientCertificateIdentity> {
        let options = LocationMatchOptions::default();
        self.order.iter().find_map(|id| {
            let identity = self.identities.get(id)?;
            if !identity.info.enabled
                || !identity
                    .info
                    .locations
                    .iter()
                    .any(|pattern| locationMatches(pattern, location, options).unwrap_or(false))
            {
                return None;
            }
            Some(identity.clone())
        })
    }

    /// 把公开清单写入临时文件并替换权威文件；JSON 不包含任何证书或私钥原文。
    fn persistManifest(&self, certificateDirectory: &Path) -> Result<(), SslMitmError> {
        let path = certificateDirectory.join(manifestFileName);
        let next = path.with_extension("json.next");
        let backup = path.with_extension("json.backup");
        let bytes = serde_json::to_vec_pretty(&self.publicState())
            .map_err(|_| SslMitmError::CertificateStorage)?;
        writeSynchronized(&next, &bytes)?;
        let hadCurrent = path.exists();
        if hadCurrent {
            removeFileIfPresent(&backup)?;
            fs::rename(&path, &backup).map_err(|_| SslMitmError::CertificateStorage)?;
        }
        if fs::rename(&next, &path).is_err() {
            if hadCurrent {
                let _ = fs::rename(&backup, &path);
            }
            let _ = removeFileIfPresent(&next);
            return Err(SslMitmError::CertificateStorage);
        }
        if hadCurrent {
            removeFileIfPresent(&backup)?;
        }
        Ok(())
    }
}

/// 解析三类导入容器并统一为证书链和 rustls 私钥；不接受无私钥的信任库。
fn parseImport(
    input: &ClientCertificateImport,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), SslMitmError> {
    match input.format {
        ClientCertificateFormat::Pkcs12 => parsePkcs12(&input.certificateBytes, &input.password),
        ClientCertificateFormat::Pem => parsePem(
            &input.certificateBytes,
            input
                .keyBytes
                .as_deref()
                .ok_or(SslMitmError::InvalidPrivateKey)?,
        ),
        ClientCertificateFormat::Der => parseDer(
            &input.certificateBytes,
            input
                .keyBytes
                .as_deref()
                .ok_or(SslMitmError::InvalidPrivateKey)?,
        ),
    }
}

/// 解析 PKCS#12/PFX 的第一条私钥链；兼容旧式 PBES1 与现代 AES 加密容器。
fn parsePkcs12(
    bytes: &[u8],
    password: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), SslMitmError> {
    let store = KeyStore::from_pkcs12(bytes, password, Pkcs12ImportPolicy::Strict)
        .map_err(|_| SslMitmError::InvalidClientCertificate)?;
    let (_, identity) = store
        .private_key_chain()
        .ok_or(SslMitmError::InvalidPrivateKey)?;
    let chain = identity
        .certs()
        .iter()
        .map(|certificate| CertificateDer::from(certificate.as_der().to_vec()))
        .collect::<Vec<_>>();
    validateChain(&chain)?;
    let privateKey = PrivateKeyDer::Pkcs8(identity.key().as_der().to_vec().into());
    validateIdentity(&chain, &privateKey)?;
    Ok((chain, privateKey))
}

/// 解析 PEM 证书链以及 PKCS#1、PKCS#8 或 SEC1 私钥；加密 PEM 应先导出为 PKCS#12。
fn parsePem(
    certificateBytes: &[u8],
    keyBytes: &[u8],
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), SslMitmError> {
    let mut certificateReader = BufReader::new(certificateBytes);
    let chain = rustls_pemfile::certs(&mut certificateReader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SslMitmError::InvalidClientCertificate)?;
    validateChain(&chain)?;
    let privateKey = rustls_pemfile::private_key(&mut BufReader::new(keyBytes))
        .map_err(|_| SslMitmError::InvalidPrivateKey)?
        .ok_or(SslMitmError::InvalidPrivateKey)?;
    validateIdentity(&chain, &privateKey)?;
    Ok((chain, privateKey))
}

/// 解析单张 DER 叶证书和 DER 私钥；完整中间链应使用 PEM 或 PKCS#12 容器。
fn parseDer(
    certificateBytes: &[u8],
    keyBytes: &[u8],
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), SslMitmError> {
    let chain = vec![CertificateDer::from(certificateBytes.to_vec())];
    validateChain(&chain)?;
    let privateKey =
        PrivateKeyDer::try_from(keyBytes.to_vec()).map_err(|_| SslMitmError::InvalidPrivateKey)?;
    validateIdentity(&chain, &privateKey)?;
    Ok((chain, privateKey))
}

/// 使用 rustls 密钥提供者验证叶证书与私钥匹配，避免错误身份直到真实握手才失败。
fn validateIdentity(
    chain: &[CertificateDer<'static>],
    privateKey: &PrivateKeyDer<'static>,
) -> Result<(), SslMitmError> {
    rustls::sign::CertifiedKey::from_der(
        chain.to_vec(),
        privateKey.clone_key(),
        &rustls::crypto::aws_lc_rs::default_provider(),
    )
    .map(|_| ())
    .map_err(|_| SslMitmError::InvalidPrivateKey)
}

/// 校验证书链非空且每张证书均可解析，第一张必须是公开元数据对应的叶证书。
fn validateChain(chain: &[CertificateDer<'static>]) -> Result<(), SslMitmError> {
    if chain.is_empty()
        || chain
            .iter()
            .any(|certificate| parse_x509_certificate(certificate.as_ref()).is_err())
    {
        return Err(SslMitmError::InvalidClientCertificate);
    }
    Ok(())
}

/// 从叶证书生成稳定 ID 与公开元数据；ID 使用 SHA-256 前 16 字节防止路径注入。
fn buildInfo(
    input: &ClientCertificateImport,
    leaf: &CertificateDer<'static>,
) -> Result<ClientCertificateInfo, SslMitmError> {
    let (_, parsed) = parse_x509_certificate(leaf.as_ref())
        .map_err(|_| SslMitmError::InvalidClientCertificate)?;
    let digest = Sha256::digest(leaf.as_ref());
    let id = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let fingerprintSha256 = digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    Ok(ClientCertificateInfo {
        id,
        name: input.name.trim().to_owned(),
        format: input.format,
        enabled: input.enabled,
        locations: input.locations.clone(),
        subject: parsed.subject().to_string(),
        issuer: parsed.issuer().to_string(),
        validFromMilliseconds: timestampMilliseconds(parsed.validity().not_before.timestamp())?,
        validToMilliseconds: timestampMilliseconds(parsed.validity().not_after.timestamp())?,
        fingerprintSha256,
    })
}

/// 校验公开记录的 ID、名称和规则，阻止损坏清单形成任意文件路径。
fn validateInfo(info: &ClientCertificateInfo) -> Result<(), SslMitmError> {
    if info.id.len() != 32 || !info.id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SslMitmError::InvalidClientCertificate);
    }
    validateNameAndLocations(&info.name, &info.locations)
}

/// 限制名称和 Location 规则，保证热路径只执行已经验证的匹配。
fn validateNameAndLocations(name: &str, locations: &[LocationPattern]) -> Result<(), SslMitmError> {
    if name.trim().is_empty() || name.chars().count() > 80 || locations.is_empty() {
        return Err(SslMitmError::InvalidClientCertificate);
    }
    locations.iter().try_for_each(|pattern| {
        validateLocationPattern(pattern).map_err(|_| SslMitmError::InvalidLocation)
    })
}

/// 从规范化材料文件恢复身份；私钥 DER 类型由 rustls 按标准容器自动识别。
fn loadIdentity(
    root: &Path,
    info: ClientCertificateInfo,
) -> Result<ClientCertificateIdentity, SslMitmError> {
    let chainBytes = fs::read(root.join(format!("{}.chain.pem", info.id)))
        .map_err(|_| SslMitmError::CertificateStorage)?;
    let keyBytes = fs::read(root.join(format!("{}.key.der", info.id)))
        .map_err(|_| SslMitmError::CertificateStorage)?;
    let mut reader = BufReader::new(chainBytes.as_slice());
    let chain = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SslMitmError::InvalidClientCertificate)?;
    let privateKey =
        PrivateKeyDer::try_from(keyBytes).map_err(|_| SslMitmError::InvalidPrivateKey)?;
    validateChain(&chain)?;
    validateIdentity(&chain, &privateKey)?;
    Ok(ClientCertificateIdentity {
        info,
        chain,
        privateKey,
    })
}

/// 写入规范化 PEM 链和 DER 私钥；文件名只来自校验后的哈希 ID。
fn persistIdentity(
    root: &Path,
    id: &str,
    chain: &[CertificateDer<'static>],
    privateKey: &PrivateKeyDer<'static>,
) -> Result<(), SslMitmError> {
    let mut chainPem = Vec::new();
    for certificate in chain {
        chainPem.extend_from_slice(b"-----BEGIN CERTIFICATE-----\n");
        let encoded = base64Standard.encode(certificate.as_ref());
        for chunk in encoded.as_bytes().chunks(64) {
            chainPem.extend_from_slice(chunk);
            chainPem.push(b'\n');
        }
        chainPem.extend_from_slice(b"-----END CERTIFICATE-----\n");
    }
    let chainPath = root.join(format!("{id}.chain.pem"));
    let keyPath = root.join(format!("{id}.key.der"));
    writeSynchronized(&chainPath, &chainPem)?;
    if let Err(error) = writeSynchronized(&keyPath, privateKey.secret_der()) {
        let _ = removeFileIfPresent(&chainPath);
        return Err(error);
    }
    Ok(())
}

/// 同步写入一个新文件；调用方保证目标文件不存在或已完成删除。
fn writeSynchronized(path: &Path, bytes: &[u8]) -> Result<(), SslMitmError> {
    let mut file = fs::File::create(path).map_err(|_| SslMitmError::CertificateStorage)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| SslMitmError::CertificateStorage)
}

/// 删除可能存在的事务文件；不存在视为已达到目标状态，其他 I/O 错误保持失败。
fn removeFileIfPresent(path: &Path) -> Result<(), SslMitmError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SslMitmError::CertificateStorage),
    }
}

/// 删除身份材料；清单已提交后，材料清理失败只留下不可达文件，不会重新启用身份。
fn removeIdentityFiles(root: &Path, id: &str) {
    for suffix in ["chain.pem", "key.der"] {
        if let Err(error) = fs::remove_file(root.join(format!("{id}.{suffix}")))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                errorCode = "sslClientCertificateCleanupFailed",
                identityId = id
            );
        }
    }
}

/// 把非负 X.509 秒时间转换为控制协议毫秒时间戳。
fn timestampMilliseconds(seconds: i64) -> Result<u64, SslMitmError> {
    u64::try_from(seconds)
        .ok()
        .and_then(|value| value.checked_mul(1_000))
        .ok_or(SslMitmError::InvalidClientCertificate)
}
