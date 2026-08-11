use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, StatusCode, Uri,
    header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderName},
    uri::Authority,
};
use location_core::{
    LocationMatchOptions, LocationPattern, ResolvedLocation, locationMatches,
    validateLocationPattern,
};
use mime_guess::from_path;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const defaultMapLocalBodyLimit: u64 = 64 * 1024 * 1024;
const maximumRuleIdLength: usize = 128;
const maximumMapRuleCount: usize = 2_000;
const maximumLocalPathLength: usize = 4_096;
const maximumResponseHeaderCount: usize = 128;
const maximumContentTypeOverrideLength: usize = 512;
const maximumRemotePathLength: usize = 2_048;
const directoryIndexFileName: &str = "index.html";

/// Map Remote 的目标模板；空字段保持当前请求对应字段，path 中的 `*` 按 from.path 的捕获顺序替换。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct MapRemoteTarget {
    pub protocol: String,
    pub host: String,
    pub port: String,
    pub path: String,
}

/// 一条按顺序匹配的出站目标映射规则；同一请求只会应用首条命中的已启用规则。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapRemoteRule {
    pub id: String,
    pub enabled: bool,
    #[serde(rename = "from")]
    pub r#from: LocationPattern,
    pub to: MapRemoteTarget,
}

/// Map Remote 的完整热更新配置；rules 保持数组顺序，首条命中规则优先。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct MapRemoteConfiguration {
    pub enabled: bool,
    pub rules: Vec<MapRemoteRule>,
}

impl MapRemoteConfiguration {
    /// 校验远程映射的来源范围、目标模板、标识与规则上限；失败时调用方不得持久化候选规则。
    pub fn validate(&self) -> Result<(), MapToolError> {
        validateMapRemoteConfiguration(self)
    }
}

/// Map Local 附加响应头的 JSON 形状；名称和值在配置写入时完成 HTTP 语法校验。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapResponseHeader {
    pub name: String,
    pub value: String,
}

/// 一条本地文件或目录映射规则；目录模式以请求 URL path 相对拼接并拒绝越出映射根的路径。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapLocalRule {
    pub id: String,
    pub enabled: bool,
    pub location: LocationPattern,
    pub localPath: String,
    pub isDirectory: bool,
    #[serde(default = "defaultStatusCode")]
    pub statusCode: u16,
    #[serde(default)]
    pub responseHeaders: Vec<MapResponseHeader>,
    #[serde(default)]
    pub contentTypeOverride: String,
}

/// Map Local 的完整热更新配置；rules 保持数组顺序，首条命中规则优先。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct MapLocalConfiguration {
    pub enabled: bool,
    pub rules: Vec<MapLocalRule>,
}

impl MapLocalConfiguration {
    /// 校验本地映射的位置、相对路径、响应头和规则上限；文件是否存在仍在请求命中时判断。
    pub fn validate(&self) -> Result<(), MapToolError> {
        validateMapLocalConfiguration(self)
    }
}

/// 描述 Map Remote 的一次命中，供流水线保留原始 Location、更新出站位置并写入事务痕迹。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MapRemoteApplication {
    pub ruleId: String,
    pub originalLocation: ResolvedLocation,
    pub mappedLocation: ResolvedLocation,
    pub appliedTool: String,
}

impl MapRemoteApplication {
    /// 将映射后的 Location 组装为上游绝对 URI；失败说明配置生成了 HTTP 栈无法发送的目标。
    pub fn upstreamUri(&self) -> Result<Uri, MapToolError> {
        buildLocationUri(&self.mappedLocation)
    }

    /// 生成与映射后 authority 一致的 Host 头；默认端口按 HTTP 语义省略，IPv6 保留方括号。
    pub fn hostHeader(&self) -> Result<HeaderValue, MapToolError> {
        HeaderValue::from_str(&hostHeaderValue(&self.mappedLocation))
            .map_err(|_| MapToolError::InvalidMappedUri)
    }
}

/// 标记本地合成响应的来源，使流水线在 403/404 等短路场景仍能记录明确的事务痕迹。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapLocalResponseSource {
    File,
    Missing,
    PathTraversal,
    BodyLimitExceeded,
}

/// Map Local 命中后交给流水线的合成响应；pipeline 应设置 shortCircuit 后继续运行响应钩子和录制。
#[derive(Clone, Debug)]
pub struct MapLocalResponse {
    pub ruleId: String,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub contentType: String,
    pub source: MapLocalResponseSource,
    pub appliedTool: String,
}

/// Map Local 请求阶段的稳定结果；PassThrough 表示无需短路，Synthetic 必须跳过出站连接。
#[derive(Clone, Debug)]
pub enum MapLocalResolution {
    PassThrough,
    Synthetic(Box<MapLocalResponse>),
}

impl MapLocalResolution {
    /// 返回命中后的合成响应引用，供 PipelineTool 适配层直接转换为 ShortCircuit 指令。
    pub fn syntheticResponse(&self) -> Option<&MapLocalResponse> {
        match self {
            Self::PassThrough => None,
            Self::Synthetic(response) => Some(response.as_ref()),
        }
    }
}

