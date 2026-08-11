//! 将 SOCKS5 RFC1929 凭据认证接入统一 Mod 阶段内核。
//!
//! 本模块只负责认证决策，不参与协议字节读写；口令仅存在于当前调用信封，调用追踪只记录序列化大小，
//! 不保存正文。所有订阅插件按既定顺序执行，首个 `respond` 接受，`reject/close` 拒绝。

use serde_json::json;

use crate::{ActionKind, EventEnvelope, InterceptionMode, PluginHost, Stage, StageContext};

const EXTENSION_API_VERSION: &str = "2.0.0";

/// 保存 SOCKS5 插件认证所需的连接身份与 RFC1929 凭据；密码不得进入日志、事务或控制事件。
pub struct Socks5AuthenticationRequest {
    pub connectionId: String,
    pub clientAddress: String,
    pub username: String,
    pub password: String,
}

/// 表示插件认证链的最终结果；`Unavailable` 用于区分没有订阅者与明确拒绝。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Socks5AuthenticationDecision {
    Accepted { principalId: String },
    Rejected,
    Unavailable,
}

impl PluginHost {
    /// 调用所有订阅 SOCKS5 认证阶段的插件，并把结构化响应转换为不可变主体身份。
    ///
    /// 运行上下文：服务已完成 RFC1929 方法协商和字段解码，但尚未读取 CONNECT/BIND/UDP 命令。
    /// 失败语义：运行时错误、非法响应、空主体或 `continue` 链末尾均拒绝认证；没有订阅者返回 `Unavailable`。
    pub async fn authenticateSocks5(
        &self,
        request: Socks5AuthenticationRequest,
    ) -> Socks5AuthenticationDecision {
        if !self
            .inner
            .extensionKernel
            .hasSubscriptions(Stage::Socks5Authentication)
        {
            return Socks5AuthenticationDecision::Unavailable;
        }
        let (serviceGeneration, recordingGeneration) =
            self.inner.extensionKernel.currentGenerations();
        let eventSequence = self
            .inner
            .nextExtensionEventId
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let envelope = EventEnvelope {
            apiVersion: EXTENSION_API_VERSION.to_owned(),
            eventId: format!("{}:{eventSequence}", request.connectionId),
            stage: Stage::Socks5Authentication,
            serviceGeneration,
            recordingGeneration,
            pluginInstanceId: String::new(),
            connectionId: Some(request.connectionId),
            transactionId: None,
            deadlineUnixMs: u64::MAX,
            context: StageContext {
                entry: Some("socks5".to_owned()),
                transport: Some("tcp".to_owned()),
                protocol: Some("socks5".to_owned()),
                address: Some(request.clientAddress),
                interceptionMode: InterceptionMode::Intercept,
                ..StageContext::default()
            },
            payload: json!({
                "username": request.username,
                "password": request.password,
            }),
        };
        let dispatch = match self.inner.extensionKernel.dispatch(envelope).await {
            Ok(dispatch) => dispatch,
            Err(_) => return Socks5AuthenticationDecision::Rejected,
        };
        match dispatch.terminalAction {
            Some(ActionKind::Respond) => dispatch
                .appliedActions
                .last()
                .and_then(|action| action.output.as_ref())
                .unwrap_or(&dispatch.finalPayload)
                .get("principalId")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|principalId| !principalId.is_empty())
                .map(|principalId| Socks5AuthenticationDecision::Accepted {
                    principalId: principalId.to_owned(),
                })
                .unwrap_or(Socks5AuthenticationDecision::Rejected),
            Some(ActionKind::Reject | ActionKind::Close) | None => {
                Socks5AuthenticationDecision::Rejected
            }
            _ => Socks5AuthenticationDecision::Rejected,
        }
    }
}
