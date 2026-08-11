use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, StatusCode, Uri,
    header::{HOST, HeaderName},
};
use location_core::{LocationPattern, validateLocationPattern};
use parking_lot::RwLock;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    PipelineContext, PipelineDirective, PipelineError, PipelineTool, ToolId, ToolPhase,
    ToolRegistration, target::parsePipelineTarget,
};

use super::{
    locationScope::matchesLocations,
    messageDraft::{isTextBody, normalizeModifiedBodyHeaders},
};

const maximumRewriteSets: usize = 256;
const maximumRewriteRules: usize = 2_000;
const maximumIdentifierLength: usize = 128;
const maximumSetNameLength: usize = 256;
const maximumRegexLength: usize = 4_096;
const maximumReplacementLength: usize = 64 * 1024;
const maximumHeaderNameLength: usize = 256;

/// 描述 Rewrite 规则能够处理的固定 HTTP 消息字段，方向由类型本身确定，避免配置出现不可解释的双向分支。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RewriteRuleType {
    UrlHost,
    UrlPath,
    UrlQuery,
    RequestHeader,
    ResponseHeader,
    RequestBody,
    ResponseBody,
    ResponseStatus,
}

/// 描述头部规则对同名字段执行的确定性动作；新增只追加一个值，修改和删除仅处理正则命中的既有值。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HeaderAction {
    Add,
    Modify,
    Remove,
}

/// 描述一条可热更新的 Rewrite 规则；所有字符串上限与控制 API 的 JSON 协议保持一致。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RewriteRule {
    pub id: String,
    pub enabled: bool,
    pub r#type: RewriteRuleType,
    pub matchRegex: String,
    pub replace: String,
    pub headerName: Option<String>,
    pub matchValueRegex: Option<String>,
    pub headerAction: Option<HeaderAction>,
    pub caseSensitive: bool,
    pub matchAllOccurrences: bool,
}

/// 描述按 Location 作用域划分的有序 Rewrite 集；空 Location 列表表示全局作用域。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RewriteSet {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub locations: Vec<LocationPattern>,
    pub rules: Vec<RewriteRule>,
}

/// 描述 Rewrite 的完整可替换配置；配置更新先编译全部正则，再原子替换运行快照。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RewriteConfiguration {
    pub enabled: bool,
    pub sets: Vec<RewriteSet>,
}

/// 描述 Rewrite 配置或运行期动态替换的稳定失败原因；错误值不包含规则内容或报文字节。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RewriteError {
    #[error("error.rewrite.tooManySets")]
    TooManySets,
    #[error("error.rewrite.tooManyRules")]
    TooManyRules,
    #[error("error.rewrite.invalidSetId")]
    InvalidSetId,
    #[error("error.rewrite.duplicateSetId")]
    DuplicateSetId,
    #[error("error.rewrite.invalidSetName")]
    InvalidSetName,
    #[error("error.rewrite.invalidRuleId")]
    InvalidRuleId,
    #[error("error.rewrite.duplicateRuleId")]
    DuplicateRuleId,
    #[error("error.rewrite.invalidLocation")]
    InvalidLocation,
    #[error("error.rewrite.regexTooLong")]
    RegexTooLong,
    #[error("error.rewrite.invalidRegex")]
    InvalidRegex,
    #[error("error.rewrite.replacementTooLong")]
    ReplacementTooLong,
    #[error("error.rewrite.invalidHeaderName")]
    InvalidHeaderName,
    #[error("error.rewrite.missingHeaderAction")]
    MissingHeaderAction,
    #[error("error.rewrite.invalidTarget")]
    InvalidTarget,
    #[error("error.rewrite.invalidHeaderValue")]
    InvalidHeaderValue,
    #[error("error.rewrite.invalidStatus")]
    InvalidStatus,
}

