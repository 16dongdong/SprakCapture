use async_trait::async_trait;
use http::header::HOST;

use crate::{
    PipelineContext, PipelineDirective, PipelineError, PipelineTool, ToolId, ToolPhase,
    ToolRegistration,
};

pub use super::mapSupport::{
    MapRemoteApplication, MapRemoteConfiguration, MapRemoteRule, MapRemoteTarget, MapRemoteTool,
};

/// 将 Map Remote 的纯规则引擎接入请求流水线；命中时同步更新 Location、绝对 URI 和 Host 头。
#[async_trait]
impl PipelineTool for MapRemoteTool {
    /// 返回当前启用状态；规则内容由工具内部读锁在实际匹配时获取，避免注册快照长期陈旧。
    fn registration(&self) -> ToolRegistration {
        ToolRegistration::new(
            ToolId::MapRemote,
            vec![ToolPhase::Request],
            self.configuration().enabled,
        )
    }

    /// 改写后续出站使用的目标，但始终保留 `originalLocation` 供规则匹配和工具痕迹比对；事务摘要记录最终目标。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let Some(application) = self
            .applyRemote(&context.location)
            .map_err(mapRemotePipelineError)?
        else {
            return Ok(PipelineDirective::Continue);
        };
        let upstreamUri = application.upstreamUri().map_err(mapRemotePipelineError)?;
        let hostHeader = application.hostHeader().map_err(mapRemotePipelineError)?;
        context.location = application.mappedLocation;
        context.request.uri = upstreamUri;
        context.request.headers.insert(HOST, hostHeader);
        context.flags.mappedRemote = true;
        appendMappingTrace(context, application.appliedTool);
        Ok(PipelineDirective::Applied)
    }
}

/// 将工具模块错误映射为带稳定槽位和机器码的流水线错误，控制面不需要解析 Display 文本。
fn mapRemotePipelineError(error: super::mapSupport::MapToolError) -> PipelineError {
    PipelineError::ToolFailed {
        toolId: ToolId::MapRemote,
        code: error.code().to_owned(),
    }
}

/// 记录规则级映射痕迹；流水线随后还会写入通用 `mapRemote` 工具标识，二者共同保留规则和阶段信息。
fn appendMappingTrace(context: &mut PipelineContext, trace: String) {
    if !context.appliedTools.iter().any(|value| value == &trace) {
        context.appliedTools.push(trace);
    }
}
