use std::{
    collections::{HashMap, VecDeque},
    fmt, fs,
    io::{BufReader, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use location_core::{
    LocationMatchOptions, LocationPattern, ResolvedLocation, locationMatches,
    validateLocationPattern,
};
use parking_lot::{Mutex, RwLock};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use x509_parser::parse_x509_certificate;

use crate::clientCertificate::{
    ClientCertificateIdentity, ClientCertificateImport, ClientCertificateInfo,
    ClientCertificateStore, ClientCertificateUpdate,
};

const rootCertificateFileName: &str = "rootCA.pem";
const rootPrivateKeyFileName: &str = "rootCA.key";
const rootCommonName: &str = "Local Proxy Root CA";
const defaultLeafCacheLimit: usize = 256;
const maximumLeafCacheLimit: usize = 4_096;
const rootValidityDays: i64 = 3_650;
const leafValidityDays: i64 = 365;

/// 保存 SSL 解密范围与证书缓存边界；include 为空时始终保持 CONNECT 裸隧道。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct SslMitmConfiguration {
    pub enabled: bool,
    pub includeLocations: Vec<LocationPattern>,
    pub excludeLocations: Vec<LocationPattern>,
    pub maxCachedCertificates: usize,
    pub useClientSni: bool,
}

impl Default for SslMitmConfiguration {
    /// 默认关闭解密且不隐式命中任何主机，避免安装根证书后意外扩大观察范围。
    fn default() -> Self {
        Self {
            enabled: false,
            includeLocations: Vec::new(),
            excludeLocations: Vec::new(),
            maxCachedCertificates: defaultLeafCacheLimit,
            useClientSni: true,
        }
    }
}

impl SslMitmConfiguration {
    /// 校验证书缓存边界与主机匹配规则；控制面可在持久化前复用该入口，避免落盘无效配置。
    pub fn validate(&self) -> Result<(), SslMitmError> {
        validateConfiguration(self)
    }
}

/// 描述可公开的根证书元数据；结构中刻意不存在私钥、密钥路径或密钥指纹。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CertificateAuthorityInfo {
    pub installed: bool,
    pub subject: String,
    pub validFromMilliseconds: u64,
    pub validToMilliseconds: u64,
    pub fingerprintSha256: String,
    pub pemPath: String,
}

/// 汇总 SSL 配置、证书状态、叶证书缓存和握手累计值，供控制面与 MCP 共用。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SslPublicState {
    pub enabled: bool,
    pub includeLocations: Vec<LocationPattern>,
    pub excludeLocations: Vec<LocationPattern>,
    pub maxCachedCertificates: usize,
    pub useClientSni: bool,
    pub ca: CertificateAuthorityInfo,
    pub cachedLeafCount: usize,
    pub handshakeSuccessTotal: u64,
    pub handshakeFailureTotal: u64,
    pub clientCertificates: Vec<ClientCertificateInfo>,
    pub supportedHttpVersions: Vec<String>,
}

/// 区分证书持久化、规则校验、叶证书生成和 TLS 配置失败，控制层据此映射稳定错误。
#[derive(Debug, Error)]
pub enum SslMitmError {
    #[error("error.ssl.certificateStorage")]
    CertificateStorage,
    #[error("error.ssl.invalidCertificate")]
    InvalidCertificate,
    #[error("error.ssl.invalidPrivateKey")]
    InvalidPrivateKey,
    #[error("error.ssl.invalidLocation")]
    InvalidLocation,
    #[error("error.ssl.invalidCacheLimit")]
    InvalidCacheLimit,
    #[error("error.ssl.invalidClientCertificate")]
    InvalidClientCertificate,
    #[error("error.ssl.duplicateClientCertificate")]
    DuplicateClientCertificate,
    #[error("error.ssl.clientCertificateNotFound")]
    ClientCertificateNotFound,
    #[error("error.ssl.clientCertificateLimit")]
    ClientCertificateLimit,
    #[error("error.ssl.certificateGeneration")]
    CertificateGeneration,
    #[error("error.ssl.systemRootsUnavailable")]
    SystemRootsUnavailable,
    #[error("error.ssl.tlsConfiguration")]
    TlsConfiguration,
}