impl RewriteError {
    /// 返回跨控制 API、MCP 与流水线共享的机器码，调用方据此映射本地化文案且不泄露规则内容。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooManySets => "rewriteTooManySets",
            Self::TooManyRules => "rewriteTooManyRules",
            Self::InvalidSetId => "rewriteInvalidSetId",
            Self::DuplicateSetId => "rewriteDuplicateSetId",
            Self::InvalidSetName => "rewriteInvalidSetName",
            Self::InvalidRuleId => "rewriteInvalidRuleId",
            Self::DuplicateRuleId => "rewriteDuplicateRuleId",
            Self::InvalidLocation => "rewriteInvalidLocation",
            Self::RegexTooLong => "rewriteRegexTooLong",
            Self::InvalidRegex => "rewriteInvalidRegex",
            Self::ReplacementTooLong => "rewriteReplacementTooLong",
            Self::InvalidHeaderName => "rewriteInvalidHeaderName",
            Self::MissingHeaderAction => "rewriteMissingHeaderAction",
            Self::InvalidTarget => "rewriteInvalidTarget",
            Self::InvalidHeaderValue => "rewriteInvalidHeaderValue",
            Self::InvalidStatus => "rewriteInvalidStatus",
        }
    }

    /// 返回语言包使用的稳定消息键；工具层始终不拼接用户可见的运行期错误文本。
    pub const fn messageKey(&self) -> &'static str {
        match self {
            Self::TooManySets => "error.rewrite.tooManySets",
            Self::TooManyRules => "error.rewrite.tooManyRules",
            Self::InvalidSetId => "error.rewrite.invalidSetId",
            Self::DuplicateSetId => "error.rewrite.duplicateSetId",
            Self::InvalidSetName => "error.rewrite.invalidSetName",
            Self::InvalidRuleId => "error.rewrite.invalidRuleId",
            Self::DuplicateRuleId => "error.rewrite.duplicateRuleId",
            Self::InvalidLocation => "error.rewrite.invalidLocation",
            Self::RegexTooLong => "error.rewrite.regexTooLong",
            Self::InvalidRegex => "error.rewrite.invalidRegex",
            Self::ReplacementTooLong => "error.rewriteReplacementTooLong",
            Self::InvalidHeaderName => "error.rewrite.invalidHeaderName",
            Self::MissingHeaderAction => "error.rewrite.missingHeaderAction",
            Self::InvalidTarget => "error.rewrite.invalidTarget",
            Self::InvalidHeaderValue => "error.rewrite.invalidHeaderValue",
            Self::InvalidStatus => "error.rewrite.invalidStatus",
        }
    }
}

/// 保存已通过完整校验且已预编译的运行期规则，避免数据面为每条消息重复编译正则。
#[derive(Clone)]
struct CompiledRewriteConfiguration {
    configuration: RewriteConfiguration,
    sets: Vec<CompiledRewriteSet>,
}

/// 保存单个重写集的 Location 与规则快照；公共模型与运行对象分离以避免暴露正则实现细节。
#[derive(Clone)]
struct CompiledRewriteSet {
    locations: Vec<LocationPattern>,
    enabled: bool,
    rules: Vec<CompiledRewriteRule>,
}

/// 保存一条规则的原始配置和预编译正则；可选值正则仅由头部修改/删除使用。
#[derive(Clone)]
struct CompiledRewriteRule {
    rule: RewriteRule,
    matchRegex: Regex,
    matchValueRegex: Option<Regex>,
    headerName: Option<HeaderName>,
}

/// 提供按请求与响应阶段执行的 Rewrite 工具；读锁只用于复制编译快照，不跨越异步调用或网络 I/O。
#[derive(Clone)]
pub struct RewriteTool {
    state: Arc<RwLock<CompiledRewriteConfiguration>>,
}

impl RewriteTool {
    /// 使用完整校验并预编译的配置创建工具，任一规则无效时不产生可注册的工具实例。
    pub fn new(configuration: RewriteConfiguration) -> Result<Self, RewriteError> {
        Ok(Self {
            state: Arc::new(RwLock::new(compileConfiguration(configuration)?)),
        })
    }

