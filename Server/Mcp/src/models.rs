use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// 为所有基线工具提供可选区域设置；缺省值由服务启动环境决定。使用普通注释避免
// schemars 把单语内部说明导出为 MCP 参数文案，客户端只接收已本地化的 tool 描述。
#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocaleArguments {
    #[serde(default)]
    pub locale: Option<String>,
}

// 表示单个插件启停请求；插件 ID 会作为单一路径段编码，避免影响控制路由结构。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginEnabledArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub pluginId: String,
    pub enabled: bool,
}

// 表示配置更新中的一次性认证材料；该类型禁止派生 Debug，避免口令进入诊断。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialsPayload {
    pub username: String,
    pub password: String,
}

// 限定当前控制契约允许的公开认证模式。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthenticationModePayload {
    None,
    Password,
}

// 镜像 HTTP 正向代理监听配置；字段单位与控制契约一致，避免 MCP 自行转换超时。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpProxyConfigurationPayload {
    pub enabled: bool,
    pub listenHost: String,
    pub listenPort: u16,
    pub maxConnections: usize,
    pub maxHeaderBytes: usize,
    pub maxCaptureBodyBytes: usize,
    pub connectTimeoutMilliseconds: u64,
    pub requestTimeoutMilliseconds: u64,
    pub headerReadTimeoutMilliseconds: u64,
    pub shutdownTimeoutMilliseconds: u64,
}

// 限定二级代理使用的线协议；普通注释避免 schemars 将内部说明导出到公开参数 schema。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpstreamProxyProtocolPayload {
    Http,
    Socks5,
}

// 承载二级代理的完整更新配置；`password=null` 保留现有口令，空字符串明确清除口令。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpstreamProxyConfigurationPayload {
    pub enabled: bool,
    pub protocol: UpstreamProxyProtocolPayload,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
}

// 限定 WinDivert 的单个目标进程编号；PID 0 不表示可捕获的用户进程。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(transparent)]
pub struct ProcessIdPayload(#[schemars(range(min = 1))] pub u32);

// 承载 WinDivert 进程捕获的公开配置；代理端口必须与融合监听端口保持一致。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessCaptureConfigurationPayload {
    pub enabled: bool,
    pub processIds: Vec<ProcessIdPayload>,
    pub proxyPort: u16,
}

// 镜像当前 PUT /configuration 请求体；字段保持 camelCase 并由 MCP schema 提前校验。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationPayload {
    pub listenHost: String,
    pub listenPort: u16,
    pub authenticationMode: AuthenticationModePayload,
    pub maxConnections: usize,
    pub connectTimeout: f64,
    pub bindTimeout: f64,
    pub idleTimeout: f64,
    pub shutdownTimeout: f64,
    pub readTimeout: f64,
    pub relayBufferSize: usize,
    pub udpBindHost: String,
    pub udpMaxPacketSize: usize,
    pub credentials: Option<CredentialsPayload>,
    pub httpProxy: HttpProxyConfigurationPayload,
    pub upstreamProxy: UpstreamProxyConfigurationPayload,
    pub processCapture: ProcessCaptureConfigurationPayload,
}

// 聚合配置更新与本次调用区域设置；配置对象原样转发到人工 UI 使用的同一路由。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationUpdateArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub configuration: ConfigurationPayload,
}

// 限定录制会话接纳新事务的两种稳定状态；暂停只影响新事务，不中断代理转发。
#[derive(Clone, Copy, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingStatePayload {
    Recording,
    Paused,
}

// 镜像录制忽略规则；可选字段保持 LocationPattern 的空值通配语义。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocationPatternPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

// 承载 PUT /recording 的非破坏性可选字段；正文和事务限额保持只读，避免 MCP 重新启用裁剪或淘汰。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingUpdatePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RecordingStatePayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignoreLocations: Option<Vec<LocationPatternPayload>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recordTunnelMetadata: Option<bool>,
}

// 聚合录制更新与调用级语言；recording 对象原样序列化为控制 API 请求体。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingUpdateArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub recording: RecordingUpdatePayload,
}

// 镜像 PUT /ssl 的完整配置；包含和排除规则由共用 Location 语义在控制 API 再次校验。
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SslConfigurationPayload {
    pub enabled: bool,
    pub includeLocations: Vec<LocationPatternPayload>,
    pub excludeLocations: Vec<LocationPatternPayload>,
    pub maxCachedCertificates: usize,
    pub useClientSni: bool,
}

