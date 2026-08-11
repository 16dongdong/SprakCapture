use std::{borrow::Cow, collections::HashMap, env};

use base64::{Engine, engine::general_purpose::STANDARD as base64Standard};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde_json::{Value, json};

use crate::{
    controlClient::{
        ControlClient, ControlFailure, deleteMethod, getMethod, postMethod, putMethod,
    },
    localization::MessageCatalog,
    models::{
        AdvancedRepeatArguments, BreakpointContinueArguments, BreakpointTransactionArguments,
        ConfigurationUpdateArguments, HarExportArguments, ListenerUpdateArguments, LocaleArguments,
        PluginEnabledArguments, ProtobufDecodeArguments, ProtobufUploadArguments,
        ProtocolConfigurationArguments, RecordingUpdateArguments, SslExportArguments,
        SslUpdateArguments, ToolUpdateArguments, TransactionBodyArguments, TransactionGetArguments,
        TransactionListArguments, TransactionRepeatArguments, ValidateTransactionArguments,
    },
};

const defaultControlBase: &str = "http://127.0.0.1:17890";

// 路径段必须编码所有可能改变路由结构或 URL 解析的字节，事务标识即使未来不再是 UUID
// 也只能到达单条详情路由。
const pathSegmentEncodeSet: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b':')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

// 集合令牌属于不透明查询值，必须编码可能引入第二个参数或改变 URL 语义的全部保留字符；
// 只保留 RFC 3986 的未保留字符，避免 MCP 与控制 API 对同一令牌产生不同解释。
const queryValueEncodeSet: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

const toolDescriptionKeys: [(&str, &str); 60] = [
    (
        "capture_service_get_snapshot",
        "mcp.tool.serviceGetSnapshot.description",
    ),
    ("capture_service_start", "mcp.tool.serviceStart.description"),
    ("capture_service_stop", "mcp.tool.serviceStop.description"),
    ("capture_plugin_list", "mcp.tool.pluginList.description"),
    (
        "capture_plugin_set_enabled",
        "mcp.tool.pluginSetEnabled.description",
    ),
    ("capture_config_get", "mcp.tool.configGet.description"),
    ("capture_config_update", "mcp.tool.configUpdate.description"),
    (
        "capture_sessions_clear_finished",
        "mcp.tool.sessionsClearFinished.description",
    ),
    ("capture_ssl_get", "mcp.tool.sslGet.description"),
    ("capture_ssl_update", "mcp.tool.sslUpdate.description"),
    (
        "capture_ssl_export_root",
        "mcp.tool.sslExportRoot.description",
    ),
    (
        "capture_ssl_regenerate_root",
        "mcp.tool.sslRegenerateRoot.description",
    ),
    ("capture_recording_get", "mcp.tool.recordingGet.description"),
    (
        "capture_recording_update",
        "mcp.tool.recordingUpdate.description",
    ),
    (
        "capture_recording_clear",
        "mcp.tool.recordingClear.description",
    ),
    (
        "capture_transaction_list",
        "mcp.tool.transactionList.description",
    ),
    (
        "capture_transaction_get",
        "mcp.tool.transactionGet.description",
    ),
    (
        "capture_transaction_get_body",
        "mcp.tool.transactionGetBody.description",
    ),
    (
        "capture_transaction_repeat",
        "mcp.tool.transactionRepeat.description",
    ),
    (
        "capture_transaction_repeat_edited",
        "mcp.tool.transactionRepeatEdited.description",
    ),
    (
        "capture_transaction_repeat_advanced",
        "mcp.tool.transactionRepeatAdvanced.description",
    ),
    ("capture_tools_summary", "mcp.tool.toolsSummary.description"),
    (
        "capture_tool_packet_filters_get",
        "mcp.tool.packetFiltersGet.description",
    ),
    (
        "capture_tool_packet_filters_update",
        "mcp.tool.packetFiltersUpdate.description",
    ),
    ("capture_tool_block_get", "mcp.tool.blockGet.description"),
    (
        "capture_tool_block_update",
        "mcp.tool.blockUpdate.description",
    ),
    (
        "capture_tool_no_caching_get",
        "mcp.tool.noCachingGet.description",
    ),
    (
        "capture_tool_no_caching_update",
        "mcp.tool.noCachingUpdate.description",
    ),
    (
        "capture_tool_block_cookies_get",
        "mcp.tool.blockCookiesGet.description",
    ),
    (
        "capture_tool_block_cookies_update",
        "mcp.tool.blockCookiesUpdate.description",
    ),
    (
        "capture_tool_map_local_get",
        "mcp.tool.mapLocalGet.description",
    ),
    (
        "capture_tool_map_local_update",
        "mcp.tool.mapLocalUpdate.description",
    ),
    (
        "capture_tool_map_remote_get",
        "mcp.tool.mapRemoteGet.description",
    ),
    (
        "capture_tool_map_remote_update",
        "mcp.tool.mapRemoteUpdate.description",
    ),
    (
        "capture_tool_rewrite_get",
        "mcp.tool.rewriteGet.description",
    ),
    (
        "capture_tool_rewrite_update",
        "mcp.tool.rewriteUpdate.description",
    ),
    (
        "capture_breakpoint_get_settings",
        "mcp.tool.breakpointGetSettings.description",
    ),
    (
        "capture_breakpoint_update",
        "mcp.tool.breakpointUpdate.description",
    ),
    (
        "capture_breakpoint_list_suspended",
        "mcp.tool.breakpointListSuspended.description",
    ),
    (
        "capture_breakpoint_continue",
        "mcp.tool.breakpointContinue.description",
    ),
    (
        "capture_breakpoint_abort",
        "mcp.tool.breakpointAbort.description",
    ),
    (
        "capture_tool_throttle_get",
        "mcp.tool.throttleGet.description",
    ),
    (
        "capture_tool_throttle_update",
        "mcp.tool.throttleUpdate.description",
    ),
    ("capture_export_har", "mcp.tool.exportHar.description"),
    ("capture_protobuf_get", "mcp.tool.protobufGet.description"),
    (
        "capture_protobuf_update",
        "mcp.tool.protobufUpdate.description",
    ),
    (
        "capture_protobuf_upload",
        "mcp.tool.protobufUpload.description",
    ),
    (
        "capture_protobuf_decode",
        "mcp.tool.protobufDecode.description",
    ),
    ("capture_validate_get", "mcp.tool.validateGet.description"),
    (
        "capture_validate_update",
        "mcp.tool.validateUpdate.description",
    ),
    (
        "capture_validate_response",
        "mcp.tool.validateResponse.description",
    ),
    ("capture_mirror_get", "mcp.tool.mirrorGet.description"),
    ("capture_mirror_update", "mcp.tool.mirrorUpdate.description"),
    ("capture_auto_save_get", "mcp.tool.autoSaveGet.description"),
    (
        "capture_auto_save_update",
        "mcp.tool.autoSaveUpdate.description",
    ),
    ("capture_auto_save_now", "mcp.tool.autoSaveNow.description"),
    (
        "capture_port_forward_get",
        "mcp.tool.portForwardGet.description",
    ),
    (
        "capture_port_forward_update",
        "mcp.tool.portForwardUpdate.description",
    ),
    (
        "capture_reverse_proxy_get",
        "mcp.tool.reverseProxyGet.description",
    ),
    (
        "capture_reverse_proxy_update",
        "mcp.tool.reverseProxyUpdate.description",
    ),
];