impl SslMitmError {
    /// 返回跨 API、MCP 与日志稳定的机器码，不泄露证书目录和加密材料。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CertificateStorage => "sslCertificateStorage",
            Self::InvalidCertificate => "sslInvalidCertificate",
            Self::InvalidPrivateKey => "sslInvalidPrivateKey",
            Self::InvalidLocation => "sslInvalidLocation",
            Self::InvalidCacheLimit => "sslInvalidCacheLimit",
            Self::InvalidClientCertificate => "sslInvalidClientCertificate",
            Self::DuplicateClientCertificate => "sslDuplicateClientCertificate",
            Self::ClientCertificateNotFound => "sslClientCertificateNotFound",
            Self::ClientCertificateLimit => "sslClientCertificateLimit",
            Self::CertificateGeneration => "sslCertificateGeneration",
            Self::SystemRootsUnavailable => "sslSystemRootsUnavailable",
            Self::TlsConfiguration => "sslTlsConfiguration",
        }
    }

    /// 返回本地化目录使用的稳定键；原始 I/O 与解析错误不进入公开响应。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::CertificateStorage => "error.ssl.certificateStorage",
            Self::InvalidCertificate => "error.ssl.invalidCertificate",
            Self::InvalidPrivateKey => "error.ssl.invalidPrivateKey",
            Self::InvalidLocation => "error.ssl.invalidLocation",
            Self::InvalidCacheLimit => "error.ssl.invalidCacheLimit",
            Self::InvalidClientCertificate => "error.ssl.invalidClientCertificate",
            Self::DuplicateClientCertificate => "error.ssl.duplicateClientCertificate",
            Self::ClientCertificateNotFound => "error.ssl.clientCertificateNotFound",
            Self::ClientCertificateLimit => "error.ssl.clientCertificateLimit",
            Self::CertificateGeneration => "error.ssl.certificateGeneration",
            Self::SystemRootsUnavailable => "error.ssl.systemRootsUnavailable",
            Self::TlsConfiguration => "error.ssl.tlsConfiguration",
        }
    }
}

/// 共享证书颁发机构、运行配置和握手指标；克隆只复制 Arc，不复制任何私钥材料。
#[derive(Clone)]
pub struct SslMitmManager {
    inner: Arc<SslMitmInner>,
}

struct SslMitmInner {
    configuration: RwLock<SslMitmConfiguration>,
    authority: Mutex<CertificateAuthority>,
    upstreamRoots: Vec<CertificateDer<'static>>,
    clientCertificates: RwLock<ClientCertificateStore>,
    handshakeSuccessTotal: AtomicU64,
    handshakeFailureTotal: AtomicU64,
}

struct CertificateAuthority {
    certificateDirectory: PathBuf,
    rootPem: String,
    rootDer: CertificateDer<'static>,
    rootKeyPem: String,
    info: CertificateAuthorityInfo,
    leafCertificates: HashMap<String, Arc<CertifiedKey>>,
    leafOrder: VecDeque<String>,
}

/// 为单次下游 TLS 握手按 CONNECT 目标或客户端 SNI 选择叶证书。
struct LeafCertificateResolver {
    manager: SslMitmManager,
    connectHost: String,
    useClientSni: bool,
}

impl fmt::Debug for LeafCertificateResolver {
    /// 调试输出只包含公开主机和 SNI 策略，禁止派生 Debug 意外输出证书签名材料。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LeafCertificateResolver")
            .field("connectHost", &self.connectHost)
            .field("useClientSni", &self.useClientSni)
            .finish()
    }
}