/// 描述规则校验、路径解析和本地读取的稳定错误；错误文本不携带用户文件系统路径。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MapToolError {
    #[error("error.map.invalidRuleId")]
    InvalidRuleId,
    #[error("error.map.ruleLimitExceeded")]
    RuleLimitExceeded,
    #[error("error.map.duplicateRuleId")]
    DuplicateRuleId,
    #[error("error.map.invalidLocation")]
    InvalidLocation,
    #[error("error.map.invalidRemoteProtocol")]
    InvalidRemoteProtocol,
    #[error("error.map.invalidRemoteHost")]
    InvalidRemoteHost,
    #[error("error.map.invalidRemotePort")]
    InvalidRemotePort,
    #[error("error.map.invalidRemotePath")]
    InvalidRemotePath,
    #[error("error.map.remotePathTooLong")]
    RemotePathTooLong,
    #[error("error.map.invalidPathTemplate")]
    InvalidPathTemplate,
    #[error("error.map.invalidLocalPath")]
    InvalidLocalPath,
    #[error("error.map.localPathTooLong")]
    LocalPathTooLong,
    #[error("error.map.invalidStatusCode")]
    InvalidStatusCode,
    #[error("error.map.invalidHeaderName")]
    InvalidHeaderName,
    #[error("error.map.invalidHeaderValue")]
    InvalidHeaderValue,
    #[error("error.map.responseHeaderLimitExceeded")]
    ResponseHeaderLimitExceeded,
    #[error("error.map.contentTypeTooLong")]
    ContentTypeTooLong,
    #[error("error.map.forbiddenResponseHeader")]
    ForbiddenResponseHeader,
    #[error("error.map.invalidBodyLimit")]
    InvalidBodyLimit,
    #[error("error.map.mappingRootUnavailable")]
    MappingRootUnavailable,
    #[error("error.map.localFileReadFailed")]
    LocalFileReadFailed,
    #[error("error.map.invalidMappedUri")]
    InvalidMappedUri,
}

impl MapToolError {
    /// 返回供控制面和日志使用的稳定机器错误码；不暴露规则路径或操作系统 I/O 原文。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidRuleId => "mapInvalidRuleId",
            Self::RuleLimitExceeded => "mapRuleLimitExceeded",
            Self::DuplicateRuleId => "mapDuplicateRuleId",
            Self::InvalidLocation => "mapInvalidLocation",
            Self::InvalidRemoteProtocol => "mapInvalidRemoteProtocol",
            Self::InvalidRemoteHost => "mapInvalidRemoteHost",
            Self::InvalidRemotePort => "mapInvalidRemotePort",
            Self::InvalidRemotePath => "mapInvalidRemotePath",
            Self::RemotePathTooLong => "mapRemotePathTooLong",
            Self::InvalidPathTemplate => "mapInvalidPathTemplate",
            Self::InvalidLocalPath => "mapInvalidLocalPath",
            Self::LocalPathTooLong => "mapLocalPathTooLong",
            Self::InvalidStatusCode => "mapInvalidStatusCode",
            Self::InvalidHeaderName => "mapInvalidHeaderName",
            Self::InvalidHeaderValue => "mapInvalidHeaderValue",
            Self::ResponseHeaderLimitExceeded => "mapResponseHeaderLimitExceeded",
            Self::ContentTypeTooLong => "mapContentTypeTooLong",
            Self::ForbiddenResponseHeader => "mapForbiddenResponseHeader",
            Self::InvalidBodyLimit => "mapInvalidBodyLimit",
            Self::MappingRootUnavailable => "mapMappingRootUnavailable",
            Self::LocalFileReadFailed => "mapLocalFileReadFailed",
            Self::InvalidMappedUri => "mapInvalidMappedUri",
        }
    }

    /// 返回供后续 I18N catalog 映射的稳定键；模块本身不生成单语言客户端正文。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::InvalidRuleId => "error.map.invalidRuleId",
            Self::RuleLimitExceeded => "error.map.ruleLimitExceeded",
            Self::DuplicateRuleId => "error.map.duplicateRuleId",
            Self::InvalidLocation => "error.map.invalidLocation",
            Self::InvalidRemoteProtocol => "error.map.invalidRemoteProtocol",
            Self::InvalidRemoteHost => "error.map.invalidRemoteHost",
            Self::InvalidRemotePort => "error.map.invalidRemotePort",
            Self::InvalidRemotePath => "error.map.invalidRemotePath",
            Self::RemotePathTooLong => "error.map.remotePathTooLong",
            Self::InvalidPathTemplate => "error.map.invalidPathTemplate",
            Self::InvalidLocalPath => "error.map.invalidLocalPath",
            Self::LocalPathTooLong => "error.map.localPathTooLong",
            Self::InvalidStatusCode => "error.map.invalidStatusCode",
            Self::InvalidHeaderName => "error.map.invalidHeaderName",
            Self::InvalidHeaderValue => "error.map.invalidHeaderValue",
            Self::ResponseHeaderLimitExceeded => "error.map.responseHeaderLimitExceeded",
            Self::ContentTypeTooLong => "error.map.contentTypeTooLong",
            Self::ForbiddenResponseHeader => "error.map.forbiddenResponseHeader",
            Self::InvalidBodyLimit => "error.map.invalidBodyLimit",
            Self::MappingRootUnavailable => "error.map.mappingRootUnavailable",
            Self::LocalFileReadFailed => "error.map.localFileReadFailed",
            Self::InvalidMappedUri => "error.map.invalidMappedUri",
        }
    }
}