    /// 返回当前可序列化的配置快照，供控制层读取而不暴露内部预编译状态。
    pub fn configuration(&self) -> RewriteConfiguration {
        self.state.read().configuration.clone()
    }

    /// 先验证并编译完整配置，再一次性替换运行快照，防止活动请求观察到半写入规则集。
    pub fn replaceConfiguration(
        &self,
        configuration: RewriteConfiguration,
    ) -> Result<(), RewriteError> {
        let compiled = compileConfiguration(configuration)?;
        *self.state.write() = compiled;
        Ok(())
    }

    /// 校验配置但不改变运行状态，供控制层的显式验证端点复用同一套边界语义。
    pub fn validate(configuration: &RewriteConfiguration) -> Result<(), RewriteError> {
        let _ = compileConfiguration(configuration.clone())?;
        Ok(())
    }

    /// 判断启用配置中是否存在可能读取请求正文的规则，以便代理仅在必要时物化正文。
    fn requiresRequestBody(configuration: &CompiledRewriteConfiguration) -> bool {
        configuration.sets.iter().any(|set| {
            set.enabled
                && set.rules.iter().any(|rule| {
                    rule.rule.enabled && rule.rule.r#type == RewriteRuleType::RequestBody
                })
        })
    }

    /// 判断启用配置中是否存在可能读取响应正文的规则，以便代理仅在必要时物化正文。
    fn requiresResponseBody(configuration: &CompiledRewriteConfiguration) -> bool {
        configuration.sets.iter().any(|set| {
            set.enabled
                && set.rules.iter().any(|rule| {
                    rule.rule.enabled && rule.rule.r#type == RewriteRuleType::ResponseBody
                })
        })
    }

    /// 将运行期错误转换为固定工具槽位错误，保持控制面不依赖 Rust 错误文本。
    fn pipelineError(error: RewriteError) -> PipelineError {
        PipelineError::ToolFailed {
            toolId: ToolId::Rewrite,
            code: error.code().to_owned(),
        }
    }
}

#[async_trait]
impl PipelineTool for RewriteTool {
    /// 返回当前工具开关与正文需求；需求仅由已启用规则决定，关闭时保持 M1 的流式转发路径。
    fn registration(&self) -> ToolRegistration {
        let configuration = self.state.read().clone();
        let mut registration = ToolRegistration::new(
            ToolId::Rewrite,
            vec![ToolPhase::Request, ToolPhase::Response],
            configuration.configuration.enabled,
        );
        if configuration.configuration.enabled && Self::requiresRequestBody(&configuration) {
            registration = registration.withRequestBody();
        }
        if configuration.configuration.enabled && Self::requiresResponseBody(&configuration) {
            registration = registration.withResponseBody();
        }
        registration
    }

    /// 按规则集顺序改写请求 URL、头部或正文；每个集都使用请求进入该集时的最新 Location 进行作用域判定。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let configuration = self.state.read().clone();
        if !configuration.configuration.enabled {
            return Ok(PipelineDirective::Continue);
        }
        let mut changed = false;
        for set in &configuration.sets {
            if !set.enabled
                || !matchesLocations(&set.locations, &context.location)
                    .map_err(|_| Self::pipelineError(RewriteError::InvalidLocation))?
            {
                continue;
            }
            for rule in &set.rules {
                if !rule.rule.enabled {
                    continue;
                }
                changed |= applyRequestRule(rule, context).map_err(Self::pipelineError)?;
            }
        }
        if changed {
            context.flags.rewritten = true;
            Ok(PipelineDirective::Applied)
        } else {
            Ok(PipelineDirective::Continue)
        }
    }

    /// 按规则集顺序改写响应头、正文或状态；无响应草稿时保持通过，避免影响仅请求阶段的执行。
    async fn onResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let configuration = self.state.read().clone();
        if !configuration.configuration.enabled || context.response.is_none() {
            return Ok(PipelineDirective::Continue);
        }
        let mut changed = false;
        for set in &configuration.sets {
            if !set.enabled
                || !matchesLocations(&set.locations, &context.location)
                    .map_err(|_| Self::pipelineError(RewriteError::InvalidLocation))?
            {
                continue;
            }
            let response = context
                .response
                .as_mut()
                .expect("已在响应阶段开始前确认存在响应草稿");
            for rule in &set.rules {
                if !rule.rule.enabled {
                    continue;
                }
                changed |= applyResponseRule(rule, response).map_err(Self::pipelineError)?;
            }
        }
        if changed {
            context.flags.rewritten = true;
            Ok(PipelineDirective::Applied)
        } else {
            Ok(PipelineDirective::Continue)
        }
    }
}