impl ResolvesServerCert for LeafCertificateResolver {
    /// 在 rustls 同步 ClientHello 回调中命中有界缓存；生成失败返回 None 终止当前握手。
    fn resolve(&self, clientHello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let host = if self.useClientSni {
            clientHello.server_name().unwrap_or(&self.connectHost)
        } else {
            &self.connectHost
        };
        self.manager.leafCertificate(host).ok()
    }
}

impl SslMitmManager {
    /// 从用户数据目录加载唯一根 CA；首次运行原子生成 PEM 证书和 PKCS#8 私钥。
    pub fn load(certificateDirectory: impl AsRef<Path>) -> Result<Self, SslMitmError> {
        Self::loadWithUpstreamRoots(certificateDirectory, Vec::new())
    }

    /// 加载根 CA 并附加测试或私有上游信任根；附加根只存在内存，不写入用户目录。
    pub fn loadWithUpstreamRoots(
        certificateDirectory: impl AsRef<Path>,
        upstreamRoots: Vec<CertificateDer<'static>>,
    ) -> Result<Self, SslMitmError> {
        let authority = CertificateAuthority::loadOrGenerate(certificateDirectory.as_ref())?;
        let clientCertificates = ClientCertificateStore::load(certificateDirectory.as_ref())?;
        Ok(Self {
            inner: Arc::new(SslMitmInner {
                configuration: RwLock::new(SslMitmConfiguration::default()),
                authority: Mutex::new(authority),
                upstreamRoots,
                clientCertificates: RwLock::new(clientCertificates),
                handshakeSuccessTotal: AtomicU64::new(0),
                handshakeFailureTotal: AtomicU64::new(0),
            }),
        })
    }

    /// 校验并原子替换匹配规则与缓存上限；失败时旧配置和旧缓存均保持不变。
    pub fn updateConfiguration(
        &self,
        configuration: SslMitmConfiguration,
    ) -> Result<SslPublicState, SslMitmError> {
        validateConfiguration(&configuration)?;
        {
            let mut authority = self.inner.authority.lock();
            authority.trimLeafCache(configuration.maxCachedCertificates);
        }
        *self.inner.configuration.write() = configuration;
        Ok(self.publicState())
    }

    /// 克隆当前可持久化 SSL 配置；证书材料、缓存和握手计数不会进入返回值。
    pub fn configuration(&self) -> SslMitmConfiguration {
        self.inner.configuration.read().clone()
    }

    /// 返回不含私钥的完整公开状态；所有计数使用获取时刻的一致单值快照。
    pub fn publicState(&self) -> SslPublicState {
        let configuration = self.inner.configuration.read().clone();
        let authority = self.inner.authority.lock();
        SslPublicState {
            enabled: configuration.enabled,
            includeLocations: configuration.includeLocations,
            excludeLocations: configuration.excludeLocations,
            maxCachedCertificates: configuration.maxCachedCertificates,
            useClientSni: configuration.useClientSni,
            ca: authority.info.clone(),
            cachedLeafCount: authority.leafCertificates.len(),
            handshakeSuccessTotal: self.inner.handshakeSuccessTotal.load(Ordering::Relaxed),
            handshakeFailureTotal: self.inner.handshakeFailureTotal.load(Ordering::Relaxed),
            clientCertificates: self.inner.clientCertificates.read().publicState(),
            supportedHttpVersions: vec![
                "HTTP/1.0".to_owned(),
                "HTTP/1.1".to_owned(),
                "HTTP/2".to_owned(),
            ],
        }
    }

    /// 按 exclude 优先、include 必须显式命中的规则决定 CONNECT 是否进入解密链路。
    pub fn shouldIntercept(&self, location: &ResolvedLocation) -> bool {
        let configuration = self.inner.configuration.read();
        if !configuration.enabled || configuration.includeLocations.is_empty() {
            return false;
        }
        let options = LocationMatchOptions::default();
        if configuration
            .excludeLocations
            .iter()
            .any(|pattern| locationMatches(pattern, location, options).unwrap_or(false))
        {
            return false;
        }
        configuration
            .includeLocations
            .iter()
            .any(|pattern| locationMatches(pattern, location, options).unwrap_or(false))
    }