/// 持有可热更新 Map Remote 规则的线程安全工具实例；克隆只复制共享配置句柄。
#[derive(Clone)]
pub struct MapRemoteTool {
    configuration: Arc<RwLock<MapRemoteConfiguration>>,
}

impl MapRemoteTool {
    /// 创建规则已校验的 Map Remote 工具；任一规则无效时不创建部分可用实例。
    pub fn new(configuration: MapRemoteConfiguration) -> Result<Self, MapToolError> {
        validateMapRemoteConfiguration(&configuration)?;
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
        })
    }

    /// 返回当前完整配置快照，供控制面构造权威资源视图而不暴露内部锁。
    pub fn configuration(&self) -> MapRemoteConfiguration {
        self.configuration.read().clone()
    }

    /// 先校验后一次替换全部规则；失败时旧配置保持生效，避免请求观察到半写入状态。
    pub fn updateConfiguration(
        &self,
        configuration: MapRemoteConfiguration,
    ) -> Result<MapRemoteConfiguration, MapToolError> {
        validateMapRemoteConfiguration(&configuration)?;
        *self.configuration.write() = configuration;
        Ok(self.configuration())
    }

    /// 使用首条命中规则构造映射后的目标；只应用一次，因此规则环不会在单个请求内循环。
    pub fn applyRemote(
        &self,
        location: &ResolvedLocation,
    ) -> Result<Option<MapRemoteApplication>, MapToolError> {
        let configuration = self.configuration();
        if !configuration.enabled {
            return Ok(None);
        }
        for rule in configuration.rules.iter().filter(|rule| rule.enabled) {
            if !locationMatches(&rule.r#from, location, LocationMatchOptions::default())
                .map_err(|_| MapToolError::InvalidLocation)?
            {
                continue;
            }
            let mappedLocation = applyMapRemoteRule(rule, location)?;
            return Ok(Some(MapRemoteApplication {
                ruleId: rule.id.clone(),
                originalLocation: location.clone(),
                mappedLocation,
                appliedTool: appliedToolName("mapRemote", &rule.id),
            }));
        }
        Ok(None)
    }
}

/// 持有可热更新 Map Local 规则和稳定用户映射根；根只参与相对 localPath 的解析。
#[derive(Clone)]
pub struct MapLocalTool {
    configuration: Arc<RwLock<MapLocalConfiguration>>,
    mappingRoot: Arc<PathBuf>,
    maximumBodyBytes: u64,
}

impl MapLocalTool {
    /// 以显式用户映射根创建工具；相对 localPath 永远相对此根，不依赖代理进程当前工作目录。
    pub fn new(
        configuration: MapLocalConfiguration,
        mappingRoot: impl AsRef<Path>,
    ) -> Result<Self, MapToolError> {
        Self::withMaximumBodyBytes(configuration, mappingRoot, defaultMapLocalBodyLimit)
    }