// 聚合 SSL 配置更新与调用语言；证书私钥不属于任何 MCP 参数或结果结构。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SslUpdateArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub ssl: SslConfigurationPayload,
}

// 限定公开根证书导出格式；PEM 为文本证书，CER 为 X.509 DER 字节。
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RootCertificateFormat {
    Pem,
    Cer,
}

impl RootCertificateFormat {
    /// 返回控制查询值；格式枚举消除自由文本注入查询结构的可能。
    pub const fn queryValue(self) -> &'static str {
        match self {
            Self::Pem => "pem",
            Self::Cer => "cer",
        }
    }

    /// 返回 MCP 结构化结果使用的固定下载文件名。
    pub const fn fileName(self) -> &'static str {
        match self {
            Self::Pem => "root.pem",
            Self::Cer => "root.cer",
        }
    }
}

// 聚合根证书导出格式与调用语言；结果以有界 base64 返回，便于 stdio JSON 安全传输。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SslExportArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub format: RootCertificateFormat,
}

// 提供事务列表的有界分页参数；缺省边界由控制 API 统一决定。
#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionListArguments {
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub collectionToken: Option<String>,
}

// 标识一次事务详情读取；标识符会作为单个路径段编码，不能改变控制路由。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionGetArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub transactionId: String,
}

// 限定正文读取侧，序列化值与控制 API 的 side 查询参数一致。
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TransactionBodySide {
    Request,
    Response,
}

impl TransactionBodySide {
    /// 返回稳定路径段；调用方只允许 request 或 response，不接受自由文本分支。
    pub const fn pathSegment(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

// 聚合事务标识与正文侧；正文仍由控制 API 以有界 base64 响应返回。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionBodyArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub transactionId: String,
    pub side: TransactionBodySide,
}

// 承载任一完整工具配置；具体字段由与 Web 共用的控制 API 按工具标识严格反序列化，MCP 不复制第二套规则语义。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolUpdateArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub configuration: Value,
}

// 指定需要继续或中止的挂起断点事务；标识最终作为单个编码路径段发送。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BreakpointTransactionArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub transactionId: String,
}

// 承载断点继续时的完整可编辑 HTTP 草稿；控制 API 校验头、URL、状态和 Base64 正文边界。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BreakpointContinueArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub transactionId: String,
    pub draft: Value,
}

// 描述 HAR 导出范围；未给 transactionIds 时导出当前录制会话全部事务。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarExportArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub includeBodies: bool,
    #[serde(default)]
    pub transactionIds: Option<Vec<String>>,
}

// 从已有事务派生重复请求；覆盖字段与人工界面的 Compose 编辑器使用同一 camelCase 协议对象。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionRepeatArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub transactionId: String,
    #[serde(default)]
    pub overrides: Option<Value>,
}

// 承载高级重复计划和明确确认；计划正文保持透传，唯一权威校验仍在控制 API。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdvancedRepeatArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub plan: Value,
    pub confirmed: bool,
}

// 承载辅助监听规则数组；数组内容由控制 API 用与人工界面相同的严格结构校验。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListenerUpdateArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub entries: Value,
}

// 承载 Protobuf 或 Validate 的完整配置；具体字段由控制 API 严格反序列化，MCP 不复制第二套规则。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolConfigurationArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub configuration: Value,
}

// 上传 FileDescriptorSet 的唯一 MCP 载荷；Base64 只转发至控制 API，不写入 MCP 进程文件系统。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtobufUploadArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub name: String,
    pub defaultMessageType: String,
    pub base64: String,
}

// 请求对指定事务消息侧执行 Protobuf 解码；正文仍只由控制 API 按需读取。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtobufDecodeArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub transactionId: String,
    #[serde(default)]
    pub side: Option<TransactionBodySide>,
}

// 请求 Validate 执行；W3C 选项只有调用方明确确认上传时才会被控制 API 允许外发正文。
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidateTransactionArguments {
    #[serde(default)]
    pub locale: Option<String>,
    pub transactionId: String,
    pub validatorId: String,
    #[serde(default)]
    pub onlineUploadConfirmed: bool,
}