    /// 为一个 CONNECT 会话构造同时协商 HTTP/2 与 HTTP/1.1 的下游 TLS 配置。
    pub fn downstreamServerConfiguration(
        &self,
        connectHost: &str,
    ) -> Result<Arc<ServerConfig>, SslMitmError> {
        let useClientSni = self.inner.configuration.read().useClientSni;
        let resolver = Arc::new(LeafCertificateResolver {
            manager: self.clone(),
            connectHost: connectHost.to_owned(),
            useClientSni,
        });
        let mut configuration = ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        configuration.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Arc::new(configuration))
    }

    /// 构造验证系统信任根和显式附加根的上游 TLS 配置，不接受无验证连接。
    pub fn upstreamClientConfiguration(
        &self,
        location: Option<&ResolvedLocation>,
    ) -> Result<ClientConfig, SslMitmError> {
        let identity = location.and_then(|target| self.resolveClientCertificate(target));
        self.upstreamClientConfigurationForIdentity(identity)
    }

    /// 为一次已经原子选定的身份构建 TLS 配置；规则并发更新不会让缓存键与证书材料错配。
    pub(crate) fn upstreamClientConfigurationForIdentity(
        &self,
        identity: Option<ClientCertificateIdentity>,
    ) -> Result<ClientConfig, SslMitmError> {
        let nativeCertificates = rustls_native_certs::load_native_certs();
        let mut roots = RootCertStore::empty();
        let (accepted, _) = roots.add_parsable_certificates(nativeCertificates.certs);
        for certificate in &self.inner.upstreamRoots {
            roots
                .add(certificate.clone())
                .map_err(|_| SslMitmError::InvalidCertificate)?;
        }
        if accepted == 0 && self.inner.upstreamRoots.is_empty() {
            return Err(SslMitmError::SystemRootsUnavailable);
        }
        let builder = ClientConfig::builder().with_root_certificates(roots);
        let configuration = match identity {
            Some(identity) => builder
                .with_client_auth_cert(identity.chain, identity.privateKey)
                .map_err(|_| SslMitmError::TlsConfiguration)?,
            None => builder.with_no_client_auth(),
        };
        // hyper-rustls 根据启用的 HTTP/1 与 HTTP/2 模式写入 ALPN；这里必须保持为空，
        // 否则连接器会拒绝配置。客户端身份只改变证书材料，不改变协议协商顺序。
        Ok(configuration)
    }

    /// 导入并立即启用一条上游客户端身份；口令只在本次调用栈内用于解包。
    pub fn importClientCertificate(
        &self,
        input: ClientCertificateImport,
    ) -> Result<SslPublicState, SslMitmError> {
        let certificateDirectory = self.inner.authority.lock().certificateDirectory.clone();
        self.inner
            .clientCertificates
            .write()
            .import(&certificateDirectory, input)?;
        Ok(self.publicState())
    }

    /// 更新指定身份的名称、开关和 Location 规则；现有上游连接自然排空，新请求采用新规则。
    pub fn updateClientCertificate(
        &self,
        id: &str,
        update: ClientCertificateUpdate,
    ) -> Result<SslPublicState, SslMitmError> {
        let certificateDirectory = self.inner.authority.lock().certificateDirectory.clone();
        self.inner
            .clientCertificates
            .write()
            .update(&certificateDirectory, id, update)?;
        Ok(self.publicState())
    }

    /// 删除指定身份并使后续请求恢复普通单向 TLS；已建立的连接不会被强制中断。
    pub fn removeClientCertificate(&self, id: &str) -> Result<SslPublicState, SslMitmError> {
        let certificateDirectory = self.inner.authority.lock().certificateDirectory.clone();
        self.inner
            .clientCertificates
            .write()
            .remove(&certificateDirectory, id)?;
        Ok(self.publicState())
    }

    /// 为具体上游 Location 解析完整客户端身份；未命中规则时返回 None。
    pub(crate) fn resolveClientCertificate(
        &self,
        location: &ResolvedLocation,
    ) -> Option<ClientCertificateIdentity> {
        self.inner.clientCertificates.read().resolve(location)
    }

    /// 导出公开根证书 PEM 字节；返回值不与内部可变缓冲区共享。
    pub fn exportRootPem(&self) -> Vec<u8> {
        self.inner.authority.lock().rootPem.as_bytes().to_vec()
    }

    /// 导出标准 X.509 DER 字节，供 `.cer` 下载和操作系统证书导入。
    pub fn exportRootDer(&self) -> Vec<u8> {
        self.inner.authority.lock().rootDer.as_ref().to_vec()
    }

    /// 原子更换根 CA 并清空叶证书缓存；活动 TLS 会话继续持有旧证书直至自然结束。
    pub fn regenerateRoot(&self) -> Result<SslPublicState, SslMitmError> {
        let mut authority = self.inner.authority.lock();
        authority.regenerate()?;
        drop(authority);
        Ok(self.publicState())
    }

    /// 记录一次完整的下游 TLS 握手成功，不统计未命中规则的裸 CONNECT。
    pub(crate) fn recordHandshakeSuccess(&self) {
        self.inner
            .handshakeSuccessTotal
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次下游 TLS 握手失败；计数不包含原始错误文本或目标证书内容。
    pub(crate) fn recordHandshakeFailure(&self) {
        self.inner
            .handshakeFailureTotal
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 预热指定主机的叶证书缓存；证书仅驻留在内存，失败时不暴露密钥材料。
    pub fn primeLeafCertificate(&self, host: &str) -> Result<(), SslMitmError> {
        self.leafCertificate(host).map(|_| ())
    }

    /// 获取或签发一个 SAN 与目标主机一致的叶证书，并执行严格 FIFO 容量回收。
    fn leafCertificate(&self, host: &str) -> Result<Arc<CertifiedKey>, SslMitmError> {
        let normalizedHost = normalizeCertificateHost(host)?;
        let cacheLimit = self.inner.configuration.read().maxCachedCertificates;
        let mut authority = self.inner.authority.lock();
        if let Some(certificate) = authority.leafCertificates.get(&normalizedHost) {
            return Ok(certificate.clone());
        }
        let certificate = authority.issueLeaf(&normalizedHost)?;
        authority
            .leafCertificates
            .insert(normalizedHost.clone(), certificate.clone());
        authority.leafOrder.push_back(normalizedHost);
        authority.trimLeafCache(cacheLimit);
        Ok(certificate)
    }
}

impl CertificateAuthority {
    /// 加载完整证书对；任一文件缺失或不匹配时明确失败，禁止静默覆盖已有身份。
    fn loadOrGenerate(certificateDirectory: &Path) -> Result<Self, SslMitmError> {
        fs::create_dir_all(certificateDirectory).map_err(|_| SslMitmError::CertificateStorage)?;
        let certificatePath = certificateDirectory.join(rootCertificateFileName);
        let keyPath = certificateDirectory.join(rootPrivateKeyFileName);
        let certificateExists = certificatePath.exists();
        let keyExists = keyPath.exists();
        if certificateExists != keyExists {
            return Err(SslMitmError::CertificateStorage);
        }
        if !certificateExists {
            let generated = generateRootMaterial()?;
            persistRootMaterial(certificateDirectory, &generated)?;
        }
        let rootPem =
            fs::read_to_string(&certificatePath).map_err(|_| SslMitmError::CertificateStorage)?;
        let rootKeyPem =
            fs::read_to_string(&keyPath).map_err(|_| SslMitmError::CertificateStorage)?;
        let rootDer = parseSingleCertificate(&rootPem)?;
        validateIssuerPair(&rootPem, &rootKeyPem)?;
        let info = certificateInfo(&rootDer, &certificatePath)?;
        Ok(Self {
            certificateDirectory: certificateDirectory.to_owned(),
            rootPem,
            rootDer,
            rootKeyPem,
            info,
            leafCertificates: HashMap::new(),
            leafOrder: VecDeque::new(),
        })
    }

    /// 签发一年有效的服务端叶证书；DNS 与 IP SAN 均由 rcgen 从规范化主机推导。
    fn issueLeaf(&self, host: &str) -> Result<Arc<CertifiedKey>, SslMitmError> {
        let rootKey =
            KeyPair::from_pem(&self.rootKeyPem).map_err(|_| SslMitmError::InvalidPrivateKey)?;
        let issuer = Issuer::from_ca_cert_pem(&self.rootPem, rootKey)
            .map_err(|_| SslMitmError::InvalidCertificate)?;
        let leafKey = KeyPair::generate().map_err(|_| SslMitmError::CertificateGeneration)?;
        let mut parameters = CertificateParams::new(vec![host.to_owned()])
            .map_err(|_| SslMitmError::CertificateGeneration)?;
        let now = OffsetDateTime::now_utc();
        parameters.not_before = now - Duration::days(1);
        parameters.not_after = now + Duration::days(leafValidityDays);
        parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        parameters.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        parameters.distinguished_name.push(DnType::CommonName, host);
        let certificate = parameters
            .signed_by(&leafKey, &issuer)
            .map_err(|_| SslMitmError::CertificateGeneration)?;
        let privateKey = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leafKey.serialize_der()));
        let chain = vec![certificate.der().clone(), self.rootDer.clone()];
        let provider = aws_lc_rs::default_provider();
        let certifiedKey = CertifiedKey::from_der(chain, privateKey, &provider)
            .map_err(|_| SslMitmError::TlsConfiguration)?;
        Ok(Arc::new(certifiedKey))
    }

    /// 将缓存裁剪到最新配置上限；逐项删除确保映射和顺序队列始终同构。
    fn trimLeafCache(&mut self, limit: usize) {
        while self.leafCertificates.len() > limit {
            if let Some(host) = self.leafOrder.pop_front() {
                self.leafCertificates.remove(&host);
            }
        }
    }

    /// 生成并持久化新根后再切换内存状态，写入失败不会污染当前可用证书。
    fn regenerate(&mut self) -> Result<(), SslMitmError> {
        let generated = generateRootMaterial()?;
        persistRootMaterial(&self.certificateDirectory, &generated)?;
        let certificatePath = self.certificateDirectory.join(rootCertificateFileName);
        self.rootPem = generated.rootPem;
        self.rootDer = generated.rootDer;
        self.rootKeyPem = generated.rootKeyPem;
        self.info = certificateInfo(&self.rootDer, &certificatePath)?;
        self.leafCertificates.clear();
        self.leafOrder.clear();
        Ok(())
    }
}

