use async_trait::async_trait;

use crate::{
    PipelineContext, PipelineDirective, PipelineError, PipelineTool, SyntheticResponse, ToolId,
    ToolPhase, ToolRegistration,
};

pub use super::mapSupport::{
    MapLocalConfiguration, MapLocalResolution, MapLocalResponse, MapLocalResponseSource,
    MapLocalRule, MapLocalTool, MapResponseHeader,
};

/// 将 Map Local 的文件解析结果接入请求流水线；任何命中结果都短路出站，但仍进入统一响应钩子和录制。
#[async_trait]
impl PipelineTool for MapLocalTool {
    /// 返回当前启用状态；每次调用 `resolveLocal` 读取完整热更新配置，注册快照只决定是否进入槽位。
    fn registration(&self) -> ToolRegistration {
        ToolRegistration::new(
            ToolId::MapLocal,
            vec![ToolPhase::Request],
            self.configuration().enabled,
        )
    }

    /// 命中文件、缺失文件、目录越界或正文上限时均构造 SyntheticResponse，确保代理不会建立远端连接。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let resolution = self
            .resolveLocal(&context.location)
            .await
            .map_err(mapLocalPipelineError)?;
        let Some(response) = resolution.syntheticResponse() else {
            return Ok(PipelineDirective::Continue);
        };
        context.flags.mappedLocal = true;
        appendMappingTrace(context, response.appliedTool.clone());
        let mut synthetic = SyntheticResponse::new(response.status, response.body.clone());
        synthetic.headers = response.headers.clone();
        Ok(PipelineDirective::ShortCircuit(synthetic))
    }
}

/// 将本地路径解析和读取错误映射为稳定流水线错误，不向数据面泄露本机目录或操作系统报错。
fn mapLocalPipelineError(error: super::mapSupport::MapToolError) -> PipelineError {
    PipelineError::ToolFailed {
        toolId: ToolId::MapLocal,
        code: error.code().to_owned(),
    }
}

/// 记录规则级映射痕迹；ToolPipeline 会额外追加稳定通用 `mapLocal` 名称，便于列表过滤与详情比对。
fn appendMappingTrace(context: &mut PipelineContext, trace: String) {
    if !context.appliedTools.iter().any(|value| value == &trace) {
        context.appliedTools.push(trace);
    }
}