    /// 创建带单响应读取上限的工具；上限约束内存型合成响应，正常录制仍由 capture 的独立限额管理。
    pub fn withMaximumBodyBytes(
        configuration: MapLocalConfiguration,
        mappingRoot: impl AsRef<Path>,
        maximumBodyBytes: u64,
    ) -> Result<Self, MapToolError> {
        validateMapLocalConfiguration(&configuration)?;
        if maximumBodyBytes == 0 {
            return Err(MapToolError::InvalidBodyLimit);
        }
        let mappingRoot = absolutePath(mappingRoot.as_ref())?;
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
            mappingRoot: Arc::new(mappingRoot),
            maximumBodyBytes,
        })
    }

    /// 返回当前完整配置快照，供控制面读取并保持配置与数据面共享同一权威来源。
    pub fn configuration(&self) -> MapLocalConfiguration {
        self.configuration.read().clone()
    }

    /// 返回规范化后的用户映射根；该路径不含任何规则的本地文件信息。
    pub fn mappingRoot(&self) -> &Path {
        self.mappingRoot.as_ref()
    }

    /// 先校验后一次替换全部规则；失败时旧配置保持可用，不产生字段级部分热更新。
    pub fn updateConfiguration(
        &self,
        configuration: MapLocalConfiguration,
    ) -> Result<MapLocalConfiguration, MapToolError> {
        validateMapLocalConfiguration(&configuration)?;
        *self.configuration.write() = configuration;
        Ok(self.configuration())
    }

    /// 解析首条命中规则为合成响应；命中后无论文件成功、缺失或路径被拒绝都必须由流水线短路出站。
    pub async fn resolveLocal(
        &self,
        location: &ResolvedLocation,
    ) -> Result<MapLocalResolution, MapToolError> {
        let configuration = self.configuration();
        if !configuration.enabled {
            return Ok(MapLocalResolution::PassThrough);
        }
        for rule in configuration.rules.iter().filter(|rule| rule.enabled) {
            if !locationMatches(&rule.location, location, LocationMatchOptions::default())
                .map_err(|_| MapToolError::InvalidLocation)?
            {
                continue;
            }
            let response = if rule.isDirectory {
                self.resolveDirectoryRule(rule, location).await?
            } else {
                self.resolveFileRule(rule).await?
            };
            return Ok(MapLocalResolution::Synthetic(Box::new(response)));
        }
        Ok(MapLocalResolution::PassThrough)
    }

    /// 读取文件型映射的目标；配置路径缺失和目标为目录时按设计合成为 404。
    async fn resolveFileRule(&self, rule: &MapLocalRule) -> Result<MapLocalResponse, MapToolError> {
        let path = self.resolveRulePath(&rule.localPath);
        let canonicalPath = match canonicalizePath(&path).await? {
            Some(path) => path,
            None => return self.missingResponse(rule),
        };
        let metadata = metadataForPath(&canonicalPath).await?;
        if !metadata.is_file() {
            return self.missingResponse(rule);
        }
        self.readMappedFile(rule, canonicalPath, metadata.len())
            .await
    }

    /// 读取目录型映射；请求路径先经 percent 解码和段级规范化，再以 canonical 路径验证没有逃离根目录。
    async fn resolveDirectoryRule(
        &self,
        rule: &MapLocalRule,
        location: &ResolvedLocation,
    ) -> Result<MapLocalResponse, MapToolError> {
        let relativePath = match decodeRelativeRequestPath(&location.path) {
            Ok(path) => path,
            Err(()) => return self.pathTraversalResponse(rule),
        };
        let rootPath = self.resolveRulePath(&rule.localPath);
        let canonicalRoot = match canonicalizePath(&rootPath).await? {
            Some(path) => path,
            None => return self.missingResponse(rule),
        };
        let rootMetadata = metadataForPath(&canonicalRoot).await?;
        if !rootMetadata.is_dir() {
            return self.missingResponse(rule);
        }
        let candidatePath = canonicalRoot.join(relativePath);
        let canonicalCandidate = match canonicalizePath(&candidatePath).await? {
            Some(path) => path,
            None => return self.missingResponse(rule),
        };
        if !canonicalCandidate.starts_with(&canonicalRoot) {
            return self.pathTraversalResponse(rule);
        }
        let candidateMetadata = metadataForPath(&canonicalCandidate).await?;
        let canonicalFile = if candidateMetadata.is_dir() {
            let indexPath = canonicalCandidate.join(directoryIndexFileName);
            match canonicalizePath(&indexPath).await? {
                Some(path) => path,
                None => return self.missingResponse(rule),
            }
        } else {
            canonicalCandidate
        };
        if !canonicalFile.starts_with(&canonicalRoot) {
            return self.pathTraversalResponse(rule);
        }
        let fileMetadata = metadataForPath(&canonicalFile).await?;
        if !fileMetadata.is_file() {
            return self.missingResponse(rule);
        }
        self.readMappedFile(rule, canonicalFile, fileMetadata.len())
            .await
    }

    /// 在读取前检查已知文件长度，避免内存型合成响应无限占用；超限仍短路并返回明确状态。
    async fn readMappedFile(
        &self,
        rule: &MapLocalRule,
        canonicalPath: PathBuf,
        fileLength: u64,
    ) -> Result<MapLocalResponse, MapToolError> {
        if fileLength > self.maximumBodyBytes {
            return self.bodyLimitResponse(rule);
        }
        let body = tokio::fs::read(&canonicalPath)
            .await
            .map_err(|_| MapToolError::LocalFileReadFailed)?;
        if body.len() as u64 > self.maximumBodyBytes {
            return self.bodyLimitResponse(rule);
        }
        self.fileResponse(rule, canonicalPath, Bytes::from(body))
    }

    /// 解析规则配置中的本地路径；相对路径始终以显式映射根目录为基准，不依赖进程工作目录。
    pub fn resolveRulePath(&self, localPath: &str) -> PathBuf {
        let localPath = PathBuf::from(localPath);
        if localPath.is_absolute() {
            localPath
        } else {
            self.mappingRoot.join(localPath)
        }
    }

    /// 为正常文件构造配置化状态、响应头和 MIME；Content-Length 始终由实际正文重算。
    fn fileResponse(
        &self,
        rule: &MapLocalRule,
        path: PathBuf,
        body: Bytes,
    ) -> Result<MapLocalResponse, MapToolError> {
        let contentType = configuredContentType(rule, Some(&path))?;
        let headers = buildResponseHeaders(rule, Some(&contentType), body.len())?;
        Ok(MapLocalResponse {
            ruleId: rule.id.clone(),
            status: StatusCode::from_u16(rule.statusCode)
                .map_err(|_| MapToolError::InvalidStatusCode)?,
            headers,
            body,
            contentType,
            source: MapLocalResponseSource::File,
            appliedTool: appliedToolName("mapLocal", &rule.id),
        })
    }

    /// 为缺失文件或目录合成空 404；命中规则后不回源，保证 Map Local 的短路语义。
    fn missingResponse(&self, rule: &MapLocalRule) -> Result<MapLocalResponse, MapToolError> {
        self.emptyResponse(rule, StatusCode::NOT_FOUND, MapLocalResponseSource::Missing)
    }

    /// 为规范化后越出目录根的路径合成空 403，不读取或探测根目录以外的任何文件。
    fn pathTraversalResponse(&self, rule: &MapLocalRule) -> Result<MapLocalResponse, MapToolError> {
        self.emptyResponse(
            rule,
            StatusCode::FORBIDDEN,
            MapLocalResponseSource::PathTraversal,
        )
    }

    /// 为超过合成响应内存预算的文件合成空 413；仍让录制显示一次已命中的本地规则。
    fn bodyLimitResponse(&self, rule: &MapLocalRule) -> Result<MapLocalResponse, MapToolError> {
        self.emptyResponse(
            rule,
            StatusCode::PAYLOAD_TOO_LARGE,
            MapLocalResponseSource::BodyLimitExceeded,
        )
    }

    /// 统一构造无正文的本地短路响应，保证错误状态也携带规则痕迹并走后续响应钩子。
    fn emptyResponse(
        &self,
        rule: &MapLocalRule,
        status: StatusCode,
        source: MapLocalResponseSource,
    ) -> Result<MapLocalResponse, MapToolError> {
        Ok(MapLocalResponse {
            ruleId: rule.id.clone(),
            status,
            headers: buildResponseHeaders(rule, None, 0)?,
            body: Bytes::new(),
            contentType: String::new(),
            source,
            appliedTool: appliedToolName("mapLocal", &rule.id),
        })
    }
}

