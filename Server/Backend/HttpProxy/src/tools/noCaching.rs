use std::sync::Arc;

use async_trait::async_trait;
use http::{HeaderMap, HeaderName, HeaderValue};
use location_core::{LocationPattern, ResolvedLocation};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::{
    PipelineContext, PipelineDirective, PipelineTool, ToolId, ToolPhase, ToolRegistration,
    tools::{
        ToolError,
        locationScope::{matchesLocations, validateLocations},
        pipelineError,
    },
};

const requestStripHeaders: [&str; 4] = [
    "if-modified-since",
    "if-none-match",
    "cache-control",
    "pragma",
];
const responseStripHeaders: [&str; 6] = [
    "expires",
    "cache-control",
    "pragma",
    "etag",
    "last-modified",
    "age",
];
const requestCacheControlValue: &str = "no-cache";
const requestPragmaValue: &str = "no-cache";
const responseCacheControlValue: &str = "no-cache, no-store, must-revalidate";
const responsePragmaValue: &str = "no-cache";
const responseExpiresValue: &str = "0";

/// 定义 No Caching 的可持久化配置；空 Location 列表表示对所有已解析 HTTP 消息生效。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct NoCachingConfiguration {
    pub enabled: bool,
    pub locations: Vec<LocationPattern>,
    pub stripRequestHeaders: bool,
    pub stripResponseHeaders: bool,
    pub injectRequestNoCache: bool,
    pub injectResponseNoStore: bool,
}

impl Default for NoCachingConfiguration {
    /// 默认关闭工具；启用后默认同时清理条件缓存字段并写入禁止缓存指令。
    fn default() -> Self {
        Self {
            enabled: false,
            locations: Vec::new(),
            stripRequestHeaders: true,
            stripResponseHeaders: true,
            injectRequestNoCache: true,
            injectResponseNoStore: true,
        }
    }
}

impl NoCachingConfiguration {
    /// 校验全部 Location，确保热更新完成后请求路径只执行确定的匹配语义。
    pub fn validate(&self) -> Result<(), ToolError> {
        validateLocations(&self.locations)
    }
}

/// 描述一次头部处理是否命中作用域以及是否改变实际线上字段；流水线仅在命中时写入 appliedTools。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderMutation {
    pub matched: bool,
    pub changed: bool,
}

/// 保存可热更新的 No Caching 配置；读写锁边界仅覆盖配置快照，绝不覆盖网络正文传输。
#[derive(Clone, Default)]
pub struct NoCachingTool {
    configuration: Arc<RwLock<NoCachingConfiguration>>,
}

impl NoCachingTool {
    /// 使用已校验配置创建工具实例，错误配置不会进入可执行数据面。
    pub fn new(configuration: NoCachingConfiguration) -> Result<Self, ToolError> {
        configuration.validate()?;
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
        })
    }

    /// 返回当前配置快照，控制面可据此生成完整且一致的资源表示。
    pub fn configuration(&self) -> NoCachingConfiguration {
        self.configuration.read().clone()
    }

    /// 先校验后替换配置，运行中的请求只会观察到更新前或更新后的完整快照。
    pub fn replaceConfiguration(
        &self,
        configuration: NoCachingConfiguration,
    ) -> Result<(), ToolError> {
        configuration.validate()?;
        *self.configuration.write() = configuration;
        Ok(())
    }

    /// 在请求转发前按配置移除验证器并可强制写入 no-cache；不读取或修改请求正文。
    pub fn onRequest(
        &self,
        location: &ResolvedLocation,
        headers: &mut HeaderMap,
    ) -> Result<HeaderMutation, ToolError> {
        let configuration = self.configuration();
        if !configuration.enabled || !matchesLocations(&configuration.locations, location)? {
            return Ok(HeaderMutation::default());
        }
        let mut changed = false;
        if configuration.stripRequestHeaders {
            changed |= removeHeaders(headers, &requestStripHeaders);
        }
        if configuration.injectRequestNoCache {
            changed |= replaceHeader(headers, "cache-control", requestCacheControlValue);
            changed |= replaceHeader(headers, "pragma", requestPragmaValue);
        }
        Ok(HeaderMutation {
            matched: true,
            changed,
        })
    }

    /// 在响应回传前按配置移除缓存元数据并可写入 no-store；不读取或修改响应正文。
    pub fn onResponse(
        &self,
        location: &ResolvedLocation,
        headers: &mut HeaderMap,
    ) -> Result<HeaderMutation, ToolError> {
        let configuration = self.configuration();
        if !configuration.enabled || !matchesLocations(&configuration.locations, location)? {
            return Ok(HeaderMutation::default());
        }
        let mut changed = false;
        if configuration.stripResponseHeaders {
            changed |= removeHeaders(headers, &responseStripHeaders);
        }
        if configuration.injectResponseNoStore {
            changed |= replaceHeader(headers, "cache-control", responseCacheControlValue);
            changed |= replaceHeader(headers, "pragma", responsePragmaValue);
            changed |= replaceHeader(headers, "expires", responseExpiresValue);
        }
        Ok(HeaderMutation {
            matched: true,
            changed,
        })
    }
}

