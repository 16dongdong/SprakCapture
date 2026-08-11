//! 定义 Native ABI 的 JSON 事件与动作模型，并提供 TCP/UDP 二进制负载适配。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 标识宿主发布的稳定处理阶段；未知的新阶段会在反序列化时明确失败，避免错误修改流量。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    ServiceStarting,
    ServiceStarted,
    ConfigurationChanged,
    ServiceStopping,
    ConnectionAccepted,
    Socks5Authentication,
    ProtocolClassified,
    TargetResolving,
    BeforeConnect,
    Connected,
    ConnectionClosing,
    ClientHelloObserved,
    CertificateSelecting,
    TlsEstablished,
    TlsFailed,
    RequestHeaders,
    RequestBodyChunk,
    RequestComplete,
    BeforeUpstream,
    ResponseHeaders,
    ResponseBodyChunk,
    ResponseComplete,
    WebSocketOpening,
    WebSocketFrame,
    WebSocketClosing,
    TcpChunk,
    UdpDatagram,
    DnsMessage,
    BeforeRecord,
    TransactionUpdated,
    TransactionCompleted,
    RecordingCleared,
    InspectorDataRequested,
    CommandInvoked,
    ContextActionInvoked,
}

/// 保存阶段共享的连接、协议、进程与拦截模式上下文；不适用字段保持为空。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageContext {
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub processId: Option<u32>,
    #[serde(default)]
    pub processName: Option<String>,
    #[serde(default)]
    pub processPath: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub scheme: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub statusCode: Option<u16>,
    #[serde(default)]
    pub mimeType: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default = "defaultInterceptionMode")]
    pub interceptionMode: String,
}

/// 返回协议缺省的可拦截模式；仅用于兼容测试夹具省略字段的情况。
fn defaultInterceptionMode() -> String {
    "intercept".to_owned()
}

/// 保存一次完整阶段事件；事件 ID 必须原样回填到动作中。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    pub apiVersion: String,
    pub eventId: String,
    pub stage: Stage,
    pub serviceGeneration: u64,
    pub recordingGeneration: u64,
    pub pluginInstanceId: String,
    #[serde(default)]
    pub connectionId: Option<String>,
    #[serde(default)]
    pub transactionId: Option<String>,
    pub deadlineUnixMs: u64,
    pub context: StageContext,
    pub payload: Value,
}

/// 保存 Native 宿主传入的模块身份与阶段事件。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invocation {
    pub pluginId: String,
    pub moduleId: String,
    pub moduleKind: String,
    pub envelope: EventEnvelope,
}

/// 标识插件可返回的标准动作；宿主仍会按当前阶段复验合法性。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionKind {
    Continue,
    Modify,
    Hold,
    Drop,
    Reject,
    Respond,
    Redirect,
    Annotate,
    Close,
}

/// 保存插件对当前事件的结构化决定；输出字节使用 JSON 数组与宿主 ABI 保持一致。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub eventId: String,
    pub action: ActionKind,
    #[serde(default)]
    pub patch: Vec<Value>,
    #[serde(default)]
    pub annotations: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

impl Action {
    /// 构造不改变当前事件的动作；事件 ID 直接取自输入，避免作者误填其他调用的标识。
    pub fn continueEvent(invocation: &Invocation) -> Self {
        Self::new(invocation, ActionKind::Continue)
    }

    /// 构造指定种类动作；输出、补丁和注释默认为空，由链式方法按需添加。
    pub fn new(invocation: &Invocation, action: ActionKind) -> Self {
        Self {
            eventId: invocation.envelope.eventId.clone(),
            action,
            patch: Vec::new(),
            annotations: Vec::new(),
            output: None,
        }
    }

    /// 原子替换当前完整 payload；宿主将在阶段校验后应用根路径 JSON Patch。
    pub fn modifyPayload(invocation: &Invocation, payload: Value) -> Self {
        let mut action = Self::new(invocation, ActionKind::Modify);
        action.patch.push(json!({
            "op": "replace",
            "path": "",
            "value": payload,
        }));
        action
    }

    /// 为 TCP/UDP/正文块替换 bytes 并保留 payload 的其他字段；非对象 payload 明确失败。
    pub fn modifyBytes(invocation: &Invocation, bytes: Vec<u8>) -> Result<Self, String> {
        let mut payload = invocation.envelope.payload.clone();
        let Value::Object(fields) = &mut payload else {
            return Err("二进制事件 payload 必须是对象".to_owned());
        };
        fields.insert("bytes".to_owned(), json!(bytes));
        Ok(Self::modifyPayload(invocation, payload))
    }