/// 返回 JSON 解码时使用的默认成功状态；显式零值不会被静默解释为 HTTP 200。
const fn defaultStatusCode() -> u16 {
    200
}

/// 校验全部 Map Remote 规则并拒绝重复 ID；写配置前完成以保证热更新原子性。
fn validateMapRemoteConfiguration(
    configuration: &MapRemoteConfiguration,
) -> Result<(), MapToolError> {
    validateRuleCount(configuration.rules.len())?;
    let mut ruleIds = HashSet::with_capacity(configuration.rules.len());
    for rule in &configuration.rules {
        validateRuleId(&rule.id, &mut ruleIds)?;
        validateLocationPattern(&rule.r#from).map_err(|_| MapToolError::InvalidLocation)?;
        validateMapRemoteTarget(&rule.r#from, &rule.to)?;
    }
    Ok(())
}

/// 校验全部 Map Local 规则并拒绝会破坏 HTTP 响应边界的头字段；不要求目标文件在保存时已存在。
fn validateMapLocalConfiguration(
    configuration: &MapLocalConfiguration,
) -> Result<(), MapToolError> {
    validateRuleCount(configuration.rules.len())?;
    let mut ruleIds = HashSet::with_capacity(configuration.rules.len());
    for rule in &configuration.rules {
        validateRuleId(&rule.id, &mut ruleIds)?;
        validateLocationPattern(&rule.location).map_err(|_| MapToolError::InvalidLocation)?;
        if rule.localPath.trim().is_empty() {
            return Err(MapToolError::InvalidLocalPath);
        }
        if rule.localPath.len() > maximumLocalPathLength {
            return Err(MapToolError::LocalPathTooLong);
        }
        if !(100..=599).contains(&rule.statusCode) {
            return Err(MapToolError::InvalidStatusCode);
        }
        validateResponseHeaders(rule)?;
    }
    Ok(())
}

/// 在为规则 ID 建立集合前限制规则总数，避免无界配置先触发与规则数量成比例的分配。
fn validateRuleCount(ruleCount: usize) -> Result<(), MapToolError> {
    if ruleCount > maximumMapRuleCount {
        return Err(MapToolError::RuleLimitExceeded);
    }
    Ok(())
}

/// 校验规则 ID 的稳定、紧凑形状；该值会写入事务 appliedTools，因此禁止空白和无界文本。
fn validateRuleId(ruleId: &str, ruleIds: &mut HashSet<String>) -> Result<(), MapToolError> {
    if ruleId.is_empty()
        || ruleId.len() > maximumRuleIdLength
        || !ruleId
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(MapToolError::InvalidRuleId);
    }
    if !ruleIds.insert(ruleId.to_owned()) {
        return Err(MapToolError::DuplicateRuleId);
    }
    Ok(())
}

/// 校验 Map Remote 输出模板；输出 host/port 必须是单值，path 的替换占位数量不得超过 from.path 可捕获的星号数。
fn validateMapRemoteTarget(
    source: &LocationPattern,
    target: &MapRemoteTarget,
) -> Result<(), MapToolError> {
    if !target.protocol.is_empty()
        && (target.protocol == "*"
            || !matches!(
                target.protocol.to_ascii_lowercase().as_str(),
                "http" | "https" | "ws" | "wss"
            ))
    {
        return Err(MapToolError::InvalidRemoteProtocol);
    }
    if !target.host.is_empty() {
        if target.host.contains('*')
            || target.host.contains('?')
            || target.host.chars().any(char::is_whitespace)
            || target.host.contains('/')
            || target.host.contains('#')
        {
            return Err(MapToolError::InvalidRemoteHost);
        }
        let authority = bracketedHost(&target.host);
        Authority::from_str(&authority).map_err(|_| MapToolError::InvalidRemoteHost)?;
    }
    if !target.port.is_empty() {
        target
            .port
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or(MapToolError::InvalidRemotePort)?;
    }
    if !target.path.is_empty() {
        if target.path.len() > maximumRemotePathLength {
            return Err(MapToolError::RemotePathTooLong);
        }
        if !target.path.starts_with('/') || target.path.contains('?') || target.path.contains('#') {
            return Err(MapToolError::InvalidRemotePath);
        }
        let captures = source.path.matches('*').count();
        if target.path.matches('*').count() > captures {
            return Err(MapToolError::InvalidPathTemplate);
        }
    }
    Ok(())
}

/// 校验本地响应头和类型覆盖；Content-Length 与 hop-by-hop 字段由代理生成，规则不得伪造。
fn validateResponseHeaders(rule: &MapLocalRule) -> Result<(), MapToolError> {
    if rule.responseHeaders.len() > maximumResponseHeaderCount {
        return Err(MapToolError::ResponseHeaderLimitExceeded);
    }
    if rule.contentTypeOverride.len() > maximumContentTypeOverrideLength {
        return Err(MapToolError::ContentTypeTooLong);
    }
    if !rule.contentTypeOverride.is_empty()
        && HeaderValue::from_str(&rule.contentTypeOverride).is_err()
    {
        return Err(MapToolError::InvalidHeaderValue);
    }
    for header in &rule.responseHeaders {
        let headerName = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| MapToolError::InvalidHeaderName)?;
        HeaderValue::from_str(&header.value).map_err(|_| MapToolError::InvalidHeaderValue)?;
        if headerName == CONTENT_LENGTH || isHopByHopHeader(&headerName) {
            return Err(MapToolError::ForbiddenResponseHeader);
        }
    }
    Ok(())
}