/// 完成 Rewrite 配置的结构、长度、正则和 HTTP 字段校验，并生成数据面使用的不可变编译快照。
fn compileConfiguration(
    configuration: RewriteConfiguration,
) -> Result<CompiledRewriteConfiguration, RewriteError> {
    if configuration.sets.len() > maximumRewriteSets {
        return Err(RewriteError::TooManySets);
    }
    let mut setIds = HashSet::new();
    let mut sets = Vec::with_capacity(configuration.sets.len());
    for set in &configuration.sets {
        validateSet(set, &mut setIds)?;
        let mut ruleIds = HashSet::new();
        let mut rules = Vec::with_capacity(set.rules.len());
        for rule in &set.rules {
            rules.push(compileRule(rule, &mut ruleIds)?);
        }
        sets.push(CompiledRewriteSet {
            locations: set.locations.clone(),
            enabled: set.enabled,
            rules,
        });
    }
    Ok(CompiledRewriteConfiguration {
        configuration,
        sets,
    })
}

/// 校验集合 ID、名称、Location 与规则数量，确保所有运行期失败都能在配置写入前暴露。
fn validateSet(set: &RewriteSet, setIds: &mut HashSet<String>) -> Result<(), RewriteError> {
    if set.id.is_empty() || set.id.len() > maximumIdentifierLength {
        return Err(RewriteError::InvalidSetId);
    }
    if !setIds.insert(set.id.clone()) {
        return Err(RewriteError::DuplicateSetId);
    }
    if set.name.is_empty() || set.name.len() > maximumSetNameLength {
        return Err(RewriteError::InvalidSetName);
    }
    if set.rules.len() > maximumRewriteRules {
        return Err(RewriteError::TooManyRules);
    }
    for location in &set.locations {
        validateLocationPattern(location).map_err(|_| RewriteError::InvalidLocation)?;
    }
    Ok(())
}

/// 校验并编译单条规则；头部规则在写入前解析 HeaderName，避免数据面出现无效字段名。
fn compileRule(
    rule: &RewriteRule,
    ruleIds: &mut HashSet<String>,
) -> Result<CompiledRewriteRule, RewriteError> {
    if rule.id.is_empty() || rule.id.len() > maximumIdentifierLength {
        return Err(RewriteError::InvalidRuleId);
    }
    if !ruleIds.insert(rule.id.clone()) {
        return Err(RewriteError::DuplicateRuleId);
    }
    if rule.matchRegex.len() > maximumRegexLength
        || rule
            .matchValueRegex
            .as_deref()
            .is_some_and(|value| value.len() > maximumRegexLength)
    {
        return Err(RewriteError::RegexTooLong);
    }
    if rule.replace.len() > maximumReplacementLength {
        return Err(RewriteError::ReplacementTooLong);
    }
    let matchRegex = compileRegex(&rule.matchRegex, rule.caseSensitive)?;
    let matchValueRegex = rule
        .matchValueRegex
        .as_deref()
        .map(|value| compileRegex(value, rule.caseSensitive))
        .transpose()?;
    let headerName = match rule.r#type {
        RewriteRuleType::RequestHeader | RewriteRuleType::ResponseHeader => {
            let headerName = rule
                .headerName
                .as_deref()
                .filter(|name| !name.is_empty() && name.len() <= maximumHeaderNameLength)
                .ok_or(RewriteError::InvalidHeaderName)?
                .parse::<HeaderName>()
                .map_err(|_| RewriteError::InvalidHeaderName)?;
            if rule.headerAction.is_none() {
                return Err(RewriteError::MissingHeaderAction);
            }
            Some(headerName)
        }
        _ => None,
    };
    Ok(CompiledRewriteRule {
        rule: rule.clone(),
        matchRegex,
        matchValueRegex,
        headerName,
    })
}