    /// 暂存当前流式事件等待后续字节；仅应在宿主声明支持 hold 的阶段返回。
    pub fn hold(invocation: &Invocation) -> Self {
        Self::new(invocation, ActionKind::Hold)
    }

    /// 丢弃当前数据块或录制事务；最终语义由宿主当前阶段确定。
    pub fn dropEvent(invocation: &Invocation) -> Self {
        Self::new(invocation, ActionKind::Drop)
    }

    /// 拒绝当前操作并附带非空稳定原因；空白原因会返回作者输入错误。
    pub fn reject(invocation: &Invocation, reason: impl Into<String>) -> Result<Self, String> {
        Self::reasonAction(invocation, ActionKind::Reject, reason)
    }

    /// 请求立即关闭当前连接并附带非空原因；非连接阶段仍由宿主复验拒绝。
    pub fn close(invocation: &Invocation, reason: impl Into<String>) -> Result<Self, String> {
        Self::reasonAction(invocation, ActionKind::Close, reason)
    }

    /// 添加任意结构化注释而不修改线上字节；空集合仍是合法 annotate 动作。
    pub fn annotate(invocation: &Invocation, annotations: Vec<Value>) -> Self {
        let mut action = Self::new(invocation, ActionKind::Annotate);
        action.annotations = annotations;
        action
    }

    /// 改写最终上游目标；空主机或零端口在进入宿主前明确失败。
    pub fn redirect(
        invocation: &Invocation,
        host: impl Into<String>,
        port: u16,
    ) -> Result<Self, String> {
        let host = host.into();
        if host.trim().is_empty() || port == 0 {
            return Err("重定向目标必须包含有效主机和端口".to_owned());
        }
        let mut action = Self::new(invocation, ActionKind::Redirect);
        action.output = Some(json!({ "host": host, "port": port }));
        Ok(action)
    }

    /// 生成 HTTP、DNS 或命令阶段的完整合成响应；具体结构由对应阶段定义。
    pub fn respond(invocation: &Invocation, output: Value) -> Self {
        let mut action = Self::new(invocation, ActionKind::Respond);
        action.output = Some(output);
        action
    }

    /// 为 reject/close 共用原因校验与稳定输出结构，避免两个终止动作产生字段漂移。
    fn reasonAction(
        invocation: &Invocation,
        actionKind: ActionKind,
        reason: impl Into<String>,
    ) -> Result<Self, String> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err("终止原因不能为空".to_owned());
        }
        let mut action = Self::new(invocation, actionKind);
        action.output = Some(json!({ "reason": reason }));
        Ok(action)
    }

    /// 添加可序列化诊断注释；该字段不改变线上字节。
    pub fn withAnnotation(mut self, annotation: Value) -> Self {
        self.annotations.push(annotation);
        self
    }
}

/// 提供 TCP 块与 UDP 数据报共同使用的完整二进制视图。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BinaryEvent {
    pub bytes: Vec<u8>,
    pub endOfStream: bool,
}

impl BinaryEvent {
    /// 从 TCP、UDP 或正文块事件读取二进制负载；非二进制阶段或畸形字节数组明确报错。
    pub fn fromInvocation(invocation: &Invocation) -> Result<Self, String> {
        if !matches!(
            invocation.envelope.stage,
            Stage::TcpChunk
                | Stage::UdpDatagram
                | Stage::RequestBodyChunk
                | Stage::ResponseBodyChunk
                | Stage::WebSocketFrame
        ) {
            return Err("当前阶段不包含可修改二进制负载".to_owned());
        }
        let bytes = serde_json::from_value::<Vec<u8>>(
            invocation
                .envelope
                .payload
                .get("bytes")
                .cloned()
                .ok_or_else(|| "事件缺少 bytes 字段".to_owned())?,
        )
        .map_err(|_| "bytes 必须是 0..255 的整数数组".to_owned())?;
        let endOfStream = invocation
            .envelope
            .payload
            .get("endOfStream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(Self { bytes, endOfStream })
    }

    /// 使用普通闭包修改字节并返回宿主动作；闭包拥有独立缓冲，不会改写输入事件。
    pub fn modify<F>(self, invocation: &Invocation, transform: F) -> Result<Action, String>
    where
        F: FnOnce(Vec<u8>) -> Vec<u8>,
    {
        Action::modifyBytes(invocation, transform(self.bytes))
    }
}