/// 按固定映射规则构造新的 ResolvedLocation；query 始终保持，避免路径映射意外吞掉请求参数。
fn applyMapRemoteRule(
    rule: &MapRemoteRule,
    location: &ResolvedLocation,
) -> Result<ResolvedLocation, MapToolError> {
    let protocol = if rule.to.protocol.is_empty() {
        location.protocol.clone()
    } else {
        rule.to.protocol.to_ascii_lowercase()
    };
    let host = if rule.to.host.is_empty() {
        location.host.clone()
    } else {
        normalizeTargetHost(&rule.to.host)
    };
    let port = if rule.to.port.is_empty() {
        location.port
    } else {
        rule.to
            .port
            .parse::<u16>()
            .map_err(|_| MapToolError::InvalidRemotePort)?
    };
    let path = mapTargetPath(&rule.r#from.path, &rule.to.path, &location.path)?;
    let mut mapped = ResolvedLocation {
        protocol,
        host,
        port,
        path,
        query: location.query.clone(),
        display: String::new(),
    };
    mapped.display = buildLocationUri(&mapped)?.to_string();
    Ok(mapped)
}

/// 将 from.path 中每个 `*` 的捕获按顺序填入 to.path；空 to.path 保持原路径以保留未映射部分。
fn mapTargetPath(
    sourcePattern: &str,
    targetPattern: &str,
    sourcePath: &str,
) -> Result<String, MapToolError> {
    if targetPattern.is_empty() {
        return Ok(sourcePath.to_owned());
    }
    if !targetPattern.contains('*') {
        return Ok(targetPattern.to_owned());
    }
    let captures =
        capturePathWildcards(sourcePattern, sourcePath).ok_or(MapToolError::InvalidPathTemplate)?;
    let mut captureIndex = 0_usize;
    let mut mapped = String::with_capacity(targetPattern.len() + sourcePath.len());
    for character in targetPattern.chars() {
        if character != '*' {
            mapped.push(character);
            continue;
        }
        let capture = captures
            .get(captureIndex)
            .ok_or(MapToolError::InvalidPathTemplate)?;
        mapped.push_str(capture);
        captureIndex += 1;
    }
    Ok(mapped)
}

/// 从与 Location 语义相同的 `*` 模式中提取子串；每个星号按从左到右的最短可行匹配绑定。
fn capturePathWildcards(pattern: &str, candidate: &str) -> Option<Vec<String>> {
    if !pattern.contains('*') {
        return Some(Vec::new());
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();
    let starIndexes = pattern
        .iter()
        .enumerate()
        .filter_map(|(index, character)| (*character == '*').then_some(index))
        .collect::<Vec<_>>();
    let mut captures = Vec::with_capacity(starIndexes.len());
    let mut candidateIndex = 0_usize;
    let mut patternStart = 0_usize;

    for (starPosition, patternStar) in starIndexes.iter().enumerate() {
        let fixedEnd = *patternStar;
        let fixed = &pattern[patternStart..fixedEnd];
        let fixedIndex = if starPosition == 0 {
            matchesAt(&candidate, candidateIndex, fixed).then_some(candidateIndex)?
        } else {
            findSegment(&candidate, candidateIndex, fixed)?
        };
        if starPosition > 0 {
            captures.push(candidate[candidateIndex..fixedIndex].iter().collect());
        }
        candidateIndex = fixedIndex + fixed.len();
        patternStart = fixedEnd + 1;
    }

    let suffix = &pattern[patternStart..];
    let suffixStart = if suffix.is_empty() {
        candidate.len()
    } else {
        findSuffix(&candidate, candidateIndex, suffix)?
    };
    captures.push(candidate[candidateIndex..suffixStart].iter().collect());
    (suffixStart + suffix.len() == candidate.len()).then_some(captures)
}

/// 在固定候选偏移匹配一个仅含普通字符和 `?` 的片段；`?` 与 Location wildcard 语义一致匹配一个字符。
fn matchesAt(candidate: &[char], start: usize, pattern: &[char]) -> bool {
    candidate
        .get(start..start.saturating_add(pattern.len()))
        .is_some_and(|slice| {
            slice
                .iter()
                .zip(pattern)
                .all(|(value, expected)| *expected == '?' || value == expected)
        })
}

/// 查找片段最早可行位置，为后续星号保留最长剩余空间并使替换结果可预测。
fn findSegment(candidate: &[char], start: usize, pattern: &[char]) -> Option<usize> {
    (start..=candidate.len().saturating_sub(pattern.len()))
        .find(|index| matchesAt(candidate, *index, pattern))
}

/// 仅接受贴合候选结尾的尾部片段，保证最后一个 `*` 捕获的是完整未映射后缀。
fn findSuffix(candidate: &[char], start: usize, suffix: &[char]) -> Option<usize> {
    let suffixStart = candidate.len().checked_sub(suffix.len())?;
    (suffixStart >= start && matchesAt(candidate, suffixStart, suffix)).then_some(suffixStart)
}

/// 在不泄露配置路径的前提下构建受控响应头；用户 Content-Type 可覆盖嗅探值，显式 override 优先级最高。
fn buildResponseHeaders(
    rule: &MapLocalRule,
    contentType: Option<&str>,
    bodyLength: usize,
) -> Result<HeaderMap, MapToolError> {
    let mut headers = HeaderMap::new();
    let mut configuredContentType = None;
    for header in &rule.responseHeaders {
        let headerName = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| MapToolError::InvalidHeaderName)?;
        let headerValue =
            HeaderValue::from_str(&header.value).map_err(|_| MapToolError::InvalidHeaderValue)?;
        if headerName == CONTENT_TYPE {
            configuredContentType = Some(headerValue);
        } else {
            headers.append(headerName, headerValue);
        }
    }
    if let Some(contentType) = contentType {
        let headerValue = if rule.contentTypeOverride.is_empty() {
            configuredContentType.unwrap_or_else(|| {
                HeaderValue::from_str(contentType)
                    .expect("MIME 嗅探结果必须满足 HTTP HeaderValue 语法")
            })
        } else {
            HeaderValue::from_str(&rule.contentTypeOverride)
                .map_err(|_| MapToolError::InvalidHeaderValue)?
        };
        headers.insert(CONTENT_TYPE, headerValue);
    }
    let contentLength = HeaderValue::from_str(&bodyLength.to_string())
        .expect("usize 十进制长度必须满足 HTTP HeaderValue 语法");
    headers.insert(CONTENT_LENGTH, contentLength);
    Ok(headers)
}

/// 推断本地文件的 MIME；仅当规则未提供显式 Content-Type 覆盖时使用确定性文件扩展名推断。
fn configuredContentType(rule: &MapLocalRule, path: Option<&Path>) -> Result<String, MapToolError> {
    if !rule.contentTypeOverride.is_empty() {
        HeaderValue::from_str(&rule.contentTypeOverride)
            .map_err(|_| MapToolError::InvalidHeaderValue)?;
        return Ok(rule.contentTypeOverride.clone());
    }
    if let Some(header) = rule
        .responseHeaders
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()))
    {
        HeaderValue::from_str(&header.value).map_err(|_| MapToolError::InvalidHeaderValue)?;
        return Ok(header.value.clone());
    }
    Ok(path
        .and_then(|path| from_path(path).first_raw())
        .unwrap_or("application/octet-stream")
        .to_owned())
}