/// 使用配置声明的大小写语义编译正则；正则自身的语法失败在配置保存前转为稳定错误码。
fn compileRegex(expression: &str, caseSensitive: bool) -> Result<Regex, RewriteError> {
    RegexBuilder::new(expression)
        .case_insensitive(!caseSensitive)
        .build()
        .map_err(|_| RewriteError::InvalidRegex)
}

/// 只对请求方向定义的 URL、头部和正文规则执行改写，其他规则类型在本阶段保持严格无副作用。
fn applyRequestRule(
    rule: &CompiledRewriteRule,
    context: &mut PipelineContext,
) -> Result<bool, RewriteError> {
    match rule.rule.r#type {
        RewriteRuleType::UrlHost => rewriteUrlHost(rule, context),
        RewriteRuleType::UrlPath => rewriteUrlPath(rule, context),
        RewriteRuleType::UrlQuery => rewriteUrlQuery(rule, context),
        RewriteRuleType::RequestHeader => rewriteHeaders(rule, &mut context.request.headers),
        RewriteRuleType::RequestBody => rewriteBody(
            rule,
            &mut context.request.headers,
            &mut context.request.body,
        ),
        RewriteRuleType::ResponseHeader
        | RewriteRuleType::ResponseBody
        | RewriteRuleType::ResponseStatus => Ok(false),
    }
}

/// 只对响应方向定义的头部、正文和状态规则执行改写，禁止响应规则回写请求上下文。
fn applyResponseRule(
    rule: &CompiledRewriteRule,
    response: &mut crate::ResponseDraft,
) -> Result<bool, RewriteError> {
    match rule.rule.r#type {
        RewriteRuleType::ResponseHeader => rewriteHeaders(rule, &mut response.headers),
        RewriteRuleType::ResponseBody => {
            rewriteBody(rule, &mut response.headers, &mut response.body)
        }
        RewriteRuleType::ResponseStatus => rewriteStatus(rule, &mut response.status),
        RewriteRuleType::UrlHost
        | RewriteRuleType::UrlPath
        | RewriteRuleType::UrlQuery
        | RewriteRuleType::RequestHeader
        | RewriteRuleType::RequestBody => Ok(false),
    }
}

/// 按规则选择替换首次或全部匹配；未匹配时直接返回原文本以保持无分配的判断边界。
fn replaceText(rule: &CompiledRewriteRule, source: &str) -> String {
    if rule.rule.matchAllOccurrences {
        rule.matchRegex
            .replace_all(source, rule.rule.replace.as_str())
            .into_owned()
    } else {
        rule.matchRegex
            .replace(source, rule.rule.replace.as_str())
            .into_owned()
    }
}

/// 将 URL 主机替换为规则输出，同时保留协议、端口、路径和查询，并同步刷新实际出站目标及 Host 字段。
fn rewriteUrlHost(
    rule: &CompiledRewriteRule,
    context: &mut PipelineContext,
) -> Result<bool, RewriteError> {
    let currentHost = context
        .request
        .uri
        .host()
        .ok_or(RewriteError::InvalidTarget)?;
    let replacedHost = replaceText(rule, currentHost);
    if replacedHost == currentHost {
        return Ok(false);
    }
    let port = context.request.uri.port_u16();
    let authority = authorityWithHost(&replacedHost, port);
    let uri = buildUri(
        context
            .request
            .uri
            .scheme_str()
            .ok_or(RewriteError::InvalidTarget)?,
        &authority,
        context
            .request
            .uri
            .path_and_query()
            .map(|value| value.as_str()),
    )?;
    updateRequestTarget(context, uri)?;
    Ok(true)
}