/// 提供与本机 UI 等价的控制 tools；权限判断完全由既有控制 API 业务状态决定。
#[derive(Clone)]
pub struct ControlMcpServer {
    controlClient: ControlClient,
    catalog: MessageCatalog,
    defaultLocale: &'static str,
    toolRouter: ToolRouter<Self>,
}

impl ControlMcpServer {
    /// 从环境读取控制基址与默认区域设置；无变量时使用回环控制地址和英文机器友好文案。
    pub fn fromEnvironment() -> Result<Self, String> {
        let catalog = MessageCatalog::load()?;
        let requestedLocale = env::var("CAPTURE_LOCALE").ok();
        let defaultLocale = catalog.resolveLocale(requestedLocale.as_deref());
        let controlBase =
            env::var("CAPTURE_CONTROL_BASE").unwrap_or_else(|_| defaultControlBase.to_owned());
        Self::newWithCatalog(controlBase, defaultLocale, catalog)
    }

    /// 使用显式依赖创建服务，供测试验证工具路由而不修改进程环境。
    fn newWithCatalog(
        controlBase: String,
        defaultLocale: &'static str,
        catalog: MessageCatalog,
    ) -> Result<Self, String> {
        let controlClient = ControlClient::new(&controlBase).map_err(|detail| {
            let message = catalog.message(defaultLocale, "mcp.error.invalidControlBase");
            format!("{message}: {detail}")
        })?;
        let mut toolRouter = Self::toolRouter();
        localizeToolDescriptions(&mut toolRouter, &catalog, defaultLocale);
        Ok(Self {
            controlClient,
            catalog,
            defaultLocale,
            toolRouter,
        })
    }