#[async_trait]
impl PipelineTool for NoCachingTool {
    /// 读取当前 enabled 配置并声明双向钩子，响应阶段用于注入 no-store 响应头。
    fn registration(&self) -> ToolRegistration {
        ToolRegistration::new(
            ToolId::NoCaching,
            vec![ToolPhase::Request, ToolPhase::Response],
            self.configuration().enabled,
        )
    }

    /// 修改请求头草稿；仅当启用配置命中 Location 时返回 Applied，以便流水线写入工具痕迹。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, crate::PipelineError> {
        let mutation =
            NoCachingTool::onRequest(self, &context.location, &mut context.request.headers)
                .map_err(|error| pipelineError(ToolId::NoCaching, error))?;
        Ok(if mutation.matched {
            PipelineDirective::Applied
        } else {
            PipelineDirective::Continue
        })
    }

    /// 修改响应头草稿；尚未产生响应时直接跳过，避免响应钩子在异常路径制造合成响应。
    async fn onResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, crate::PipelineError> {
        let Some(response) = context.response.as_mut() else {
            return Ok(PipelineDirective::Continue);
        };
        let mutation = NoCachingTool::onResponse(self, &context.location, &mut response.headers)
            .map_err(|error| pipelineError(ToolId::NoCaching, error))?;
        Ok(if mutation.matched {
            PipelineDirective::Applied
        } else {
            PipelineDirective::Continue
        })
    }
}

/// 移除指定名称的全部重复字段；HeaderMap 名称匹配遵从 HTTP 的大小写不敏感语义。
fn removeHeaders(headers: &mut HeaderMap, names: &[&str]) -> bool {
    let mut changed = false;
    for name in names {
        changed |= headers.remove(*name).is_some();
    }
    changed
}

/// 仅在当前字段不等于唯一目标值时替换，既消除多值缓存字段又避免无意义的重复写入。
fn replaceHeader(headers: &mut HeaderMap, name: &str, value: &str) -> bool {
    let headerName = HeaderName::from_bytes(name.as_bytes()).expect("固定 HTTP 字段名必须有效");
    let headerValue = HeaderValue::from_str(value).expect("固定 HTTP 字段值必须有效");
    if hasSingleValue(headers, &headerName, &headerValue) {
        return false;
    }
    headers.insert(headerName, headerValue);
    true
}

/// 比较字段的值数量与字节内容，避免 HeaderMap 在保留重复字段时被误判为已规范化。
fn hasSingleValue(headers: &HeaderMap, name: &HeaderName, expected: &HeaderValue) -> bool {
    let mut values = headers.get_all(name).iter();
    matches!(values.next(), Some(actual) if actual == expected) && values.next().is_none()
}