struct GeneratedRootMaterial {
    rootPem: String,
    rootDer: CertificateDer<'static>,
    rootKeyPem: String,
}

/// 生成 ECDSA P-256 根 CA；十年有效期和签名用途明确写入证书扩展。
fn generateRootMaterial() -> Result<GeneratedRootMaterial, SslMitmError> {
    let key = KeyPair::generate().map_err(|_| SslMitmError::CertificateGeneration)?;
    let mut parameters = CertificateParams::new(Vec::<String>::new())
        .map_err(|_| SslMitmError::CertificateGeneration)?;
    let now = OffsetDateTime::now_utc();
    parameters.not_before = now - Duration::days(1);
    parameters.not_after = now + Duration::days(rootValidityDays);
    parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    parameters.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let mut distinguishedName = DistinguishedName::new();
    distinguishedName.push(DnType::CommonName, rootCommonName);
    distinguishedName.push(DnType::OrganizationName, "Local Proxy");
    parameters.distinguished_name = distinguishedName;
    let issuer = CertifiedIssuer::self_signed(parameters, key)
        .map_err(|_| SslMitmError::CertificateGeneration)?;
    Ok(GeneratedRootMaterial {
        rootPem: issuer.pem(),
        rootDer: issuer.der().clone(),
        rootKeyPem: issuer.key().serialize_pem(),
    })
}

