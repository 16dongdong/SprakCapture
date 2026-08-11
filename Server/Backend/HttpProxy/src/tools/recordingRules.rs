//! 将录制规则集的 REJECT 裁决接入 HTTP 请求流水线。
//!
//! “不录制”由 Capture 会话在创建事务前处理；本工具只负责在出站前生成 403 合成响应，保证
//! HTTP/HTTPS 请求不会先到达上游。TCP 原始隧道的拒绝由透明连接入口使用同一运行快照执行。

use async_trait::async_trait;
use bytes::Bytes;
use capture_core::{
    BeginTransaction, RecordingRuleAction, RecordingRuleRuntime, TransactionProtocol,
    currentTimeMilliseconds,
};
use http::{
    HeaderValue, StatusCode,
    header::{CONTENT_LENGTH, CONTENT_TYPE},
};

use crate::pipeline::{
    PipelineContext, PipelineDirective, PipelineError, PipelineTool, SyntheticResponse, ToolId,
    ToolPhase, ToolRegistration,
};

const rejectedBody: &str = "REJECT";

/// 在 HTTP 请求阶段读取共享录制规则快照；实例无独立配置，避免控制面与录制会话发生漂移。
pub struct RecordingRulesTool {
    runtime: RecordingRuleRuntime,
}

impl RecordingRulesTool {
    /// 绑定录制会话持有的规则运行时；构造本身没有失败分支。
    pub fn new(runtime: RecordingRuleRuntime) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl PipelineTool for RecordingRulesTool {
    /// 规则总开关决定是否进入请求槽；关闭时流水线完全跳过该工具。
    fn registration(&self) -> ToolRegistration {
        ToolRegistration::new(
            ToolId::RecordingRules,
            vec![ToolPhase::Request],
            self.runtime.configuration().enabled,
        )
    }

    /// 在任何正文读取或上游连接前执行 REJECT；Record 与 DoNotRecord 均继续转发。
    async fn onRequest(
        &self,
        context: &mut PipelineContext,
    ) -> Result<PipelineDirective, PipelineError> {
        let input = BeginTransaction {
            protocol: match context.location.protocol.as_str() {
                "https" => TransactionProtocol::Https,
                "ws" => TransactionProtocol::Ws,
                "wss" => TransactionProtocol::Wss,
                _ => TransactionProtocol::Http,
            },
            method: context.request.method.as_str().to_owned(),
            location: context.location.clone(),
            clientAddress: context.clientAddress.clone(),
            clientProcessName: context.clientProcessName.clone(),
            clientProcessId: context.clientProcessId,
            contentType: String::new(),
            startAtMilliseconds: currentTimeMilliseconds(),
        };
        if self.runtime.decision(&input) != RecordingRuleAction::Reject {
            return Ok(PipelineDirective::Continue);
        }
        let body = Bytes::from_static(rejectedBody.as_bytes());
        let mut response = SyntheticResponse::new(StatusCode::FORBIDDEN, body);
        response.headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        response
            .headers
            .insert(CONTENT_LENGTH, HeaderValue::from_static("6"));
        Ok(PipelineDirective::Blocked(response))
    }
}
