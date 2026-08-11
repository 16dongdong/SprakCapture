use std::sync::Arc;

use async_trait::async_trait;
use http::HeaderMap;
use location_core::{LocationPattern, ResolvedLocation};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::{
    PipelineContext, PipelineDirective, PipelineTool, ToolId, ToolPhase, ToolRegistration,
    tools::{
        HeaderMutation, ToolError,
        locationScope::{matchesLocations, validateLocations},
        pipelineError,
    },
};

/// 定义 Block Cookies 的可持久化配置；空 Location 列表表示对全部位置剥离指定方向的 Cookie 字段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct BlockCookiesConfiguration {
    pub enabled: bool,
    pub locations: Vec<LocationPattern>,
    pub stripRequestCookie: bool,
    pub stripResponseSetCookie: bool,
}

impl Default for BlockCookiesConfiguration {
    /// 默认关闭工具；启用后默认同时阻止请求 Cookie 和响应 Set-Cookie，符合完整阻止语义。
    fn default() -> Self {
        Self {
            enabled: false,
            locations: Vec::new(),
            stripRequestCookie: true,
            stripResponseSetCookie: true,
        }
    }
}

impl BlockCookiesConfiguration {
    /// 校验 Location 规则，避免配置保存成功后才在转发热路径暴露格式错误。
    pub fn validate(&self) -> Result<(), ToolError> {
        validateLocations(&self.locations)
    }
}

/// 保存可热更新的 Cookie 剥离配置；头部修改在调用方持有的单个请求或响应草稿上原地完成。
#[derive(Clone, Default)]
pub struct BlockCookiesTool {
    configuration: Arc<RwLock<BlockCookiesConfiguration>>,
}

impl BlockCookiesTool {
    /// 使用已校验配置创建工具实例，配置错误时不会产生可注册的工具对象。
    pub fn new(configuration: BlockCookiesConfiguration) -> Result<Self, ToolError> {
        configuration.validate()?;
        Ok(Self {
            configuration: Arc::new(RwLock::new(configuration)),
        })
    }

    /// 返回当前配置的独立副本，读取者不需要接触并发同步实现。
    pub fn configuration(&self) -> BlockCookiesConfiguration {
        self.configuration.read().clone()
    }

    /// 原子替换运行时配置；校验失败时继续保留旧规则，避免热更新中断代理行为。
    pub fn replaceConfiguration(
        &self,
        configuration: BlockCookiesConfiguration,
    ) -> Result<(), ToolError> {
        configuration.validate()?;
        *self.configuration.write() = configuration;
        Ok(())
    }

    /// 在出站请求前删除所有 Cookie 字段；HTTP 字段名比较由 HeaderMap 保证大小写不敏感。
    pub fn onRequest(
        &self,
        location: &ResolvedLocation,
        headers: &mut HeaderMap,
    ) -> Result<HeaderMutation, ToolError> {
        let configuration = self.configuration();
        if !configuration.enabled || !matchesLocations(&configuration.locations, location)? {
            return Ok(HeaderMutation::default());
        }
        Ok(HeaderMutation {
            matched: true,
            changed: configuration.stripRequestCookie && headers.remove("cookie").is_some(),
        })
    }

    /// 在响应返回客户端前删除所有重复 Set-Cookie 字段，避免浏览器接收任一上游会话写入。
    pub fn onResponse(
        &self,
        location: &ResolvedLocation,
        headers: &mut HeaderMap,
    ) -> Result<HeaderMutation, ToolError> {
        let configuration = self.configuration();
        if !configuration.enabled || !matchesLocations(&configuration.locations, location)? {
            return Ok(HeaderMutation::default());
        }
        Ok(HeaderMutation {
            matched: true,
            changed: configuration.stripResponseSetCookie && headers.remove("set-cookie").is_some(),
        })
    }
}

#[async_trait]
impl PipelineTool for BlockCookiesTool {
    /// 读取当前 enabled 配置并声明双向钩子，确保请求 Cookie 与响应 Set-Cookie 独立处理。
    fn registration(&self) -> ToolRegistration {
        ToolRegistration::new(
            ToolId::BlockCookies,
            vec![ToolPhase::Request, ToolPhase::Response],
            self.configuration().enabled,
        )
    }

    /// 剥离请求 Cookie；仅在工具启用且 Location 命中时写入 appliedTools。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, crate::PipelineError> {
        let mutation =
            BlockCookiesTool::onRequest(self, &context.location, &mut context.request.headers)
                .map_err(|error| pipelineError(ToolId::BlockCookies, error))?;
        Ok(if mutation.matched {
            PipelineDirective::Applied
        } else {
            PipelineDirective::Continue
        })
    }

    /// 剥离响应 Set-Cookie；无响应草稿时不改写任何状态，保持流水线异常路径可预测。
    async fn onResponse(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, crate::PipelineError> {
        let Some(response) = context.response.as_mut() else {
            return Ok(PipelineDirective::Continue);
        };
        let mutation = BlockCookiesTool::onResponse(self, &context.location, &mut response.headers)
            .map_err(|error| pipelineError(ToolId::BlockCookies, error))?;
        Ok(if mutation.matched {
            PipelineDirective::Applied
        } else {
            PipelineDirective::Continue
        })
    }
}