/// 以同目录临时文件写入并替换证书对；每次失败都删除新文件且保留可回滚旧副本。
fn persistRootMaterial(
    certificateDirectory: &Path,
    generated: &GeneratedRootMaterial,
) -> Result<(), SslMitmError> {
    let certificatePath = certificateDirectory.join(rootCertificateFileName);
    let keyPath = certificateDirectory.join(rootPrivateKeyFileName);
    let certificateNext = certificateDirectory.join("rootCA.pem.next");
    let keyNext = certificateDirectory.join("rootCA.key.next");
    let certificateBackup = certificateDirectory.join("rootCA.pem.backup");
    let keyBackup = certificateDirectory.join("rootCA.key.backup");
    removeIfExists(&certificateNext)?;
    removeIfExists(&keyNext)?;
    removeIfExists(&certificateBackup)?;
    removeIfExists(&keyBackup)?;
    writePublicFile(&certificateNext, generated.rootPem.as_bytes())?;
    writePrivateFile(&keyNext, generated.rootKeyPem.as_bytes())?;
    let hadCurrent = certificatePath.exists() && keyPath.exists();
    if hadCurrent {
        fs::rename(&certificatePath, &certificateBackup)
            .map_err(|_| SslMitmError::CertificateStorage)?;
        if fs::rename(&keyPath, &keyBackup).is_err() {
            let _ = fs::rename(&certificateBackup, &certificatePath);
            return Err(SslMitmError::CertificateStorage);
        }
    }
    if fs::rename(&certificateNext, &certificatePath).is_err()
        || fs::rename(&keyNext, &keyPath).is_err()
    {
        let _ = removeIfExists(&certificatePath);
        let _ = removeIfExists(&keyPath);
        if hadCurrent {
            let _ = fs::rename(&certificateBackup, &certificatePath);
            let _ = fs::rename(&keyBackup, &keyPath);
        }
        return Err(SslMitmError::CertificateStorage);
    }
    // 新证书对完成双重 rename 后已成为唯一权威状态；备份清理失败不能把已提交结果伪装成失败，
    // 否则调用方会保留旧内存证书而磁盘已切换新证书。残留备份会在下次写入前再次严格清理。
    let certificateCleanup = removeIfExists(&certificateBackup);
    let keyCleanup = removeIfExists(&keyBackup);
    if certificateCleanup.is_err() || keyCleanup.is_err() {
        tracing::warn!(errorCode = "sslRootBackupCleanupFailed");
    }
    Ok(())
}