    /// 解析调用级 locale；显式参数优先，随后使用 CAPTURE_LOCALE 在启动时确定的默认值。
    fn resolveLocale(&self, locale: Option<&str>) -> &'static str {
        locale.map_or(self.defaultLocale, |locale| {
            self.catalog.resolveLocale(Some(locale))
        })
    }

    /// 调用控制 API 并转换为 MCP 结构化成功或错误结果，业务失败不提升为 JSON-RPC 协议错误。
    async fn executeRequest(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        locale: Option<&str>,
    ) -> CallToolResult {
        let resolvedLocale = self.resolveLocale(locale);
        match self
            .controlClient
            .request(method, path, body, resolvedLocale)
            .await
        {
            Ok(response) => CallToolResult::structured(response),
            Err(error) => self.controlErrorResult(error, resolvedLocale),
        }
    }

    /// 调用固定 204 控制动作并返回简洁确认对象；用于断点继续和中止，不把空响应误解为协议故障。
    async fn executeNoContentRequest(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        locale: Option<&str>,
    ) -> CallToolResult {
        let resolvedLocale = self.resolveLocale(locale);
        match self
            .controlClient
            .requestNoContent(method, path, body, resolvedLocale)
            .await
        {
            Ok(()) => CallToolResult::structured(json!({ "completed": true })),
            Err(error) => self.controlErrorResult(error, resolvedLocale),
        }
    }

    /// 把传输、业务状态和解码失败映射到稳定结构；正文只保留后端声明的允许字段。
    fn controlErrorResult(&self, error: ControlFailure, locale: &str) -> CallToolResult {
        match error {
            ControlFailure::Unavailable => {
                self.errorResult("mcp.error.controlUnavailable", locale, json!({}))
            }
            ControlFailure::Rejected { statusCode, error } => {
                CallToolResult::structured_error(json!({
                    "code": error.code,
                    "messageKey": error.messageKey,
                    "message": error.message,
                    "params": error.params,
                    "controlStatus": statusCode,
                }))
            }
            ControlFailure::InvalidResponse { metadata } => {
                let params = metadata.map_or_else(
                    || json!({}),
                    |metadata| {
                        json!({
                            "statusCode": metadata.statusCode,
                            "contentType": metadata.contentType,
                            "contentLength": metadata.contentLength,
                            "contentDigest": metadata.contentDigest,
                        })
                    },
                );
                self.errorResult("mcp.error.invalidControlResponse", locale, params)
            }
        }
    }

    /// 构造调用方可见的 MCP tool error；结构固定包含 messageKey、message、params。
    fn errorResult(&self, messageKey: &str, locale: &str, params: Value) -> CallToolResult {
        CallToolResult::structured_error(json!({
            "messageKey": messageKey,
            "message": self.catalog.message(locale, messageKey),
            "params": params,
        }))
    }

    /// 返回配置对象或本地化结构错误；snapshot 仍是唯一配置权威来源。
    async fn getConfiguration(&self, locale: Option<&str>) -> CallToolResult {
        let resolvedLocale = self.resolveLocale(locale);
        match self
            .controlClient
            .request(getMethod(), "api/v1/snapshot", None, resolvedLocale)
            .await
        {
            Ok(snapshot) => match snapshot.get("configuration").cloned() {
                Some(configuration) => CallToolResult::structured(configuration),
                None => {
                    self.errorResult("mcp.error.configurationMissing", resolvedLocale, json!({}))
                }
            },
            Err(error) => self.controlErrorResult(error, resolvedLocale),
        }
    }

    /// 从权威快照读取完整工具状态或指定工具配置，避免 MCP 缓存造成 UI 与自动化观察不同步。
    async fn getToolState(&self, toolId: Option<&str>, locale: Option<&str>) -> CallToolResult {
        let resolvedLocale = self.resolveLocale(locale);
        match self
            .controlClient
            .request(getMethod(), "api/v1/snapshot", None, resolvedLocale)
            .await
        {
            Ok(snapshot) => {
                let Some(tools) = snapshot.get("tools") else {
                    return self.errorResult("mcp.error.toolsMissing", resolvedLocale, json!({}));
                };
                let result = toolId
                    .and_then(|id| tools.get(id))
                    .cloned()
                    .or_else(|| toolId.is_none().then(|| tools.clone()));
                match result {
                    Some(result) => CallToolResult::structured(result),
                    None => self.errorResult(
                        "mcp.error.toolMissing",
                        resolvedLocale,
                        json!({
                            "toolId": toolId,
                        }),
                    ),
                }
            }
            Err(error) => self.controlErrorResult(error, resolvedLocale),
        }
    }

    /// 用单一控制路径写入工具配置；字段校验、热更新、持久化与事件发布全部由后台完成。
    async fn updateTool(&self, toolId: &str, arguments: ToolUpdateArguments) -> CallToolResult {
        self.executeRequest(
            putMethod(),
            &format!("api/v1/tools/{toolId}"),
            Some(arguments.configuration),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 用与桌面界面相同的资源路由替换整组辅助监听规则；控制 API 负责端口冲突与服务生命周期。
    async fn updateListenerEntries(
        &self,
        path: &str,
        arguments: ListenerUpdateArguments,
    ) -> CallToolResult {
        self.executeRequest(
            putMethod(),
            path,
            Some(arguments.entries),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 为断点队列构造单个受编码事务路径；任何无效标识都在请求离开 MCP 前返回本地化错误。
    fn breakpointPath(
        &self,
        transactionId: &str,
        locale: Option<&str>,
    ) -> Result<String, CallToolResult> {
        if transactionId.is_empty() || matches!(transactionId, "." | "..") {
            let resolvedLocale = self.resolveLocale(locale);
            return Err(self.errorResult(
                "mcp.error.invalidTransactionId",
                resolvedLocale,
                json!({}),
            ));
        }
        let encodedId = utf8_percent_encode(transactionId, pathSegmentEncodeSet);
        Ok(format!("api/v1/breakpoints/suspended/{encodedId}"))
    }

    /// 构造事务列表分页路径；集合令牌按不透明查询值编码，固定顺序保证 fixture 可复现。
    fn transactionListPath(arguments: &TransactionListArguments) -> String {
        let mut query = Vec::with_capacity(3);
        if let Some(offset) = arguments.offset {
            query.push(format!("offset={offset}"));
        }
        if let Some(limit) = arguments.limit {
            query.push(format!("limit={limit}"));
        }
        if let Some(collectionToken) = arguments.collectionToken.as_deref() {
            let encodedToken = utf8_percent_encode(collectionToken, queryValueEncodeSet);
            query.push(format!("collectionToken={encodedToken}"));
        }
        if query.is_empty() {
            "api/v1/transactions".to_owned()
        } else {
            format!("api/v1/transactions?{}", query.join("&"))
        }
    }

    /// 将事务标识限制在单个 URL 路径段内；空值和相对路径段必须在 Url::join 前拒绝，
    /// 其余保留字符统一编码，最终存在性仍由控制 API 判定。
    fn transactionPath(
        &self,
        transactionId: &str,
        locale: Option<&str>,
    ) -> Result<String, CallToolResult> {
        if transactionId.is_empty() || matches!(transactionId, "." | "..") {
            let resolvedLocale = self.resolveLocale(locale);
            return Err(self.errorResult(
                "mcp.error.invalidTransactionId",
                resolvedLocale,
                json!({}),
            ));
        }
        let encodedId = utf8_percent_encode(transactionId, pathSegmentEncodeSet);
        Ok(format!("api/v1/transactions/{encodedId}"))
    }
}

#[tool_router(router = toolRouter)]
impl ControlMcpServer {
    /// 获取完整权威快照。
    #[tool(
        name = "capture_service_get_snapshot",
        description = "Get the authoritative Sprak Capture service snapshot.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn serviceGetSnapshot(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            getMethod(),
            "api/v1/snapshot",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 启动 SOCKS5 数据面。
    #[tool(
        name = "capture_service_start",
        description = "Start the Sprak Capture data service.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn serviceStart(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            postMethod(),
            "api/v1/service/start",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 停止 SOCKS5 数据面。
    #[tool(
        name = "capture_service_stop",
        description = "Stop the Sprak Capture data service.",
        annotations(
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn serviceStop(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            postMethod(),
            "api/v1/service/stop",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 从权威快照读取公开配置。
    #[tool(
        name = "capture_config_get",
        description = "Get the public Sprak Capture service configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn configGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getConfiguration(arguments.locale.as_deref()).await
    }

    /// 使用与 UI 相同的控制路由替换服务配置；运行中的数据面会先断开活动代理连接并重启。
    #[tool(
        name = "capture_config_update",
        description = "Replace Sprak Capture service configuration; a running data service disconnects active proxy connections and restarts.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn configUpdate(
        &self,
        Parameters(arguments): Parameters<ConfigurationUpdateArguments>,
    ) -> CallToolResult {
        let body = serde_json::to_value(arguments.configuration)
            .expect("ConfigurationPayload serialization is infallible");
        self.executeRequest(
            putMethod(),
            "api/v1/configuration",
            Some(body),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 清除已结束 SOCKS5 会话；是否先确认由 Skill 工作流决定，服务端不设置权限围栏。
    #[tool(
        name = "capture_sessions_clear_finished",
        description = "Clear finished SOCKS5 session history.",
        annotations(
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn sessionsClearFinished(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            deleteMethod(),
            "api/v1/sessions",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 获取 SSL 主机范围、公开根证书元数据、缓存和握手累计值。
    #[tool(
        name = "capture_ssl_get",
        description = "Get SSL proxy settings and public certificate state.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn sslGet(&self, Parameters(arguments): Parameters<LocaleArguments>) -> CallToolResult {
        self.executeRequest(getMethod(), "api/v1/ssl", None, arguments.locale.as_deref())
            .await
    }

    /// 使用与 Web 对话框相同的控制路由替换 SSL 主机规则和缓存边界。
    #[tool(
        name = "capture_ssl_update",
        description = "Replace SSL proxy matching and certificate cache settings.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn sslUpdate(
        &self,
        Parameters(arguments): Parameters<SslUpdateArguments>,
    ) -> CallToolResult {
        let body = serde_json::to_value(arguments.ssl)
            .expect("SslConfigurationPayload serialization is infallible");
        self.executeRequest(
            putMethod(),
            "api/v1/ssl",
            Some(body),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 导出公开根证书并以有界 base64 返回；结果结构不存在根私钥字段。
    #[tool(
        name = "capture_ssl_export_root",
        description = "Export the public SSL root certificate as PEM or CER.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn sslExportRoot(
        &self,
        Parameters(arguments): Parameters<SslExportArguments>,
    ) -> CallToolResult {
        let locale = self.resolveLocale(arguments.locale.as_deref());
        let format = arguments.format;
        let path = format!("api/v1/ssl/ca/export?format={}", format.queryValue());
        match self
            .controlClient
            .requestBytes(getMethod(), &path, locale)
            .await
        {
            Ok(certificate) => CallToolResult::structured(json!({
                "format": format.queryValue(),
                "fileName": format.fileName(),
                "contentType": certificate.contentType,
                "byteLength": certificate.bytes.len(),
                "base64": base64Standard.encode(certificate.bytes),
            })),
            Err(error) => self.controlErrorResult(error, locale),
        }
    }

    /// 更换根 CA 并清空叶证书缓存；Skill 必须在调用前取得用户确认。
    #[tool(
        name = "capture_ssl_regenerate_root",
        description = "Regenerate the SSL root certificate and clear the leaf cache.",
        annotations(
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn sslRegenerateRoot(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            postMethod(),
            "api/v1/ssl/ca/generate",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 获取 active RecordingSession 的权威状态和资源限额。
    #[tool(
        name = "capture_recording_get",
        description = "Get the active recording session state and limits.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn recordingGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            getMethod(),
            "api/v1/recording",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 更新 active RecordingSession 的状态与过滤规则；完整正文和事务保留边界不可修改。
    #[tool(
        name = "capture_recording_update",
        description = "Update the active recording session state or filtering rules.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn recordingUpdate(
        &self,
        Parameters(arguments): Parameters<RecordingUpdateArguments>,
    ) -> CallToolResult {
        let body = serde_json::to_value(arguments.recording)
            .expect("RecordingUpdatePayload serialization is infallible");
        self.executeRequest(
            putMethod(),
            "api/v1/recording",
            Some(body),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 清空 active RecordingSession 的全部事务和正文；确认流程仅由 Skill 约束。
    #[tool(
        name = "capture_recording_clear",
        description = "Clear all transactions and bodies from the active recording session.",
        annotations(
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn recordingClear(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            postMethod(),
            "api/v1/recording/clear",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 按分页边界读取事务元数据列表；该路由不会请求头或正文。
    #[tool(
        name = "capture_transaction_list",
        description = "List bounded transaction metadata without message bodies.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn transactionList(
        &self,
        Parameters(arguments): Parameters<TransactionListArguments>,
    ) -> CallToolResult {
        let path = Self::transactionListPath(&arguments);
        self.executeRequest(getMethod(), &path, None, arguments.locale.as_deref())
            .await
    }

    /// 读取单条事务元数据与请求、响应头；正文必须通过独立 tool 按需获取。
    #[tool(
        name = "capture_transaction_get",
        description = "Get transaction metadata and headers without message bodies.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn transactionGet(
        &self,
        Parameters(arguments): Parameters<TransactionGetArguments>,
    ) -> CallToolResult {
        let path = match self.transactionPath(&arguments.transactionId, arguments.locale.as_deref())
        {
            Ok(path) => path,
            Err(error) => return error,
        };
        self.executeRequest(getMethod(), &path, None, arguments.locale.as_deref())
            .await
    }

    /// 按请求侧或响应侧读取单条事务正文；控制 API 保持存储限额和 base64 传输语义。
    #[tool(
        name = "capture_transaction_get_body",
        description = "Get one request or response body for a transaction.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn transactionGetBody(
        &self,
        Parameters(arguments): Parameters<TransactionBodyArguments>,
    ) -> CallToolResult {
        let transactionPath =
            match self.transactionPath(&arguments.transactionId, arguments.locale.as_deref()) {
                Ok(path) => path,
                Err(error) => return error,
            };
        let path = format!("{}/{}/body", transactionPath, arguments.side.pathSegment());
        self.executeRequest(getMethod(), &path, None, arguments.locale.as_deref())
            .await
    }

    /// 原样重复指定事务；后端从原始录制读取请求头和正文，并创建全新的事务记录。
    #[tool(
        name = "capture_transaction_repeat",
        description = "Repeat one recorded HTTP transaction without changing the original.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn transactionRepeat(
        &self,
        Parameters(arguments): Parameters<TransactionRepeatArguments>,
    ) -> CallToolResult {
        let path = match self.transactionPath(&arguments.transactionId, arguments.locale.as_deref())
        {
            Ok(path) => path,
            Err(error) => return error,
        };
        self.executeRequest(
            postMethod(),
            &format!("{path}/repeat"),
            Some(json!({ "transactionId": arguments.transactionId })),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 以显式覆盖字段重复事务；调用端仅提交要修改的字段，缺失字段由原始事务继承。
    #[tool(
        name = "capture_transaction_repeat_edited",
        description = "Repeat a recorded HTTP transaction with edited request fields.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn transactionRepeatEdited(
        &self,
        Parameters(arguments): Parameters<TransactionRepeatArguments>,
    ) -> CallToolResult {
        let path = match self.transactionPath(&arguments.transactionId, arguments.locale.as_deref())
        {
            Ok(path) => path,
            Err(error) => return error,
        };
        self.executeRequest(
            postMethod(),
            &format!("{path}/repeat"),
            Some(json!({
                "transactionId": arguments.transactionId,
                "overrides": arguments.overrides,
            })),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 启动有并发和次数上限的高级重复作业；confirmed 必须为 true，后端才会分配网络与录制资源。
    #[tool(
        name = "capture_transaction_repeat_advanced",
        description = "Start a bounded advanced repeat job after explicit confirmation.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn transactionRepeatAdvanced(
        &self,
        Parameters(arguments): Parameters<AdvancedRepeatArguments>,
    ) -> CallToolResult {
        let mut request = match arguments.plan {
            Value::Object(object) => object,
            _ => {
                let locale = self.resolveLocale(arguments.locale.as_deref());
                return self.errorResult("mcp.error.invalidControlResponse", locale, json!({}));
            }
        };
        request.insert("confirmed".to_owned(), Value::Bool(arguments.confirmed));
        self.executeRequest(
            postMethod(),
            "api/v1/loadTests",
            Some(Value::Object(request)),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 获取全部工具的权威配置与执行槽顺序。
    #[tool(
        name = "capture_tools_summary",
        description = "Get the current proxy tool pipeline state.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn toolsSummary(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(None, arguments.locale.as_deref()).await
    }

    /// 读取最终写线前执行的封包滤镜配置。
    #[tool(
        name = "capture_tool_packet_filters_get",
        description = "Get the ordered packet-filter configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn packetFiltersGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("packetFilters"), arguments.locale.as_deref())
            .await
    }

    /// 替换完整封包滤镜规则集；配置会持久化并原子热更新现有连接。
    #[tool(
        name = "capture_tool_packet_filters_update",
        description = "Replace the ordered packet-filter configuration.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn packetFiltersUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("packetFilters", arguments).await
    }

    /// 读取屏蔽列表配置。
    #[tool(
        name = "capture_tool_block_get",
        description = "Get the block-list tool configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn blockGet(&self, Parameters(arguments): Parameters<LocaleArguments>) -> CallToolResult {
        self.getToolState(Some("blockList"), arguments.locale.as_deref())
            .await
    }

    /// 替换屏蔽列表配置。
    #[tool(
        name = "capture_tool_block_update",
        description = "Replace the block-list tool configuration.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn blockUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("blockList", arguments).await
    }

    /// 读取无缓存配置。
    #[tool(
        name = "capture_tool_no_caching_get",
        description = "Get the no-caching tool configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn noCachingGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("noCaching"), arguments.locale.as_deref())
            .await
    }

    /// 替换无缓存配置。
    #[tool(
        name = "capture_tool_no_caching_update",
        description = "Replace the no-caching tool configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn noCachingUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("noCaching", arguments).await
    }

    /// 读取 Cookie 剥离配置。
    #[tool(
        name = "capture_tool_block_cookies_get",
        description = "Get the cookie-blocking tool configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn blockCookiesGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("blockCookies"), arguments.locale.as_deref())
            .await
    }

    /// 替换 Cookie 剥离配置。
    #[tool(
        name = "capture_tool_block_cookies_update",
        description = "Replace the cookie-blocking tool configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn blockCookiesUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("blockCookies", arguments).await
    }

    /// 读取 Map Local 配置。
    #[tool(
        name = "capture_tool_map_local_get",
        description = "Get the Map Local tool configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn mapLocalGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("mapLocal"), arguments.locale.as_deref())
            .await
    }

    /// 替换 Map Local 配置。
    #[tool(
        name = "capture_tool_map_local_update",
        description = "Replace the Map Local tool configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn mapLocalUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("mapLocal", arguments).await
    }

    /// 读取 Map Remote 配置。
    #[tool(
        name = "capture_tool_map_remote_get",
        description = "Get the Map Remote tool configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn mapRemoteGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("mapRemote"), arguments.locale.as_deref())
            .await
    }

    /// 替换 Map Remote 配置。
    #[tool(
        name = "capture_tool_map_remote_update",
        description = "Replace the Map Remote tool configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn mapRemoteUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("mapRemote", arguments).await
    }

    /// 读取 Rewrite 集合。
    #[tool(
        name = "capture_tool_rewrite_get",
        description = "Get the Rewrite tool configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn rewriteGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("rewrite"), arguments.locale.as_deref())
            .await
    }

    /// 替换 Rewrite 集合。
    #[tool(
        name = "capture_tool_rewrite_update",
        description = "Replace the Rewrite tool configuration.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn rewriteUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("rewrite", arguments).await
    }

    /// 读取断点规则和队列边界。
    #[tool(
        name = "capture_breakpoint_get_settings",
        description = "Get breakpoint rules and queue limits.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn breakpointGetSettings(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("breakpoints"), arguments.locale.as_deref())
            .await
    }

    /// 替换断点规则和队列边界。
    #[tool(
        name = "capture_breakpoint_update",
        description = "Replace breakpoint rules and queue limits.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn breakpointUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("breakpoints", arguments).await
    }

    /// 列出目前等待人工继续或中止的断点事务。
    #[tool(
        name = "capture_breakpoint_list_suspended",
        description = "List suspended breakpoint transactions.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn breakpointListSuspended(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            getMethod(),
            "api/v1/breakpoints/suspended",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 提交编辑草稿并继续指定断点事务。
    #[tool(
        name = "capture_breakpoint_continue",
        description = "Apply an edited breakpoint draft and continue the transaction.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn breakpointContinue(
        &self,
        Parameters(arguments): Parameters<BreakpointContinueArguments>,
    ) -> CallToolResult {
        let path = match self.breakpointPath(&arguments.transactionId, arguments.locale.as_deref())
        {
            Ok(path) => format!("{path}/continue"),
            Err(error) => return error,
        };
        self.executeNoContentRequest(
            postMethod(),
            &path,
            Some(arguments.draft),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 中止指定断点事务并释放挂起槽位。
    #[tool(
        name = "capture_breakpoint_abort",
        description = "Abort a suspended breakpoint transaction.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn breakpointAbort(
        &self,
        Parameters(arguments): Parameters<BreakpointTransactionArguments>,
    ) -> CallToolResult {
        let path = match self.breakpointPath(&arguments.transactionId, arguments.locale.as_deref())
        {
            Ok(path) => format!("{path}/abort"),
            Err(error) => return error,
        };
        self.executeNoContentRequest(postMethod(), &path, None, arguments.locale.as_deref())
            .await
    }

    /// 读取节流配置和只读预设列表。
    #[tool(
        name = "capture_tool_throttle_get",
        description = "Get the throttling tool configuration and presets.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn throttleGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("throttling"), arguments.locale.as_deref())
            .await
    }

    /// 替换节流开关、作用域和速率参数。
    #[tool(
        name = "capture_tool_throttle_update",
        description = "Replace the throttling tool configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn throttleUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("throttling", arguments).await
    }

    /// 读取镜像目录、写入策略和累计状态。
    #[tool(
        name = "capture_mirror_get",
        description = "Get the mirror tool configuration and write status.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn mirrorGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("mirror"), arguments.locale.as_deref())
            .await
    }

    /// 替换镜像目录、作用域和写入队列策略。
    #[tool(
        name = "capture_mirror_update",
        description = "Replace the mirror tool configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn mirrorUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("mirror", arguments).await
    }

    /// 读取自动保存配置和最近导出状态。
    #[tool(
        name = "capture_auto_save_get",
        description = "Get automatic recording-save configuration and last result.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn autoSaveGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.getToolState(Some("autoSave"), arguments.locale.as_deref())
            .await
    }

    /// 替换自动保存目录、触发器和归档轮转配置。
    #[tool(
        name = "capture_auto_save_update",
        description = "Replace automatic recording-save configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn autoSaveUpdate(
        &self,
        Parameters(arguments): Parameters<ToolUpdateArguments>,
    ) -> CallToolResult {
        self.updateTool("autoSave", arguments).await
    }

    /// 立即导出当前录制会话到已配置的自动保存目录。
    #[tool(
        name = "capture_auto_save_now",
        description = "Save the current recording immediately using automatic-save settings.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn autoSaveNow(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            postMethod(),
            "api/v1/tools/autoSave/saveNow",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 读取 TCP 端口转发规则和当前绑定端点。
    #[tool(
        name = "capture_port_forward_get",
        description = "Get TCP port-forward rules and active bindings.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn portForwardGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            getMethod(),
            "api/v1/listeners/portForwards",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 替换 TCP 端口转发规则；更新期间控制层会有序断开旧转发连接。
    #[tool(
        name = "capture_port_forward_update",
        description = "Replace TCP port-forward rules.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn portForwardUpdate(
        &self,
        Parameters(arguments): Parameters<ListenerUpdateArguments>,
    ) -> CallToolResult {
        self.updateListenerEntries("api/v1/listeners/portForwards", arguments)
            .await
    }

    /// 读取反向代理规则和当前绑定端点。
    #[tool(
        name = "capture_reverse_proxy_get",
        description = "Get reverse-proxy rules and active bindings.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn reverseProxyGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            getMethod(),
            "api/v1/listeners/reverseProxies",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 替换反向代理规则；请求继续经过 HTTP 流水线、录制和已启用工具。
    #[tool(
        name = "capture_reverse_proxy_update",
        description = "Replace reverse-proxy rules.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn reverseProxyUpdate(
        &self,
        Parameters(arguments): Parameters<ListenerUpdateArguments>,
    ) -> CallToolResult {
        self.updateListenerEntries("api/v1/listeners/reverseProxies", arguments)
            .await
    }

    /// 导出当前或指定事务为 HAR 1.2，以有界 Base64 形式回传给 MCP 调用方。
    #[tool(
        name = "capture_export_har",
        description = "Export recorded transactions as a HAR 1.2 archive.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn exportHar(
        &self,
        Parameters(arguments): Parameters<HarExportArguments>,
    ) -> CallToolResult {
        let locale = self.resolveLocale(arguments.locale.as_deref());
        // 控制面以字段缺失表示“导出全部事务”；显式传递 null 会破坏后端的数组反序列化。
        let mut request = serde_json::Map::new();
        request.insert("format".to_owned(), json!("har"));
        request.insert("includeBodies".to_owned(), json!(arguments.includeBodies));
        if let Some(transactionIds) = arguments.transactionIds {
            request.insert("transactionIds".to_owned(), json!(transactionIds));
        }
        let request = Value::Object(request);
        match self
            .controlClient
            .requestBytesWithBody(
                postMethod(),
                "api/v1/recording/export",
                Some(request),
                locale,
            )
            .await
        {
            Ok(archive) => CallToolResult::structured(json!({
                "format": "har",
                "fileName": "recording.har",
                "contentType": archive.contentType,
                "byteLength": archive.bytes.len(),
                "base64": base64Standard.encode(archive.bytes),
            })),
            Err(error) => self.controlErrorResult(error, locale),
        }
    }

    /// 读取 Protobuf 描述符与路由配置；结果不含描述符原始字节或用户目录绝对路径。
    #[tool(
        name = "capture_protobuf_get",
        description = "Get Protobuf descriptor and route configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn protobufGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            getMethod(),
            "api/v1/tools/protobuf",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 替换 Protobuf 解码开关和路由表；描述符登记继续使用专用上传 tool。
    #[tool(
        name = "capture_protobuf_update",
        description = "Replace Protobuf decoding configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn protobufUpdate(
        &self,
        Parameters(arguments): Parameters<ProtocolConfigurationArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            putMethod(),
            "api/v1/tools/protobuf",
            Some(arguments.configuration),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 上传经用户明确提供的 FileDescriptorSet；Base64 仅透传控制 API，不写入 MCP 本地磁盘。
    #[tool(
        name = "capture_protobuf_upload",
        description = "Upload a Protobuf FileDescriptorSet.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn protobufUpload(
        &self,
        Parameters(arguments): Parameters<ProtobufUploadArguments>,
    ) -> CallToolResult {
        let body = json!({"name":arguments.name,"defaultMessageType":arguments.defaultMessageType,"base64":arguments.base64});
        self.executeRequest(
            postMethod(),
            "api/v1/tools/protobuf/schemas",
            Some(body),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 解码单个事务的请求或响应正文；无路由或字段失败由 decodeError 表示，调用者可回退 Hex。
    #[tool(
        name = "capture_protobuf_decode",
        description = "Decode one recorded message with a configured Protobuf descriptor.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn protobufDecode(
        &self,
        Parameters(arguments): Parameters<ProtobufDecodeArguments>,
    ) -> CallToolResult {
        let transactionPath =
            match self.transactionPath(&arguments.transactionId, arguments.locale.as_deref()) {
                Ok(path) => path,
                Err(error) => return error,
            };
        let side = arguments.side.map_or("response", |side| side.pathSegment());
        self.executeRequest(
            getMethod(),
            &format!("{transactionPath}/decode/protobuf?side={side}"),
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 读取 Validate 配置，调用方据此决定是否需要请求用户确认在线正文上传。
    #[tool(
        name = "capture_validate_get",
        description = "Get response validation configuration.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn validateGet(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            getMethod(),
            "api/v1/tools/validate",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 替换 Validate 配置；在线端点的许可仅改变配置，真正外发仍需每次 validate 调用确认。
    #[tool(
        name = "capture_validate_update",
        description = "Replace response validation configuration.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn validateUpdate(
        &self,
        Parameters(arguments): Parameters<ProtocolConfigurationArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            putMethod(),
            "api/v1/tools/validate",
            Some(arguments.configuration),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 对响应正文执行校验；Skill 要求选择 W3C 前先取得用户明确上传确认。
    #[tool(
        name = "capture_validate_response",
        description = "Validate one recorded response body.",
        annotations(destructive_hint = false, open_world_hint = true)
    )]
    async fn validateResponse(
        &self,
        Parameters(arguments): Parameters<ValidateTransactionArguments>,
    ) -> CallToolResult {
        let transactionPath =
            match self.transactionPath(&arguments.transactionId, arguments.locale.as_deref()) {
                Ok(path) => path,
                Err(error) => return error,
            };
        let body = json!({"validatorId":arguments.validatorId,"onlineUploadConfirmed":arguments.onlineUploadConfirmed});
        self.executeRequest(
            postMethod(),
            &format!("{transactionPath}/validate"),
            Some(body),
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 列出本机用户数据目录中已发现插件的公开运行状态。
    #[tool(
        name = "capture_plugin_list",
        description = "List discovered plugins.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn pluginList(
        &self,
        Parameters(arguments): Parameters<LocaleArguments>,
    ) -> CallToolResult {
        self.executeRequest(
            getMethod(),
            "api/v1/plugins",
            None,
            arguments.locale.as_deref(),
        )
        .await
    }

    /// 启用或禁用指定插件；禁用会按宿主生命周期关闭该插件已处理的活动连接。
    #[tool(
        name = "capture_plugin_set_enabled",
        description = "Enable or disable one plugin.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn pluginSetEnabled(
        &self,
        Parameters(arguments): Parameters<PluginEnabledArguments>,
    ) -> CallToolResult {
        let encodedId = utf8_percent_encode(&arguments.pluginId, pathSegmentEncodeSet);
        self.executeRequest(
            putMethod(),
            &format!("api/v1/plugins/{encodedId}/enabled"),
            Some(json!({"enabled": arguments.enabled})),
            arguments.locale.as_deref(),
        )
        .await
    }
}

#[tool_handler(router = self.toolRouter)]
impl ServerHandler for ControlMcpServer {
    /// 发布工具能力与按启动 locale 渲染的服务说明；不声明权限或授权能力。
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("capture", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                self.catalog
                    .message(self.defaultLocale, "mcp.server.instructions"),
            )
    }
}

/// 用启动 locale 替换宏提供的英文回退描述；tool 名称与 schema 保持稳定。
fn localizeToolDescriptions(
    toolRouter: &mut ToolRouter<ControlMcpServer>,
    catalog: &MessageCatalog,
    locale: &str,
) {
    let descriptionKeys: HashMap<&str, &str> = toolDescriptionKeys.into_iter().collect();
    for (toolName, route) in &mut toolRouter.map {
        if let Some(messageKey) = descriptionKeys.get(toolName.as_ref()) {
            route.attr.description = Some(Cow::Owned(catalog.message(locale, messageKey)));
        }
    }
}