/// 将 URL 路径替换为规则输出并保留原查询；路径必须继续满足绝对路径约束才能进入转发层。
fn rewriteUrlPath(
    rule: &CompiledRewriteRule,
    context: &mut PipelineContext,
) -> Result<bool, RewriteError> {
    let currentPath = context.request.uri.path();
    let replacedPath = replaceText(rule, currentPath);
    if replacedPath == currentPath {
        return Ok(false);
    }
    if !replacedPath.starts_with('/') || replacedPath.contains('?') {
        return Err(RewriteError::InvalidTarget);
    }
    let pathAndQuery = joinPathAndQuery(&replacedPath, context.request.uri.query());
    let uri = buildUri(
        context
            .request
            .uri
            .scheme_str()
            .ok_or(RewriteError::InvalidTarget)?,
        context
            .request
            .uri
            .authority()
            .ok_or(RewriteError::InvalidTarget)?
            .as_str(),
        Some(&pathAndQuery),
    )?;
    updateRequestTarget(context, uri)?;
    Ok(true)
}

/// 将 URL 查询原文替换为规则输出并保留路径；空查询会被规范为无查询，以避免不必要的尾随问号差异。
fn rewriteUrlQuery(
    rule: &CompiledRewriteRule,
    context: &mut PipelineContext,
) -> Result<bool, RewriteError> {
    let currentQuery = context.request.uri.query().unwrap_or_default();
    let replacedQuery = replaceText(rule, currentQuery);
    if replacedQuery == currentQuery {
        return Ok(false);
    }
    let pathAndQuery = joinPathAndQuery(context.request.uri.path(), nonEmpty(&replacedQuery));
    let uri = buildUri(
        context
            .request
            .uri
            .scheme_str()
            .ok_or(RewriteError::InvalidTarget)?,
        context
            .request
            .uri
            .authority()
            .ok_or(RewriteError::InvalidTarget)?
            .as_str(),
        Some(&pathAndQuery),
    )?;
    updateRequestTarget(context, uri)?;
    Ok(true)
}

/// 对请求 URI 的每次合法改写同步更新 Location 和 Host，保证后续工具与实际出站连接观察同一目标。
fn updateRequestTarget(context: &mut PipelineContext, uri: Uri) -> Result<(), RewriteError> {
    context.request.uri = uri;
    let target = parsePipelineTarget(&context.request).map_err(|_| RewriteError::InvalidTarget)?;
    context.location = target.location;
    context.request.headers.insert(HOST, target.hostHeader);
    Ok(())
}

/// 根据替换后的主机和原端口生成 authority；IPv6 字面量统一补上方括号，避免 URI 端口解析歧义。
fn authorityWithHost(host: &str, port: Option<u16>) -> String {
    let normalizedHost = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    let host = if normalizedHost.contains(':') {
        format!("[{normalizedHost}]")
    } else {
        normalizedHost.to_owned()
    };
    port.map_or(host.clone(), |port| format!("{host}:{port}"))
}

/// 使用已校验的协议、authority 与 path-and-query 构建绝对 URI；失败表示动态替换产生了 HTTP 栈不可发送的目标。
fn buildUri(
    scheme: &str,
    authority: &str,
    pathAndQuery: Option<&str>,
) -> Result<Uri, RewriteError> {
    let pathAndQuery = pathAndQuery.unwrap_or("/");
    Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query(pathAndQuery)
        .build()
        .map_err(|_| RewriteError::InvalidTarget)
}

/// 组合路径与可选查询，确保路径和查询边界由 URI builder 统一验证而不是手工转义。
fn joinPathAndQuery(path: &str, query: Option<&str>) -> String {
    match query {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    }
}

/// 仅将非空字符串作为查询写入 URI；空替换代表移除查询而非保留无意义的问号。
fn nonEmpty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