/// 写入公开证书并强制刷新到文件系统，避免进程崩溃留下仅缓存的成功状态。
fn writePublicFile(path: &Path, bytes: &[u8]) -> Result<(), SslMitmError> {
    let mut file = fs::File::create(path).map_err(|_| SslMitmError::CertificateStorage)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| SslMitmError::CertificateStorage)
}

/// 写入私钥并在 Unix 上固定为 0600；Windows 继承当前用户数据目录的用户级 ACL。
fn writePrivateFile(path: &Path, bytes: &[u8]) -> Result<(), SslMitmError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| SslMitmError::CertificateStorage)?;
        return file
            .write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| SslMitmError::CertificateStorage);
    }
    #[cfg(not(unix))]
    {
        writePublicFile(path, bytes)
    }
}

/// 删除可能存在的事务临时文件；不存在视为已达到目标状态。
fn removeIfExists(path: &Path) -> Result<(), SslMitmError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SslMitmError::CertificateStorage),
    }
}

/// 解析 PEM 中唯一证书；空文件、多证书和尾随证书都作为身份损坏处理。
fn parseSingleCertificate(pem: &str) -> Result<CertificateDer<'static>, SslMitmError> {
    let mut reader = BufReader::new(pem.as_bytes());
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| SslMitmError::InvalidCertificate)?;
    if certificates.len() != 1 {
        return Err(SslMitmError::InvalidCertificate);
    }
    certificates
        .into_iter()
        .next()
        .ok_or(SslMitmError::InvalidCertificate)
}