/// 将 URL path 解码为安全相对路径；拒绝编码斜杠、反斜杠、驱动器前缀和试图越出根目录的 `..`。
fn decodeRelativeRequestPath(path: &str) -> Result<PathBuf, ()> {
    let mut segments = Vec::new();
    for rawSegment in path.split('/') {
        let segment = percentDecodeSegment(rawSegment)?;
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            if segments.pop().is_none() {
                return Err(());
            }
            continue;
        }
        if segment.contains(['/', '\\', '\0', ':']) || Path::new(&segment).is_absolute() {
            return Err(());
        }
        segments.push(segment);
    }
    let mut relativePath = PathBuf::new();
    for segment in segments {
        relativePath.push(segment);
    }
    Ok(relativePath)
}

/// 只解码有效 `%XX` 转义并要求 UTF-8；不完整转义和不可表示文本一律按路径穿越风险拒绝。
fn percentDecodeSegment(segment: &str) -> Result<String, ()> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = *bytes.get(index + 1).ok_or(())?;
        let low = *bytes.get(index + 2).ok_or(())?;
        let high = hexValue(high).ok_or(())?;
        let low = hexValue(low).ok_or(())?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| ())
}

/// 将单个 ASCII 十六进制字符转换为半字节；非十六进制输入不进行宽松容错。
const fn hexValue(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

/// 规范化用户配置的目标主机；IPv6 去除可选方括号，DNS 主机统一小写以保持 Host/SNI 一致。
fn normalizeTargetHost(host: &str) -> String {
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    if host.parse::<std::net::IpAddr>().is_ok() {
        host.to_owned()
    } else {
        host.to_ascii_lowercase()
    }
}

/// 为 authority/Host 组装 IPv6 方括号，普通 DNS 和 IPv4 不额外变形。
fn bracketedHost(host: &str) -> String {
    let normalized = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    if normalized.contains(':') {
        format!("[{normalized}]")
    } else {
        normalized.to_owned()
    }
}

/// 依据协议默认端口生成上游 Host 值；未定义默认端口的协议保留端口以避免静默歧义。
fn hostHeaderValue(location: &ResolvedLocation) -> String {
    let host = bracketedHost(&location.host);
    if defaultPort(&location.protocol).is_some_and(|port| port == location.port) {
        host
    } else {
        format!("{host}:{}", location.port)
    }
}

/// 使用 URI builder 生成展示与上游目标，统一处理 query、IPv6 authority 和默认端口。
fn buildLocationUri(location: &ResolvedLocation) -> Result<Uri, MapToolError> {
    let path = if location.path.is_empty() {
        "/".to_owned()
    } else {
        location.path.clone()
    };
    let pathAndQuery = if location.query.is_empty() {
        path
    } else {
        format!("{path}?{}", location.query)
    };
    Uri::builder()
        .scheme(location.protocol.as_str())
        .authority(hostHeaderValue(location))
        .path_and_query(pathAndQuery)
        .build()
        .map_err(|_| MapToolError::InvalidMappedUri)
}

/// 返回 HTTP 族协议的默认端口；仅用于展示和 Host 头，Location 始终保留实际端口数值。
fn defaultPort(protocol: &str) -> Option<u16> {
    match protocol.to_ascii_lowercase().as_str() {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

/// 以工具稳定 ID 与规则 ID 生成事务痕迹；原始和映射 Location 由 PipelineContext 分别保留。
fn appliedToolName(toolId: &str, ruleId: &str) -> String {
    format!("{toolId}:{ruleId}")
}

/// 将映射根解析为绝对路径但不要求其当前存在；目录可在规则保存后由用户稍后创建。
fn absolutePath(path: &Path) -> Result<PathBuf, MapToolError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|currentDirectory| currentDirectory.join(path))
        .map_err(|_| MapToolError::MappingRootUnavailable)
}

/// 异步规范化路径；不存在映射到 None，其它 I/O 错误统一为稳定本地读取失败。
async fn canonicalizePath(path: &Path) -> Result<Option<PathBuf>, MapToolError> {
    match tokio::fs::canonicalize(path).await {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(MapToolError::LocalFileReadFailed),
    }
}

/// 异步读取元数据；调用点已区分不存在，权限或设备错误不能伪装成不存在。
async fn metadataForPath(path: &Path) -> Result<std::fs::Metadata, MapToolError> {
    tokio::fs::metadata(path)
        .await
        .map_err(|_| MapToolError::LocalFileReadFailed)
}

/// 判断响应头是否属于连接级协议字段；本地合成响应必须交给 Hyper 重新管理这些字段。
fn isHopByHopHeader(headerName: &HeaderName) -> bool {
    matches!(
        headerName.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-connection"
    )
}