/// 根据头部动作在 HeaderMap 上执行增、改、删；重建映射以保留重复头部的顺序与独立值。
fn rewriteHeaders(
    rule: &CompiledRewriteRule,
    headers: &mut HeaderMap,
) -> Result<bool, RewriteError> {
    let headerName = rule
        .headerName
        .as_ref()
        .ok_or(RewriteError::InvalidHeaderName)?;
    let action = rule
        .rule
        .headerAction
        .ok_or(RewriteError::MissingHeaderAction)?;
    if action == HeaderAction::Add {
        let value = HeaderValue::from_str(&rule.rule.replace)
            .map_err(|_| RewriteError::InvalidHeaderValue)?;
        headers.append(headerName.clone(), value);
        return Ok(true);
    }
    let valueRegex = rule.matchValueRegex.as_ref().unwrap_or(&rule.matchRegex);
    let mut rewritten = HeaderMap::with_capacity(headers.len());
    let mut changed = false;
    for (name, value) in headers.iter() {
        if name != headerName {
            rewritten.append(name.clone(), value.clone());
            continue;
        }
        let Ok(text) = value.to_str() else {
            rewritten.append(name.clone(), value.clone());
            continue;
        };
        if !valueRegex.is_match(text) {
            rewritten.append(name.clone(), value.clone());
            continue;
        }
        match action {
            HeaderAction::Remove => changed = true,
            HeaderAction::Modify => {
                let replacement = replaceTextWithRegex(valueRegex, &rule.rule, text);
                if replacement == text {
                    rewritten.append(name.clone(), value.clone());
                } else {
                    let replacement = HeaderValue::from_str(&replacement)
                        .map_err(|_| RewriteError::InvalidHeaderValue)?;
                    rewritten.append(name.clone(), replacement);
                    changed = true;
                }
            }
            HeaderAction::Add => unreachable!("新增头部在重建前已完成"),
        }
    }
    if changed {
        *headers = rewritten;
    }
    Ok(changed)
}

/// 对头部值使用其专用匹配正则执行一次或全部替换，保证 matchValueRegex 的实际替换语义与匹配语义一致。
fn replaceTextWithRegex(regex: &Regex, rule: &RewriteRule, source: &str) -> String {
    if rule.matchAllOccurrences {
        regex
            .replace_all(source, rule.replace.as_str())
            .into_owned()
    } else {
        regex.replace(source, rule.replace.as_str()).into_owned()
    }
}

/// 仅在文本媒体类型且正文已物化时改写 UTF-8 内容；二进制、未物化和非法 UTF-8 正文严格保持原样。
fn rewriteBody(
    rule: &CompiledRewriteRule,
    headers: &mut HeaderMap,
    body: &mut Option<Bytes>,
) -> Result<bool, RewriteError> {
    if !isTextBody(headers) {
        return Ok(false);
    }
    let Some(currentBody) = body.as_deref() else {
        return Ok(false);
    };
    let Ok(currentText) = std::str::from_utf8(currentBody) else {
        return Ok(false);
    };
    let replacement = replaceText(rule, currentText);
    if replacement == currentText {
        return Ok(false);
    }
    let replacement = Bytes::from(replacement);
    normalizeModifiedBodyHeaders(headers, replacement.len());
    *body = Some(replacement);
    Ok(true)
}

/// 将状态码文本按规则改写并重新解析为标准 HTTP StatusCode，任何非 100..599 输出都被拒绝而不写入响应草稿。
fn rewriteStatus(
    rule: &CompiledRewriteRule,
    status: &mut StatusCode,
) -> Result<bool, RewriteError> {
    let source = status.as_u16().to_string();
    let replacement = replaceText(rule, &source);
    if replacement == source {
        return Ok(false);
    }
    let code = replacement
        .parse::<u16>()
        .map_err(|_| RewriteError::InvalidStatus)?;
    let statusCode = StatusCode::from_u16(code).map_err(|_| RewriteError::InvalidStatus)?;
    *status = statusCode;
    Ok(true)
}