/// 用一次测试签发验证根证书和私钥属于同一签名身份，拒绝错配文件进入运行期。
fn validateIssuerPair(rootPem: &str, keyPem: &str) -> Result<(), SslMitmError> {
    let key = KeyPair::from_pem(keyPem).map_err(|_| SslMitmError::InvalidPrivateKey)?;
    let issuer =
        Issuer::from_ca_cert_pem(rootPem, key).map_err(|_| SslMitmError::InvalidCertificate)?;
    let leafKey = KeyPair::generate().map_err(|_| SslMitmError::CertificateGeneration)?;
    CertificateParams::new(vec!["identity-check.invalid".to_owned()])
        .map_err(|_| SslMitmError::CertificateGeneration)?
        .signed_by(&leafKey, &issuer)
        .map_err(|_| SslMitmError::InvalidPrivateKey)?;
    Ok(())
}

/// 从 X.509 DER 生成公开主题、有效期和 SHA-256 指纹，不读取或派生私钥标识。
fn certificateInfo(
    certificate: &CertificateDer<'static>,
    certificatePath: &Path,
) -> Result<CertificateAuthorityInfo, SslMitmError> {
    let (_, parsed) = parse_x509_certificate(certificate.as_ref())
        .map_err(|_| SslMitmError::InvalidCertificate)?;
    let validFromMilliseconds = secondsToMilliseconds(parsed.validity().not_before.timestamp())?;
    let validToMilliseconds = secondsToMilliseconds(parsed.validity().not_after.timestamp())?;
    let fingerprint = Sha256::digest(certificate.as_ref())
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    Ok(CertificateAuthorityInfo {
        installed: true,
        subject: parsed.subject().to_string(),
        validFromMilliseconds,
        validToMilliseconds,
        fingerprintSha256: fingerprint,
        pemPath: certificatePath.to_string_lossy().into_owned(),
    })
}

/// 将非负 X.509 秒时间安全转换成前端统一的毫秒时间戳。
fn secondsToMilliseconds(seconds: i64) -> Result<u64, SslMitmError> {
    u64::try_from(seconds)
        .ok()
        .and_then(|value| value.checked_mul(1_000))
        .ok_or(SslMitmError::InvalidCertificate)
}

/// 校验所有 Location 与叶缓存上限；调用方可据此保证匹配热路径不再产生错误。
fn validateConfiguration(configuration: &SslMitmConfiguration) -> Result<(), SslMitmError> {
    if !(1..=maximumLeafCacheLimit).contains(&configuration.maxCachedCertificates) {
        return Err(SslMitmError::InvalidCacheLimit);
    }
    configuration
        .includeLocations
        .iter()
        .chain(&configuration.excludeLocations)
        .try_for_each(|pattern| {
            validateLocationPattern(pattern).map_err(|_| SslMitmError::InvalidLocation)
        })
}

/// 规范化证书缓存键；IP 字面量保留数值格式，DNS 主机统一小写并拒绝通配符。
fn normalizeCertificateHost(host: &str) -> Result<String, SslMitmError> {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    if unbracketed.is_empty()
        || unbracketed.contains('*')
        || unbracketed.chars().any(char::is_whitespace)
    {
        return Err(SslMitmError::CertificateGeneration);
    }
    if let Ok(address) = unbracketed.parse::<IpAddr>() {
        return Ok(address.to_string());
    }
    Ok(unbracketed.to_ascii_lowercase())
}
